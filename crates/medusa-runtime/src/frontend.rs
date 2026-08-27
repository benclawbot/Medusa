//! Canonical frontend event delivery over the durable session journal.
//!
//! Runtime workers may emit process-local wakeups and presentation hints, but user-facing
//! frontends consume the versioned protocol projected from committed journal events. This keeps
//! replay, ordering, verification, and terminal state identical across CLI and remote clients.

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, atomic::Ordering, mpsc},
    thread,
    time::Duration,
};

use medusa_agent::session_browser::{load_session, replay_events};
use medusa_protocol::{
    Actor, EventPayload, SessionAction, SessionActionDeliveryPolicy, SessionActionKind,
    SessionActionLifecycle, SessionActionWakePolicy,
    frontend::{FrontendEventEnvelope, FrontendKind, project_event},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::{
    QueuedFollowup, RuntimeCommand, RuntimeController, RuntimeError, RuntimeEvent, lock_submission,
    mark_idle, record_controller_event,
    prompt::PromptDraft,
};

/// Request to append one operator action to the authoritative session journal.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionActionRequest {
    pub idempotency_key: String,
    pub source: String,
    pub target_session_id: String,
    pub expected_session_revision: u64,
    pub kind: SessionActionKind,
    pub delivery_policy: SessionActionDeliveryPolicy,
    pub wake_policy: SessionActionWakePolicy,
    pub payload: Value,
}

impl SessionActionRequest {
    fn action_id(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(self.target_session_id.as_bytes());
        digest.update([0]);
        digest.update(self.idempotency_key.as_bytes());
        format!("action-{}", hex::encode(digest.finalize()))
    }

    fn into_action(self) -> SessionAction {
        let action_id = self.action_id();
        SessionAction {
            action_id,
            idempotency_key: self.idempotency_key,
            source: self.source,
            target_session_id: self.target_session_id,
            expected_session_revision: self.expected_session_revision,
            kind: self.kind,
            delivery_policy: self.delivery_policy,
            wake_policy: self.wake_policy,
            payload: self.payload,
        }
    }
}

/// Durable receipt/projection for one session action.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SessionActionView {
    pub action: SessionAction,
    pub lifecycle: SessionActionLifecycle,
    pub accepted_sequence: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub accepted_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub delivered_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub completed_at: Option<OffsetDateTime>,
    pub transcript_event_sequence: Option<u64>,
    pub terminal_evidence: Option<Value>,
}

/// Cross-frontend materialization of all action state from the canonical event stream.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct SessionActionSnapshot {
    pub session_id: String,
    pub revision: u64,
    pub queued_count: usize,
    pub active_action_id: Option<String>,
    pub actions: Vec<SessionActionView>,
}

/// Result of idempotent admission.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SessionActionAdmission {
    pub action: SessionActionView,
    pub coalesced: bool,
}

/// Reduces the canonical journal into the action queue/lifecycle projection.
pub fn session_action_snapshot(
    repo: &std::path::Path,
    session_id: &str,
) -> Result<SessionActionSnapshot, RuntimeError> {
    let session = load_session(repo, session_id).map_err(RuntimeError::agent)?;
    let revision = session.events.last().map_or(0, |event| event.sequence);
    let mut actions = Vec::<SessionActionView>::new();
    for event in &session.events {
        match &event.payload {
            EventPayload::SessionActionAccepted { action } => {
                action.validate().map_err(RuntimeError::agent)?;
                if action.target_session_id != session_id {
                    return Err(RuntimeError::agent(
                        "session action journal entry targets a different session",
                    ));
                }
                if action.kind == SessionActionKind::ReplaceFollowUp {
                    let replaced_action_id = replacement_target(action)?;
                    let replaced = actions
                        .iter_mut()
                        .find(|candidate| candidate.action.action_id == replaced_action_id)
                        .ok_or_else(|| RuntimeError::agent("replacement action targets no admission"))?;
                    if replaced.lifecycle != SessionActionLifecycle::Queued
                        || !matches!(
                            replaced.action.kind,
                            SessionActionKind::FollowUp | SessionActionKind::ReplaceFollowUp
                        )
                    {
                        return Err(RuntimeError::agent(
                            "replacement action targets a non-queued follow-up",
                        ));
                    }
                    replaced.lifecycle = SessionActionLifecycle::Cancelled;
                    replaced.completed_at = Some(event.timestamp);
                    replaced.terminal_evidence = Some(serde_json::json!({
                        "reason": "superseded",
                        "superseded_by": action.action_id,
                    }));
                }
                if actions.iter().any(|candidate| {
                    candidate.action.action_id == action.action_id
                        || candidate.action.idempotency_key == action.idempotency_key
                }) {
                    return Err(RuntimeError::agent(
                        "session action journal contains duplicate admission identity",
                    ));
                }
                actions.push(SessionActionView {
                    action: action.clone(),
                    lifecycle: SessionActionLifecycle::Queued,
                    accepted_sequence: event.sequence,
                    accepted_at: event.timestamp,
                    delivered_at: None,
                    completed_at: None,
                    transcript_event_sequence: None,
                    terminal_evidence: None,
                });
            }
            EventPayload::SessionActionRejected {
                action,
                authoritative_revision,
                reason,
            } => {
                action.validate().map_err(RuntimeError::agent)?;
                if action.target_session_id != session_id {
                    return Err(RuntimeError::agent(
                        "rejected session action targets a different session",
                    ));
                }
                if actions.iter().any(|candidate| {
                    candidate.action.action_id == action.action_id
                        || candidate.action.idempotency_key == action.idempotency_key
                }) {
                    return Err(RuntimeError::agent(
                        "session action journal contains duplicate admission identity",
                    ));
                }
                actions.push(SessionActionView {
                    action: action.clone(),
                    lifecycle: SessionActionLifecycle::Failed,
                    accepted_sequence: event.sequence,
                    accepted_at: event.timestamp,
                    delivered_at: None,
                    completed_at: Some(event.timestamp),
                    transcript_event_sequence: None,
                    terminal_evidence: Some(serde_json::json!({
                        "reason": reason,
                        "authoritative_revision": authoritative_revision,
                    })),
                });
            }
            EventPayload::SessionActionLifecycleChanged {
                action_id,
                from,
                to,
                evidence,
            } => {
                let action = actions
                    .iter_mut()
                    .find(|candidate| candidate.action.action_id == *action_id)
                    .ok_or_else(|| RuntimeError::agent("session action transition has no admission"))?;
                if action.lifecycle != *from || !from.can_transition_to(*to) {
                    return Err(RuntimeError::agent(format!(
                        "invalid session action transition for {action_id}: {:?} -> {:?}",
                        action.lifecycle, to
                    )));
                }
                action.lifecycle = *to;
                if *to == SessionActionLifecycle::Running {
                    action.delivered_at = Some(event.timestamp);
                }
                if to.terminal() {
                    action.completed_at = Some(event.timestamp);
                    action.terminal_evidence.clone_from(evidence);
                }
            }
            EventPayload::SessionActionTranscriptLinked {
                action_id,
                transcript_event_sequence,
            } => {
                let action = actions
                    .iter_mut()
                    .find(|candidate| candidate.action.action_id == *action_id)
                    .ok_or_else(|| RuntimeError::agent("session action transcript link has no admission"))?;
                if action.transcript_event_sequence.replace(*transcript_event_sequence).is_some() {
                    return Err(RuntimeError::agent(
                        "session action was linked to authoritative context more than once",
                    ));
                }
            }
            _ => {}
        }
    }
    let queued_count = actions
        .iter()
        .filter(|action| action.lifecycle == SessionActionLifecycle::Queued)
        .count();
    let active_action_id = actions
        .iter()
        .find(|action| {
            !action.lifecycle.terminal() && action.lifecycle != SessionActionLifecycle::Queued
        })
        .map(|action| action.action.action_id.clone());
    Ok(SessionActionSnapshot {
        session_id: session_id.to_owned(),
        revision,
        queued_count,
        active_action_id,
        actions,
    })
}

