//! Canonical frontend event delivery over the durable session journal.
//!
//! Runtime workers may emit process-local wakeups and presentation hints, but user-facing
//! frontends consume the versioned protocol projected from committed journal events. This keeps
//! replay, ordering, verification, and terminal state identical across CLI and remote clients.

use std::{collections::VecDeque, path::PathBuf, sync::atomic::Ordering};

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
    QueuedFollowup, RuntimeCommand, RuntimeController, RuntimeError, SubmitDisposition,
    lock_submission, record_controller_event,
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
        SessionAction {
            action_id: self.action_id(),
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
    ///
    /// The action itself is complete when its requested control/message delivery is durably applied;
    /// any model/tool work caused by that delivery remains governed by the normal runtime lifecycle.
    pub fn submit_session_action(
        &self,
        request: SessionActionRequest,
    ) -> Result<SessionActionAdmission, RuntimeError> {
        validate_action_request(&request)?;
        let action = request.into_action();
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
        if snapshot.revision != action.expected_session_revision {
            return Err(RuntimeError::InvalidCommand(format!(
                "stale session action revision: expected {}, authoritative revision is {}",
                action.expected_session_revision, snapshot.revision
            )));
        }
        if self.active_session_id().as_deref() != Some(action.target_session_id.as_str()) {
            return Err(RuntimeError::InvalidCommand(
                "session action target is not the controller's active session".to_owned(),
            ));
        }

        record_controller_event(
            &self.repo,
            &action.target_session_id,
            Actor::User,
            EventPayload::SessionActionAccepted {
                action: action.clone(),
            },
        )?;

        match action.kind {
            SessionActionKind::Cancel => self.deliver_cancel_action(&action)?,
            SessionActionKind::GoalAdjustment if !self.is_busy() => {
                self.deliver_idle_goal_action(&action)?;
            }
            SessionActionKind::Steer
            | SessionActionKind::FollowUp
            | SessionActionKind::GoalAdjustment => {
                self.deliver_message_action(&action)?;
            }
        }

        let snapshot = session_action_snapshot(&self.repo, &action.target_session_id)?;
        let view = snapshot
            .actions
            .into_iter()
            .find(|candidate| candidate.action.action_id == action.action_id)
            .ok_or_else(|| RuntimeError::agent("accepted session action disappeared from replay"))?;
        Ok(SessionActionAdmission {
            action: view,
            coalesced: false,
        })
    }

    /// Returns the canonical action projection for the active durable session.
    pub fn session_actions(&self) -> Result<Option<SessionActionSnapshot>, RuntimeError> {
        let Some(session_id) = self.active_session_id() else {
            return Ok(None);
        };
        session_action_snapshot(&self.repo, &session_id).map(Some)
    }

    fn deliver_message_action(&self, action: &SessionAction) -> Result<(), RuntimeError> {
        let draft = action_draft(action)?;
        if self.is_busy() {
            record_controller_event(
                &self.repo,
                &action.target_session_id,
                Actor::User,
                EventPayload::UserFollowupQueued {
                    command_id: action.action_id.clone(),
                    prompt: serde_json::to_value(&draft).map_err(RuntimeError::agent)?,
                },
            )?;
            lock_submission(&self.submission)
                .followups
                .push_back(QueuedFollowup {
                    command_id: action.action_id.clone(),
                    draft,
                    durably_recorded: true,
                });
            return Ok(());
        }

        transition_action(
            &self.repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Queued,
            SessionActionLifecycle::Selected,
            None,
        )?;
        transition_action(
            &self.repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Selected,
            SessionActionLifecycle::Preparing,
            None,
        )?;
        transition_action(
            &self.repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Preparing,
            SessionActionLifecycle::Committing,
            None,
        )?;

        let before = session_action_snapshot(&self.repo, &action.target_session_id)?.revision;
        let disposition = self.submit(draft)?;
        if disposition != SubmitDisposition::Started {
            return Err(RuntimeError::agent(
                "idle session action unexpectedly entered the legacy queued path",
            ));
        }
        let session = load_session(&self.repo, &action.target_session_id).map_err(RuntimeError::agent)?;
        let transcript_event_sequence = session
            .events
            .iter()
            .rev()
            .find(|event| {
                event.sequence > before && matches!(event.payload, EventPayload::UserPromptReceived { .. })
            })
            .map(|event| event.sequence)
            .ok_or_else(|| RuntimeError::agent("session action submission produced no durable user message"))?;
        record_controller_event(
            &self.repo,
            &action.target_session_id,
            Actor::Coordinator,
            EventPayload::SessionActionTranscriptLinked {
                action_id: action.action_id.clone(),
                transcript_event_sequence,
            },
        )?;
        transition_action(
            &self.repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Committing,
            SessionActionLifecycle::Running,
            Some(serde_json::json!({"transcript_event_sequence": transcript_event_sequence})),
        )?;
        transition_action(
            &self.repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Running,
            SessionActionLifecycle::Completed,
            Some(serde_json::json!({
                "delivery": "authoritative_transcript",
                "transcript_event_sequence": transcript_event_sequence,
            })),
        )?;
        Ok(())
    }

    fn deliver_idle_goal_action(&self, action: &SessionAction) -> Result<(), RuntimeError> {
        let objective = action_objective(action)?;
        transition_action(
            &self.repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Queued,
            SessionActionLifecycle::Selected,
            None,
        )?;
        transition_action(
            &self.repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Selected,
            SessionActionLifecycle::Preparing,
            None,
        )?;
        transition_action(
            &self.repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Preparing,
            SessionActionLifecycle::Committing,
            None,
        )?;
        let mut session = load_session(&self.repo, &action.target_session_id).map_err(RuntimeError::agent)?;
        medusa_agent::update_session_objective(&mut session, objective).map_err(RuntimeError::agent)?;
        let linked_sequence = session.events.last().map_or(0, |event| event.sequence);
        record_controller_event(
            &self.repo,
            &action.target_session_id,
            Actor::Coordinator,
            EventPayload::SessionActionTranscriptLinked {
                action_id: action.action_id.clone(),
                transcript_event_sequence: linked_sequence,
            },
        )?;
        transition_action(
            &self.repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Committing,
            SessionActionLifecycle::Running,
            Some(serde_json::json!({"goal_event_sequence": linked_sequence})),
        )?;
        transition_action(
            &self.repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Running,
            SessionActionLifecycle::Completed,
            Some(serde_json::json!({"delivery": "authoritative_goal"})),
        )?;
        Ok(())
    }

    fn deliver_cancel_action(&self, action: &SessionAction) -> Result<(), RuntimeError> {
        transition_action(
            &self.repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Queued,
            SessionActionLifecycle::Selected,
            None,
        )?;
        transition_action(
            &self.repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Selected,
            SessionActionLifecycle::Preparing,
            None,
        )?;
        transition_action(
            &self.repo,
            &action.target_session_id,
            &action.action_id,
            SessionActionLifecycle::Preparing,
            SessionActionLifecycle::Committing,
            None,
        )?;
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
        SessionActionKind::FollowUp
            if request.delivery_policy != SessionActionDeliveryPolicy::WhenIdle =>
        {
            Err(RuntimeError::InvalidCommand(
                "follow-up actions must use the when-idle delivery policy".to_owned(),
            ))
        }
        SessionActionKind::Cancel if request.payload != Value::Null && request.payload != serde_json::json!({}) => {
            Err(RuntimeError::InvalidCommand(
                "cancel actions do not accept a free-form payload".to_owned(),
            ))
        }
        SessionActionKind::Steer | SessionActionKind::FollowUp => {
            action_text_payload(&request.payload).map(|_| ())
        }
        SessionActionKind::GoalAdjustment => action_objective_payload(&request.payload).map(|_| ()),
        SessionActionKind::Cancel => Ok(()),
    }
}

fn action_draft(action: &SessionAction) -> Result<PromptDraft, RuntimeError> {
    let text = match action.kind {
        SessionActionKind::Steer | SessionActionKind::FollowUp => {
            action_text_payload(&action.payload)?.to_owned()
        }
        SessionActionKind::GoalAdjustment => action_objective(action)?.to_owned(),
        SessionActionKind::Cancel => {
            return Err(RuntimeError::InvalidCommand(
                "cancel action cannot be converted to a prompt".to_owned(),
            ));
        }
    };
    Ok(PromptDraft {
        text,
        attachments: Vec::new(),
        revision: action.expected_session_revision,
    })
}

fn action_text_payload(payload: &Value) -> Result<&str, RuntimeError> {
    payload
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| RuntimeError::InvalidCommand("session action requires non-empty text".to_owned()))
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
        Actor, EventPayload,
        frontend::{FrontendEvent, FrontendKind},
    };
    use serde_json::json;
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
        let action = medusa_protocol::SessionAction {
            action_id: "action-1".to_owned(),
            idempotency_key: "idem-1".to_owned(),
            source: "test".to_owned(),
            target_session_id: session_id.clone(),
            expected_session_revision: 0,
            kind: medusa_protocol::SessionActionKind::Steer,
            delivery_policy: medusa_protocol::SessionActionDeliveryPolicy::NextSafeTurnBoundary,
            wake_policy: medusa_protocol::SessionActionWakePolicy::OnBoundary,
            payload: json!({"text":"steer"}),
        };
        record_session_event(
            &mut session,
            Actor::User,
            EventPayload::SessionActionAccepted { action },
        )
        .expect("accept action");
        for (from, to) in [
            (
                medusa_protocol::SessionActionLifecycle::Queued,
                medusa_protocol::SessionActionLifecycle::Selected,
            ),
            (
                medusa_protocol::SessionActionLifecycle::Selected,
                medusa_protocol::SessionActionLifecycle::Preparing,
            ),
            (
                medusa_protocol::SessionActionLifecycle::Preparing,
                medusa_protocol::SessionActionLifecycle::Committing,
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
                from: medusa_protocol::SessionActionLifecycle::Committing,
                to: medusa_protocol::SessionActionLifecycle::Queued,
                evidence: None,
            },
        )
        .expect("journal corrupt transition for projection test");
        assert!(session_action_snapshot(directory.path(), &session_id).is_err());
    }
}