impl RuntimeController {
    /// Admits one action through the existing session journal and runtime authority.
    pub fn submit_session_action(
        &self,
        request: SessionActionRequest,
    ) -> Result<SessionActionAdmission, RuntimeError> {
        validate_action_request(&request)?;
        let action = request.into_action();
        let submission = lock_submission(&self.submission);
        if submission.active_session_id.as_deref() != Some(action.target_session_id.as_str()) {
            return Err(RuntimeError::InvalidCommand(
                "session action target is not the controller's active session".to_owned(),
            ));
        }
        let snapshot = session_action_snapshot(&self.repo, &action.target_session_id)?;
        if let Some(existing) = snapshot
            .actions
            .iter()
            .find(|candidate| candidate.action.idempotency_key == action.idempotency_key)
        {
            if existing.action != action {
                return Err(RuntimeError::InvalidCommand(
                    "session action idempotency key was reused for a different action".to_owned(),
                ));
            }
            return Ok(SessionActionAdmission {
                action: existing.clone(),
                coalesced: true,
            });
        }

        record_controller_event(
            &self.repo,
            &action.target_session_id,
            Actor::User,
            EventPayload::SessionActionAccepted {
                action: action.clone(),
            },
        )?;
        let admission = action_admission(&self.repo, &action, false)?;
        let busy = submission.busy;
        drop(submission);
        if admission.action.lifecycle.terminal() {
            return Ok(admission);
        }

        match action.kind {
            SessionActionKind::Cancel => self.deliver_cancel_action(&action)?,
            SessionActionKind::FollowUp | SessionActionKind::ReplaceFollowUp => {
                if action.wake_policy == SessionActionWakePolicy::ExternalResume {
                    // Admission is durable. Explicit session resume calls recover_session_actions.
                } else if busy {
                    self.spawn_when_idle_action(action.clone())?;
                } else {
                    self.deliver_idle_message_action(&action)?;
                }
            }
            SessionActionKind::Steer => self.deliver_safe_boundary_message_action(&action)?,
            SessionActionKind::GoalAdjustment => {
                if busy {
                    match action.delivery_policy {
                        SessionActionDeliveryPolicy::NextSafeTurnBoundary => {
                            self.deliver_safe_boundary_message_action(&action)?;
                        }
                        SessionActionDeliveryPolicy::WhenIdle => {
                            self.spawn_when_idle_action(action.clone())?;
                        }
                    }
                } else {
                    self.deliver_idle_goal_action(&action)?;
                }
            }
        }

        action_admission(&self.repo, &action, false)
    }

    /// Returns the canonical action projection for the active durable session.
    pub fn session_actions(&self) -> Result<Option<SessionActionSnapshot>, RuntimeError> {
        let Some(session_id) = self.active_session_id() else {
            return Ok(None);
        };
        session_action_snapshot(&self.repo, &session_id).map(Some)
    }

    /// Restores queued or interrupted action delivery after a controller/session resume.
    /// Recovery only moves forward from the journaled lifecycle; it never rewinds committing.
    pub fn recover_session_actions(&self) -> Result<(), RuntimeError> {
        let Some(session_id) = self.active_session_id() else {
            return Ok(());
        };
        let snapshot = session_action_snapshot(&self.repo, &session_id)?;
        for view in snapshot.actions {
            if view.lifecycle.terminal() {
                continue;
            }
            match view.action.kind {
                SessionActionKind::Cancel if view.lifecycle == SessionActionLifecycle::Running => {
                    // The original runtime died while cancellation was in flight. Its process
                    // containment is recovered separately; the action receipt must not pretend a
                    // cancellation completed without the canonical cancellation event.
                }
                SessionActionKind::Cancel => {}
                SessionActionKind::FollowUp | SessionActionKind::ReplaceFollowUp => {
                    self.spawn_when_idle_action(view.action)?;
                }
                SessionActionKind::Steer | SessionActionKind::GoalAdjustment
                    if view.lifecycle == SessionActionLifecycle::Committing
                        || view.lifecycle == SessionActionLifecycle::Running =>
                {
                    if !reconcile_interrupted_delivery(&self.repo, &view.action)? {
                        self.consume_restored_safe_boundary_entry(&view.action);
                        self.spawn_when_idle_action(view.action)?;
                    }
                }
                SessionActionKind::Steer | SessionActionKind::GoalAdjustment => {
                    if self.is_busy()
                        && view.action.delivery_policy
                            == SessionActionDeliveryPolicy::NextSafeTurnBoundary
                    {
                        self.ensure_safe_boundary_queue(&view.action)?;
                    } else if view.action.kind == SessionActionKind::GoalAdjustment
                        && !self.is_busy()
                    {
                        self.consume_restored_safe_boundary_entry(&view.action);
                        self.deliver_idle_goal_action(&view.action)?;
                    } else {
                        if !self.is_busy() {
                            self.consume_restored_safe_boundary_entry(&view.action);
                        }
                        self.spawn_when_idle_action(view.action)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn deliver_safe_boundary_message_action(
        &self,
        action: &SessionAction,
    ) -> Result<(), RuntimeError> {
        if self.is_busy() {
            return self.ensure_safe_boundary_queue(action);
        }
        self.deliver_idle_message_action(action)
    }

    fn ensure_safe_boundary_queue(&self, action: &SessionAction) -> Result<(), RuntimeError> {
        let draft = action_draft(action)?;
        let session = load_session(&self.repo, &action.target_session_id).map_err(RuntimeError::agent)?;
        let already_queued = session.events.iter().rev().any(|event| match &event.payload {
            EventPayload::UserFollowupQueued { command_id, .. } => command_id == &action.action_id,
            EventPayload::UserFollowupDequeued { command_id, .. } => {
                if command_id == &action.action_id {
                    return false;
                }
                false
            }
            _ => false,
        });
        if !already_queued {
            record_controller_event(
                &self.repo,
                &action.target_session_id,
                Actor::User,
                EventPayload::UserFollowupQueued {
                    command_id: action.action_id.clone(),
                    prompt: serde_json::to_value(&draft).map_err(RuntimeError::agent)?,
                },
            )?;
        }
        let mut submission = lock_submission(&self.submission);
        if !submission
            .followups
            .iter()
            .any(|queued| queued.command_id == action.action_id)
        {
            submission.followups.push_back(QueuedFollowup {
                command_id: action.action_id.clone(),
                draft,
                durably_recorded: true,
            });
        }
        Ok(())
    }

    fn consume_restored_safe_boundary_entry(&self, action: &SessionAction) {
        if action.delivery_policy != SessionActionDeliveryPolicy::NextSafeTurnBoundary {
            return;
        }
        let mut submission = lock_submission(&self.submission);
        if let Some(index) = submission
            .followups
            .iter()
            .position(|queued| queued.command_id == action.action_id)
        {
            submission.followups.remove(index);
        }
    }

    fn deliver_idle_message_action(&self, action: &SessionAction) -> Result<(), RuntimeError> {
        dispatch_when_idle(
            self.repo.clone(),
            self.commands.clone(),
            Arc::clone(&self.cancel),
            Arc::clone(&self.submission),
            self.event_sender.clone(),
            action.clone(),
        )
    }

    fn spawn_when_idle_action(&self, action: SessionAction) -> Result<(), RuntimeError> {
        let repo = self.repo.clone();
        let commands = self.commands.clone();
        let cancel = Arc::clone(&self.cancel);
        let submission = Arc::clone(&self.submission);
        let event_sender = self.event_sender.clone();
        thread::Builder::new()
            .name(format!("medusa-session-action-{}", short_action_id(&action.action_id)))
            .spawn(move || loop {
                match session_action_snapshot(&repo, &action.target_session_id) {
                    Ok(snapshot) => {
                        let Some(view) = snapshot
                            .actions
                            .iter()
                            .find(|view| view.action.action_id == action.action_id)
                        else {
                            return;
                        };
                        if view.lifecycle.terminal() {
                            return;
                        }
                        if view.lifecycle == SessionActionLifecycle::Committing
                            || view.lifecycle == SessionActionLifecycle::Running
                        {
                            match reconcile_interrupted_delivery(&repo, &action) {
                                Ok(true) => return,
                                Ok(false) => {}
                                Err(error) => {
                                    let _ = event_sender.send(RuntimeEvent::Notice {
                                        title: "Session action recovery failed".to_owned(),
                                        details: vec![error.to_string()],
                                    });
                                    return;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        let _ = event_sender.send(RuntimeEvent::Notice {
                            title: "Session action projection failed".to_owned(),
                            details: vec![error.to_string()],
                        });
                        return;
                    }
                }
                if !lock_submission(&submission).busy {
                    let result = if action.kind == SessionActionKind::GoalAdjustment {
                        deliver_goal_when_idle(&repo, &submission, &action)
                    } else {
                        dispatch_when_idle(
                            repo.clone(),
                            commands.clone(),
                            Arc::clone(&cancel),
                            Arc::clone(&submission),
                            event_sender.clone(),
                            action.clone(),
                        )
                    };
                    match result {
                        Ok(()) => return,
                        Err(RuntimeError::Busy) => {
                            thread::sleep(Duration::from_millis(10));
                            continue;
                        }
                        Err(error) => {
                            let _ = event_sender.send(RuntimeEvent::Notice {
                                title: "Session action delivery failed".to_owned(),
                                details: vec![error.to_string()],
                            });
                            return;
                        }
                    }
                }
                thread::sleep(Duration::from_millis(10));
            })
            .map(|_| ())
            .map_err(|error| RuntimeError::agent(format!("failed to spawn session action delivery: {error}")))
    }

    fn deliver_idle_goal_action(&self, action: &SessionAction) -> Result<(), RuntimeError> {
        deliver_goal_when_idle(&self.repo, &self.submission, action)
    }

    fn deliver_cancel_action(&self, action: &SessionAction) -> Result<(), RuntimeError> {
        advance_to_committing(&self.repo, action, None)?;
        let busy = lock_submission(&self.submission).busy;
        record_controller_event(
            &self.repo,
            &action.target_session_id,
            Actor::User,
            EventPayload::CancellationRequested {
                source: format!("session_action:{}", action.action_id),
            },
        )?;
        transition_action(
            &self.repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Committing,
            SessionActionLifecycle::Running,
            Some(serde_json::json!({"runtime_was_busy": busy})),
        )?;
        if busy {
            self.cancel.store(true, Ordering::SeqCst);
        } else {
            transition_action(
                &self.repo,
                &action.target_session_id,
                &action.action_id,
                SessionActionLifecycle::Running,
                SessionActionLifecycle::Completed,
                Some(serde_json::json!({"delivery": "no_active_work"})),
            )?;
        }
        Ok(())
    }
}

fn action_admission(
    repo: &std::path::Path,
    action: &SessionAction,
    coalesced: bool,
) -> Result<SessionActionAdmission, RuntimeError> {
    let snapshot = session_action_snapshot(repo, &action.target_session_id)?;
    let view = snapshot
        .actions
        .into_iter()
        .find(|candidate| candidate.action.action_id == action.action_id)
        .ok_or_else(|| RuntimeError::agent("session action disappeared from canonical replay"))?;
    Ok(SessionActionAdmission {
        action: view,
        coalesced,
    })
}

fn dispatch_when_idle(
    repo: PathBuf,
    commands: std::sync::mpsc::Sender<RuntimeCommand>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    submission: Arc<std::sync::Mutex<crate::SubmissionState>>,
    event_sender: std::sync::mpsc::Sender<RuntimeEvent>,
    action: SessionAction,
) -> Result<(), RuntimeError> {
    {
        let mut state = lock_submission(&submission);
        if state.busy {
            return Err(RuntimeError::Busy);
        }
        if state.active_session_id.as_deref() != Some(action.target_session_id.as_str()) {
            return Err(RuntimeError::InvalidCommand(
                "session action target changed before delivery".to_owned(),
            ));
        }
        state.busy = true;
    }
    cancel.store(false, Ordering::SeqCst);

    let result = (|| {
        if reconcile_interrupted_delivery(&repo, &action)? {
            return Ok(());
        }
        let expected_transcript_sequence = advance_to_committing(&repo, &action, None)?;
        if let Some(sequence) = find_committed_delivery(
            &repo,
            &action,
            expected_transcript_sequence,
        )? {
            finish_message_delivery(&repo, &action, sequence)?;
            return Ok(());
        }

        let draft = action_draft(&action)?;
        let (accepted_tx, accepted_rx) = mpsc::channel();
        commands
            .send(RuntimeCommand::Submit {
                draft,
                accepted: accepted_tx,
            })
            .map_err(|_| RuntimeError::WorkerStopped)?;
        match accepted_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return Err(RuntimeError::agent(format!(
                    "runtime failed before a durable session accepted the action: {error}"
                )));
            }
            Err(_) => {
                return Err(RuntimeError::agent(
                    "runtime worker stopped before a durable session accepted the action",
                ));
            }
        }
        let sequence = find_committed_delivery(
            &repo,
            &action,
            expected_transcript_sequence,
        )?
        .ok_or_else(|| {
            RuntimeError::agent("session action submission produced no provable durable user message")
        })?;
        finish_message_delivery(&repo, &action, sequence)
    })();

    if let Err(error) = &result {
        let lifecycle = action_view(&repo, &action)?.lifecycle;
        if lifecycle == SessionActionLifecycle::Committing {
            let _ = transition_action(
                &repo,
                &action.target_session_id,
                &action.action_id,
                SessionActionLifecycle::Committing,
                SessionActionLifecycle::Failed,
                Some(serde_json::json!({"reason": error.to_string()})),
            );
        }
        let _ = event_sender.send(RuntimeEvent::Notice {
            title: "Session action failed".to_owned(),
            details: vec![error.to_string()],
        });
        mark_idle(&submission, false);
    }
    result
}

fn deliver_goal_when_idle(
    repo: &std::path::Path,
    submission: &std::sync::Mutex<crate::SubmissionState>,
    action: &SessionAction,
) -> Result<(), RuntimeError> {
    if lock_submission(submission).busy {
        return Err(RuntimeError::Busy);
    }
    advance_to_committing(repo, action, None)?;
    let objective = action_objective(action)?.to_owned();
    let mut session = load_session(repo, &action.target_session_id).map_err(RuntimeError::agent)?;
    medusa_agent::update_session_objective(&mut session, objective).map_err(RuntimeError::agent)?;
    let linked_sequence = session.events.last().map_or(0, |event| event.sequence);
    record_controller_event(
        repo,
        &action.target_session_id,
        Actor::Coordinator,
        EventPayload::SessionActionTranscriptLinked {
            action_id: action.action_id.clone(),
            transcript_event_sequence: linked_sequence,
        },
    )?;
    transition_action(
        repo,
        &action.target_session_id,
        &action.action_id,
        SessionActionLifecycle::Committing,
        SessionActionLifecycle::Running,
        Some(serde_json::json!({"goal_event_sequence": linked_sequence})),
    )?;
    transition_action(
        repo,
        &action.target_session_id,
        &action.action_id,
        SessionActionLifecycle::Running,
        SessionActionLifecycle::Completed,
        Some(serde_json::json!({"delivery": "authoritative_goal"})),
    )
}

fn advance_to_committing(
    repo: &std::path::Path,
    action: &SessionAction,
    expected_override: Option<u64>,
) -> Result<Option<u64>, RuntimeError> {
    let mut lifecycle = action_view(repo, action)?.lifecycle;
    if lifecycle == SessionActionLifecycle::Queued {
        transition_action(
            repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Queued,
            SessionActionLifecycle::Selected,
            None,
        )?;
        lifecycle = SessionActionLifecycle::Selected;
    }
    if lifecycle == SessionActionLifecycle::Selected {
        transition_action(
            repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Selected,
            SessionActionLifecycle::Preparing,
            None,
        )?;
        lifecycle = SessionActionLifecycle::Preparing;
    }
    if lifecycle == SessionActionLifecycle::Preparing {
        let revision = session_action_snapshot(repo, &action.target_session_id)?.revision;
        let expected = expected_override.unwrap_or_else(|| revision.saturating_add(2));
        transition_action(
            repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Preparing,
            SessionActionLifecycle::Committing,
            Some(serde_json::json!({
                "expected_transcript_event_sequence": expected,
            })),
        )?;
        return Ok(Some(expected));
    }
    if lifecycle == SessionActionLifecycle::Committing {
        return committing_expected_sequence(repo, action).map(Some);
    }
    Ok(None)
}

fn committing_expected_sequence(
    repo: &std::path::Path,
    action: &SessionAction,
) -> Result<u64, RuntimeError> {
    let session = load_session(repo, &action.target_session_id).map_err(RuntimeError::agent)?;
    session
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            EventPayload::SessionActionLifecycleChanged {
                action_id,
                to: SessionActionLifecycle::Committing,
                evidence: Some(evidence),
                ..
            } if action_id == &action.action_id => evidence
                .get("expected_transcript_event_sequence")
                .and_then(Value::as_u64),
            _ => None,
        })
        .ok_or_else(|| RuntimeError::agent("committing action lacks dispatch proof metadata"))
}

fn reconcile_interrupted_delivery(
    repo: &std::path::Path,
    action: &SessionAction,
) -> Result<bool, RuntimeError> {
    let view = action_view(repo, action)?;
    match view.lifecycle {
        SessionActionLifecycle::Completed
        | SessionActionLifecycle::Failed
        | SessionActionLifecycle::Cancelled => Ok(true),
        SessionActionLifecycle::Running => {
            if action.kind == SessionActionKind::Cancel {
                return Ok(true);
            }
            let sequence = view.transcript_event_sequence.ok_or_else(|| {
                RuntimeError::agent("running action has no authoritative transcript linkage")
            })?;
            transition_action(
                repo,
                &action.target_session_id,
                &action.action_id,
                SessionActionLifecycle::Running,
                SessionActionLifecycle::Completed,
                Some(serde_json::json!({
                    "delivery": "recovered_authoritative_transcript",
                    "transcript_event_sequence": sequence,
                })),
            )?;
            Ok(true)
        }
        SessionActionLifecycle::Committing => {
            if let Some(sequence) = view.transcript_event_sequence.or(find_committed_delivery(
                repo,
                action,
                Some(committing_expected_sequence(repo, action)?),
            )?) {
                finish_message_delivery(repo, action, sequence)?;
                Ok(true)
            } else {
                Ok(false)
            }
        }
        SessionActionLifecycle::Queued
        | SessionActionLifecycle::Selected
        | SessionActionLifecycle::Preparing => Ok(false),
    }
}

fn find_committed_delivery(
    repo: &std::path::Path,
    action: &SessionAction,
    expected_sequence: Option<u64>,
) -> Result<Option<u64>, RuntimeError> {
    let session = load_session(repo, &action.target_session_id).map_err(RuntimeError::agent)?;
    if let Some(sequence) = session.events.iter().find_map(|event| match &event.payload {
        EventPayload::UserFollowupDequeued { command_id, .. }
            if command_id == &action.action_id => Some(event.sequence),
        _ => None,
    }) {
        return Ok(Some(sequence));
    }
    let Some(expected) = expected_sequence else {
        return Ok(None);
    };
    let event = session.events.iter().find(|event| event.sequence == expected);
    let Some(event) = event else {
        return Ok(None);
    };
    match &event.payload {
        EventPayload::UserPromptReceived { text }
            if action_delivery_text(action).is_ok_and(|needle| text.contains(needle)) =>
        {
            Ok(Some(event.sequence))
        }
        _ => Err(RuntimeError::agent(
            "committing action dispatch sequence is occupied by unrelated authoritative state",
        )),
    }
}

fn finish_message_delivery(
    repo: &std::path::Path,
    action: &SessionAction,
    transcript_event_sequence: u64,
) -> Result<(), RuntimeError> {
    let view = action_view(repo, action)?;
    if view.transcript_event_sequence.is_none() {
        record_controller_event(
            repo,
            &action.target_session_id,
            Actor::Coordinator,
            EventPayload::SessionActionTranscriptLinked {
                action_id: action.action_id.clone(),
                transcript_event_sequence,
            },
        )?;
    }
    let lifecycle = action_view(repo, action)?.lifecycle;
    if lifecycle == SessionActionLifecycle::Committing {
        transition_action(
            repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Committing,
            SessionActionLifecycle::Running,
            Some(serde_json::json!({"transcript_event_sequence": transcript_event_sequence})),
        )?;
    }
    if action_view(repo, action)?.lifecycle == SessionActionLifecycle::Running {
        transition_action(
            repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Running,
            SessionActionLifecycle::Completed,
            Some(serde_json::json!({
                "delivery": "authoritative_transcript",
                "transcript_event_sequence": transcript_event_sequence,
            })),
        )?;
    }
    Ok(())
}

fn action_view(
    repo: &std::path::Path,
    action: &SessionAction,
) -> Result<SessionActionView, RuntimeError> {
    session_action_snapshot(repo, &action.target_session_id)?
        .actions
        .into_iter()
        .find(|view| view.action.action_id == action.action_id)
        .ok_or_else(|| RuntimeError::agent("session action disappeared from canonical replay"))
}

fn validate_action_request(request: &SessionActionRequest) -> Result<(), RuntimeError> {
    if request.idempotency_key.trim().is_empty()
        || request.source.trim().is_empty()
        || request.target_session_id.trim().is_empty()
    {
        return Err(RuntimeError::InvalidCommand(
            "session action identity/source/target cannot be empty".to_owned(),
        ));
    }
    match request.kind {
        SessionActionKind::Steer
            if request.delivery_policy != SessionActionDeliveryPolicy::NextSafeTurnBoundary =>
        {
            Err(RuntimeError::InvalidCommand(
                "steering must use the next-safe-turn-boundary delivery policy".to_owned(),
            ))
        }
        SessionActionKind::FollowUp | SessionActionKind::ReplaceFollowUp
            if request.delivery_policy != SessionActionDeliveryPolicy::WhenIdle =>
        {
            Err(RuntimeError::InvalidCommand(
                "follow-up actions must use the when-idle delivery policy".to_owned(),
            ))
        }
        SessionActionKind::Cancel
            if request.payload != Value::Null && request.payload != serde_json::json!({}) =>
        {
            Err(RuntimeError::InvalidCommand(
                "cancel actions do not accept a free-form payload".to_owned(),
            ))
        }
        SessionActionKind::Steer | SessionActionKind::FollowUp => {
            action_text_payload(&request.payload).map(|_| ())
        }
        SessionActionKind::ReplaceFollowUp => {
            action_text_payload(&request.payload)?;
            replacement_target_payload(&request.payload).map(|_| ())
        }
        SessionActionKind::GoalAdjustment => action_objective_payload(&request.payload).map(|_| ()),
        SessionActionKind::Cancel => Ok(()),
    }
}

fn action_draft(action: &SessionAction) -> Result<PromptDraft, RuntimeError> {
    let text = action_delivery_text(action)?.to_owned();
    Ok(PromptDraft {
        text,
        attachments: Vec::new(),
        revision: action.expected_session_revision,
    })
}

fn action_delivery_text(action: &SessionAction) -> Result<&str, RuntimeError> {
    match action.kind {
        SessionActionKind::Steer
        | SessionActionKind::FollowUp
        | SessionActionKind::ReplaceFollowUp => action_text_payload(&action.payload),
        SessionActionKind::GoalAdjustment => action_objective(action),
        SessionActionKind::Cancel => Err(RuntimeError::InvalidCommand(
            "cancel action cannot become a prompt".to_owned(),
        )),
    }
}

fn action_text_payload(payload: &Value) -> Result<&str, RuntimeError> {
    payload
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| RuntimeError::InvalidCommand("session action requires non-empty text".to_owned()))
}

fn replacement_target(action: &SessionAction) -> Result<&str, RuntimeError> {
    replacement_target_payload(&action.payload)
}

fn replacement_target_payload(payload: &Value) -> Result<&str, RuntimeError> {
    payload
        .get("replaces_action_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|action_id| !action_id.is_empty())
        .ok_or_else(|| {
            RuntimeError::InvalidCommand(
                "replacement follow-up requires a non-empty replaces_action_id".to_owned(),
            )
        })
}

fn action_objective(action: &SessionAction) -> Result<&str, RuntimeError> {
    action_objective_payload(&action.payload)
}

fn action_objective_payload(payload: &Value) -> Result<&str, RuntimeError> {
    payload
        .get("objective")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| RuntimeError::InvalidCommand("goal adjustment requires a non-empty objective".to_owned()))
}

fn transition_action(
    repo: &std::path::Path,
    session_id: &str,
    action_id: &str,
    from: SessionActionLifecycle,
    to: SessionActionLifecycle,
    evidence: Option<Value>,
) -> Result<(), RuntimeError> {
    if !from.can_transition_to(to) {
        return Err(RuntimeError::agent("invalid session action lifecycle transition"));
    }
    record_controller_event(
        repo,
        session_id,
        Actor::Coordinator,
        EventPayload::SessionActionLifecycleChanged {
            action_id: action_id.to_owned(),
            from,
            to,
            evidence,
        },
    )
}

fn short_action_id(action_id: &str) -> &str {
    action_id
        .strip_prefix("action-")
        .unwrap_or(action_id)
        .get(..12)
        .unwrap_or(action_id)
}

/// Cursor-bearing projection of one authoritative runtime session for one frontend kind.
pub struct CanonicalFrontendEventStream {
    repo: PathBuf,
    frontend: FrontendKind,
    session_id: Option<String>,
    journal_cursor: u64,
    pending: VecDeque<FrontendEventEnvelope>,
}

impl CanonicalFrontendEventStream {
    #[must_use]
    pub fn new(repo: PathBuf, frontend: FrontendKind) -> Self {
        Self {
            repo,
            frontend,
            session_id: None,
            journal_cursor: 0,
            pending: VecDeque::new(),
        }
    }

    /// Resumes presentation after an acknowledged canonical journal cursor.
    pub fn resume(&mut self, session_id: impl Into<String>, after_cursor: u64) {
        self.session_id = Some(session_id.into());
        self.journal_cursor = after_cursor;
        self.pending.clear();
    }

    /// Returns the next shared frontend event, replaying committed journal state as needed.
    pub fn try_event(
        &mut self,
        session_id: &str,
    ) -> Result<Option<FrontendEventEnvelope>, RuntimeError> {
        if self.session_id.as_deref() != Some(session_id) {
            self.resume(session_id.to_owned(), 0);
        }
        if let Some(event) = self.pending.pop_front() {
            return Ok(Some(event));
        }

        let events = replay_events(&self.repo, session_id, self.journal_cursor)
            .map_err(RuntimeError::agent)?;
        for event in events {
            if event.sequence <= self.journal_cursor {
                return Err(RuntimeError::InvalidCommand(format!(
                    "frontend journal sequence {} did not advance past cursor {}",
                    event.sequence, self.journal_cursor
                )));
            }
            self.journal_cursor = event.sequence;
            if let Some(projected) = project_event(&event, event.sequence, self.frontend) {
                self.pending.push_back(projected);
            }
        }
        Ok(self.pending.pop_front())
    }

    /// Returns the last scanned canonical journal sequence, including non-presentable events.
    #[must_use]
    pub const fn journal_cursor(&self) -> u64 {
        self.journal_cursor
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use medusa_agent::{AgentSession, record_session_event};
    use medusa_core::SessionId;
    use medusa_protocol::{
        Actor, EventPayload, SessionAction, SessionActionDeliveryPolicy, SessionActionKind,
        SessionActionLifecycle, SessionActionWakePolicy,
        frontend::{FrontendEvent, FrontendKind},
    };
    use serde_json::{Value, json};
    use tempfile::tempdir;
    use time::OffsetDateTime;

    use super::{CanonicalFrontendEventStream, session_action_snapshot};

    fn durable_session(repo: &Path) -> AgentSession {
        AgentSession {
            id: SessionId::new(),
            objective: "canonical frontend replay".to_owned(),
            repo: repo.to_path_buf(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            completed: false,
            turn: 0,
            plan: Vec::new(),
            pending_question: None,
            messages: Vec::new(),
            events: Vec::new(),
            evidence: Vec::new(),
            tool_artifacts: Vec::new(),
            world_model: None,
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
            codex_thread_id: None,
        }
    }

    fn action(
        session_id: &str,
        id: &str,
        expected_session_revision: u64,
        kind: SessionActionKind,
        payload: serde_json::Value,
    ) -> SessionAction {
        SessionAction {
            action_id: format!("action-{id}"),
            idempotency_key: format!("idem-{id}"),
            source: "test".to_owned(),
            target_session_id: session_id.to_owned(),
            expected_session_revision,
            kind,
            delivery_policy: if kind == SessionActionKind::Steer {
                SessionActionDeliveryPolicy::NextSafeTurnBoundary
            } else {
                SessionActionDeliveryPolicy::WhenIdle
            },
            wake_policy: SessionActionWakePolicy::OnBoundary,
            payload,
        }
    }

    #[test]
    fn stream_advances_the_canonical_cursor_through_non_presentable_events() {
        let directory = tempdir().expect("temporary repository");
        let mut session = durable_session(directory.path());
        let objective = session.objective.clone();
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::SessionCreated { objective },
        )
        .expect("persist session creation");
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::AssistantMessageRecorded {
                message: json!({
                    "role": "user",
                    "content": [{"type": "text", "text": "not assistant-visible"}],
                }),
            },
        )
        .expect("persist non-presentable event");
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::RuntimeTurnFinished,
        )
        .expect("persist terminal event");

        let session_id = session.id.to_string();
        let mut stream = CanonicalFrontendEventStream::new(
            directory.path().to_path_buf(),
            FrontendKind::Headless,
        );
        let accepted = stream
            .try_event(&session_id)
            .expect("replay accepted event")
            .expect("accepted event");
        assert!(matches!(accepted.event, FrontendEvent::SubmissionAccepted));
        assert_eq!(accepted.cursor, 1);
        assert!(accepted.event_id.ends_with(":headless"));

        let finished = stream
            .try_event(&session_id)
            .expect("replay terminal event")
            .expect("terminal event");
        assert!(matches!(finished.event, FrontendEvent::TurnFinished));
        assert_eq!(finished.cursor, 3);
        assert_eq!(stream.journal_cursor(), 3);
        assert!(
            stream
                .try_event(&session_id)
                .expect("replay exhausted")
                .is_none()
        );
    }

    #[test]
    fn action_projection_rejects_silent_committing_rollback() {
        let directory = tempdir().expect("temporary repository");
        let mut session = durable_session(directory.path());
        let session_id = session.id.to_string();
        let action = action(
            &session_id,
            "1",
            0,
            SessionActionKind::Steer,
            json!({"text":"steer"}),
        );
        record_session_event(
            &mut session,
            Actor::User,
            EventPayload::SessionActionAccepted { action },
        )
        .expect("accept action");
        for (from, to) in [
            (SessionActionLifecycle::Queued, SessionActionLifecycle::Selected),
            (SessionActionLifecycle::Selected, SessionActionLifecycle::Preparing),
            (
                SessionActionLifecycle::Preparing,
                SessionActionLifecycle::Committing,
            ),
        ] {
            record_session_event(
                &mut session,
                Actor::Coordinator,
                EventPayload::SessionActionLifecycleChanged {
                    action_id: "action-1".to_owned(),
                    from,
                    to,
                    evidence: None,
                },
            )
            .expect("advance action");
        }
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::SessionActionLifecycleChanged {
                action_id: "action-1".to_owned(),
                from: SessionActionLifecycle::Committing,
                to: SessionActionLifecycle::Queued,
                evidence: None,
            },
        )
        .expect("journal corrupt transition for projection test");
        assert!(session_action_snapshot(directory.path(), &session_id).is_err());
    }

    #[test]
    fn replacement_supersedes_exactly_one_queued_followup() {
        let directory = tempdir().expect("temporary repository");
        let mut session = durable_session(directory.path());
        let session_id = session.id.to_string();
        let original = action(
            &session_id,
            "original",
            0,
            SessionActionKind::FollowUp,
            json!({"text":"original"}),
        );
        record_session_event(
            &mut session,
            Actor::User,
            EventPayload::SessionActionAccepted {
                action: original.clone(),
            },
        )
        .expect("original admission");
        let replacement = action(
            &session_id,
            "replacement",
            1,
            SessionActionKind::ReplaceFollowUp,
            json!({
                "text":"replacement",
                "replaces_action_id": original.action_id,
            }),
        );
        record_session_event(
            &mut session,
            Actor::User,
            EventPayload::SessionActionAccepted {
                action: replacement.clone(),
            },
        )
        .expect("replacement admission");

        let snapshot = session_action_snapshot(directory.path(), &session_id).expect("snapshot");
        assert_eq!(snapshot.queued_count, 1);
        let superseded = snapshot
            .actions
            .iter()
            .find(|view| view.action.action_id == original.action_id)
            .expect("original action");
        assert_eq!(superseded.lifecycle, SessionActionLifecycle::Cancelled);
        assert_eq!(
            superseded
                .terminal_evidence
                .as_ref()
                .and_then(|value| value.get("superseded_by"))
                .and_then(Value::as_str),
            Some(replacement.action_id.as_str())
        );
        assert_eq!(
            snapshot
                .actions
                .iter()
                .find(|view| view.action.action_id == replacement.action_id)
                .expect("replacement action")
                .lifecycle,
            SessionActionLifecycle::Queued
        );
    }

    #[test]
    fn stale_revision_is_audited_as_failed_action() {
        let directory = tempdir().expect("temporary repository");
        let mut session = durable_session(directory.path());
        let session_id = session.id.to_string();
        let first = action(
            &session_id,
            "first",
            0,
            SessionActionKind::FollowUp,
            json!({"text":"first"}),
        );
        record_session_event(
            &mut session,
            Actor::User,
            EventPayload::SessionActionAccepted { action: first },
        )
        .expect("first admission");
        let stale = action(
            &session_id,
            "stale",
            0,
            SessionActionKind::FollowUp,
            json!({"text":"stale"}),
        );
        record_session_event(
            &mut session,
            Actor::User,
            EventPayload::SessionActionAccepted {
                action: stale.clone(),
            },
        )
        .expect("stale attempt is journaled");

        let snapshot = session_action_snapshot(directory.path(), &session_id).expect("snapshot");
        let rejected = snapshot
            .actions
            .iter()
            .find(|view| view.action.action_id == stale.action_id)
            .expect("stale action");
        assert_eq!(rejected.lifecycle, SessionActionLifecycle::Failed);
        assert_eq!(
            rejected
                .terminal_evidence
                .as_ref()
                .and_then(|value| value.get("reason"))
                .and_then(Value::as_str),
            Some("stale_revision")
        );
    }
}
