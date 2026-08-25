//! Journal-derived checkpoint, replay, historical-state, and continuity health.
//!
//! The canonical `medusa-agent` session journal remains the only execution authority. This module
//! projects verified journal records into the retained checkpoint, replay, time-travel, and
//! continuity contracts without persisting a second copy of runtime truth.

use std::{collections::BTreeMap, path::Path};

use medusa_agent::{
    AgentSession,
    session_browser::{load_session, replay_events},
};
use medusa_execution_checkpoint::{ExecutionCheckpoint, ExecutionLog};
use medusa_execution_replay::{ExecutionTrace, ReplayReport};
use medusa_protocol::{EventEnvelope, EventPayload};
use medusa_session_continuity::ContinuityStore;
use medusa_time_travel::{ExecutionState, FullSnapshot, StateDelta, TimeTravelStore};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{RuntimeController, RuntimeError};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuntimeContinuityHealth {
    pub revision: u64,
    pub owner_client_id: Option<String>,
    pub attachment_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuntimeHistoricalState {
    pub session_id: String,
    pub cursor: u64,
    pub values: BTreeMap<String, String>,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuntimeExecutionHealth {
    pub session_id: String,
    pub journal_cursor: u64,
    pub journal_fingerprint: String,
    pub checkpoint: ExecutionCheckpoint,
    pub replay: ReplayReport,
    pub latest_state: RuntimeHistoricalState,
    pub continuity: Option<RuntimeContinuityHealth>,
}

impl RuntimeController {
    /// Returns verified execution health derived from the canonical journal.
    pub fn execution_health(
        &self,
        session_id: &str,
    ) -> Result<RuntimeExecutionHealth, RuntimeError> {
        inspect(&self.repo, session_id)
    }

    /// Reconstructs a read-only historical state at a zero-based journal cursor.
    pub fn historical_state(
        &self,
        session_id: &str,
        cursor: u64,
    ) -> Result<RuntimeHistoricalState, RuntimeError> {
        historical(&self.repo, session_id, cursor)
    }
}

pub(crate) fn verify_resumed_session(
    repo: &Path,
    session: &AgentSession,
) -> Result<(), RuntimeError> {
    let session_id = session.id.to_string();
    let journal_events = replay_events(repo, &session_id, 0).map_err(RuntimeError::agent)?;
    let expected = trace(&session_id, &journal_events)?;
    let actual = trace(&session_id, &session.events)?;
    let replay =
        medusa_execution_replay::verify(&expected, &actual).map_err(RuntimeError::agent)?;
    replay.validate().map_err(RuntimeError::agent)?;
    if !replay.equivalent {
        let first = replay
            .divergences
            .first()
            .map(|divergence| format!("{:?}:{}", divergence.kind, divergence.subject))
            .unwrap_or_else(|| "unknown divergence".to_owned());
        return Err(RuntimeError::agent(format!(
            "canonical journal replay diverged for session {} at {first}",
            session.id
        )));
    }
    Ok(())
}
pub fn inspect(repo: &Path, session_id: &str) -> Result<RuntimeExecutionHealth, RuntimeError> {
    let session = load_session(repo, session_id).map_err(RuntimeError::agent)?;
    inspect_session(repo, &session)
}

pub fn historical(
    repo: &Path,
    session_id: &str,
    cursor: u64,
) -> Result<RuntimeHistoricalState, RuntimeError> {
    let events = replay_events(repo, session_id, 0).map_err(RuntimeError::agent)?;
    historical_from_events(session_id, &events, cursor)
}

fn inspect_session(
    repo: &Path,
    session: &AgentSession,
) -> Result<RuntimeExecutionHealth, RuntimeError> {
    let session_id = session.id.to_string();
    let journal_events = replay_events(repo, &session_id, 0).map_err(RuntimeError::agent)?;
    let log = execution_log(repo, &session_id, &journal_events)?;
    let checkpoint = build_checkpoint(session, &journal_events, log.latest_checkpoint())?;
    let expected = trace(&session_id, &journal_events)?;
    let actual = trace(&session_id, &session.events)?;
    let replay =
        medusa_execution_replay::verify(&expected, &actual).map_err(RuntimeError::agent)?;
    replay.validate().map_err(RuntimeError::agent)?;
    let journal_cursor = u64::try_from(journal_events.len()).unwrap_or(u64::MAX);
    let latest_state = historical_from_events(&session_id, &journal_events, journal_cursor)?;
    let journal_fingerprint = digest(&journal_events)?;
    let continuity = continuity_health(repo, &session_id)?;

    Ok(RuntimeExecutionHealth {
        session_id,
        journal_cursor,
        journal_fingerprint,
        checkpoint,
        replay,
        latest_state,
        continuity,
    })
}

fn execution_log(
    repo: &Path,
    session_id: &str,
    events: &[EventEnvelope],
) -> Result<ExecutionLog, RuntimeError> {
    let mut log = ExecutionLog::new(session_id).map_err(RuntimeError::agent)?;
    for event in events {
        log.append_event(payload_kind(&event.payload), digest(&event.payload)?)
            .map_err(RuntimeError::agent)?;
    }
    let state = reduce(session_id, events);
    let checkpoint = ExecutionCheckpoint::new(
        session_id,
        u64::try_from(events.len()).unwrap_or(u64::MAX),
        digest(&state.values)?,
        crate::checkpoint_payload::repository_fingerprint(repo, events)?,
        log.events.last().map(|event| event.fingerprint.clone()),
        subsystem_fingerprints(events)?,
    )
    .map_err(RuntimeError::agent)?;
    log.add_checkpoint(checkpoint)
        .map_err(RuntimeError::agent)?;
    log.verify().map_err(RuntimeError::agent)?;
    Ok(log)
}

fn build_checkpoint(
    session: &AgentSession,
    events: &[EventEnvelope],
    checkpoint: Option<&ExecutionCheckpoint>,
) -> Result<ExecutionCheckpoint, RuntimeError> {
    let checkpoint = checkpoint
        .cloned()
        .ok_or_else(|| RuntimeError::agent("journal-derived checkpoint was not created"))?;
    let expected_supervisor = digest(&reduce(&session.id.to_string(), events).values)?;
    if checkpoint.supervisor_fingerprint != expected_supervisor {
        return Err(RuntimeError::agent(
            "journal-derived checkpoint supervisor fingerprint mismatch",
        ));
    }
    checkpoint.verify().map_err(RuntimeError::agent)?;
    Ok(checkpoint)
}

fn historical_from_events(
    session_id: &str,
    events: &[EventEnvelope],
    cursor: u64,
) -> Result<RuntimeHistoricalState, RuntimeError> {
    let cursor_index = usize::try_from(cursor).map_err(RuntimeError::agent)?;
    if cursor_index > events.len() {
        return Err(RuntimeError::InvalidCommand(format!(
            "journal cursor {cursor} is beyond session {session_id} cursor {}",
            events.len()
        )));
    }
    let base = FullSnapshot::new(ExecutionState {
        execution_id: session_id.to_owned(),
        sequence: 0,
        values: BTreeMap::new(),
    })
    .map_err(RuntimeError::agent)?;
    let target = reduce(session_id, &events[..cursor_index]);
    let mut store = TimeTravelStore::default();
    store
        .insert_snapshot(base.clone())
        .map_err(RuntimeError::agent)?;
    if cursor > 0 {
        let delta = StateDelta::between(&base, &target).map_err(RuntimeError::agent)?;
        store.insert_delta(delta).map_err(RuntimeError::agent)?;
    }
    let restored = store
        .restore(session_id, cursor)
        .map_err(RuntimeError::agent)?;
    let fingerprint = digest(&restored)?;
    Ok(RuntimeHistoricalState {
        session_id: restored.execution_id,
        cursor: restored.sequence,
        values: restored.values,
        fingerprint,
    })
}

fn reduce(session_id: &str, events: &[EventEnvelope]) -> ExecutionState {
    let mut values = BTreeMap::new();
    let mut counts = BTreeMap::<String, u64>::new();
    values.insert("session_id".to_owned(), session_id.to_owned());
    for event in events {
        let kind = payload_kind(&event.payload).to_owned();
        *counts.entry(kind.clone()).or_default() += 1;
        values.insert("last_event_kind".to_owned(), kind);
        values.insert("last_event_checksum".to_owned(), event.checksum.clone());
        values.insert("last_event_sequence".to_owned(), event.sequence.to_string());
        match &event.payload {
            EventPayload::SessionCreated { objective }
            | EventPayload::GoalUpdated { objective } => {
                values.insert("objective".to_owned(), objective.clone());
            }
            EventPayload::SessionStateChanged { to, .. } => {
                values.insert("session_state".to_owned(), format!("{to:?}"));
            }
            EventPayload::UserFollowupQueued { command_id, .. } => {
                values.insert(format!("followup:{command_id}"), "queued".to_owned());
            }
            EventPayload::UserFollowupDequeued { command_id, .. } => {
                values.insert(format!("followup:{command_id}"), "dequeued".to_owned());
            }
            EventPayload::PlanCreated { plan } => {
                values.insert("plan".to_owned(), digest_lossy(plan));
            }
            EventPayload::PlanUpdated { update } => {
                values.insert("plan".to_owned(), digest_lossy(update));
            }
            EventPayload::QuestionRequested { question } => {
                values.insert("question".to_owned(), digest_lossy(question));
            }
            EventPayload::ApprovalRequested { request } => {
                values.insert("approval".to_owned(), digest_lossy(request));
            }
            EventPayload::ApprovalDecisionRecorded { decision } => {
                values.insert("approval".to_owned(), digest_lossy(decision));
            }
            EventPayload::TeamStateChanged { snapshot } => {
                values.insert("team".to_owned(), digest_lossy(snapshot));
            }
            EventPayload::IntegrationReceiptRecorded { receipt } => {
                values.insert("integration".to_owned(), digest_lossy(receipt));
            }
            EventPayload::RecoveryActionCompleted { receipt } => {
                values.insert("recovery".to_owned(), digest_lossy(receipt));
            }
            EventPayload::CheckpointRestoreRequested {
                checkpoint_id,
                source_cursor,
            } => {
                values.insert("restore_checkpoint".to_owned(), checkpoint_id.clone());
                values.insert(
                    "restore_source_cursor".to_owned(),
                    source_cursor.to_string(),
                );
            }
            EventPayload::VerificationCompleted { passed, evidence } => {
                values.insert("verification_passed".to_owned(), passed.to_string());
                values.insert("verification_evidence".to_owned(), digest_lossy(evidence));
            }
            EventPayload::CancellationRequested { .. } => {
                values.insert("cancellation".to_owned(), "requested".to_owned());
            }
            EventPayload::CancellationCompleted => {
                values.insert("cancellation".to_owned(), "completed".to_owned());
            }
            EventPayload::RuntimeFailed { message } => {
                values.insert("runtime_failure".to_owned(), digest_lossy(message));
            }
            EventPayload::SessionCompleted { report_ref } => {
                values.insert("completion".to_owned(), digest_lossy(report_ref));
            }
            EventPayload::SessionFailed { error } => {
                values.insert("session_failure".to_owned(), digest_lossy(error));
            }
            _ => {}
        }
    }
    for (kind, count) in counts {
        values.insert(format!("count:{kind}"), count.to_string());
    }
    ExecutionState {
        execution_id: session_id.to_owned(),
        sequence: u64::try_from(events.len()).unwrap_or(u64::MAX),
        values,
    }
}

fn trace(session_id: &str, events: &[EventEnvelope]) -> Result<ExecutionTrace, RuntimeError> {
    let initial = events
        .first()
        .map_or_else(|| digest(&Vec::<EventEnvelope>::new()), digest)?;
    let schedule = category_fingerprint(events, |payload| {
        matches!(
            payload,
            EventPayload::PlanCreated { .. }
                | EventPayload::PlanUpdated { .. }
                | EventPayload::TeamStateChanged { .. }
        )
    })?;
    let leases = category_fingerprint(events, |payload| {
        matches!(
            payload,
            EventPayload::TeamStateChanged { .. } | EventPayload::WorkerEvidenceRecorded { .. }
        )
    })?;
    let barrier = category_fingerprint(events, |payload| {
        matches!(
            payload,
            EventPayload::IntegrationReceiptRecorded { .. }
                | EventPayload::VerificationStarted { .. }
                | EventPayload::VerificationCompleted { .. }
                | EventPayload::SessionCompleted { .. }
                | EventPayload::SessionFailed { .. }
        )
    })?;
    let rollback_events = events
        .iter()
        .filter(|event| {
            matches!(
                &event.payload,
                EventPayload::FileTransactionCommitted { .. }
                    | EventPayload::IntegrationReceiptRecorded { .. }
            )
        })
        .collect::<Vec<_>>();
    let rollback_journal = (!rollback_events.is_empty())
        .then(|| digest(&rollback_events))
        .transpose()?;
    let mut task_outputs = BTreeMap::new();
    for event in events {
        if matches!(
            &event.payload,
            EventPayload::WorkerEvidenceRecorded { .. }
                | EventPayload::ToolExecutionCompleted { .. }
                | EventPayload::IntegrationReceiptRecorded { .. }
                | EventPayload::VerificationCompleted { .. }
        ) {
            task_outputs.insert(format!("event-{}", event.sequence), digest(event)?);
        }
    }
    ExecutionTrace::new(
        session_id,
        initial,
        schedule,
        leases,
        barrier,
        rollback_journal,
        task_outputs,
        digest(events)?,
    )
    .map_err(RuntimeError::agent)
}

fn category_fingerprint<F>(events: &[EventEnvelope], include: F) -> Result<String, RuntimeError>
where
    F: Fn(&EventPayload) -> bool,
{
    let selected = events
        .iter()
        .filter(|event| include(&event.payload))
        .collect::<Vec<_>>();
    digest(&selected)
}

fn subsystem_fingerprints(
    events: &[EventEnvelope],
) -> Result<BTreeMap<String, String>, RuntimeError> {
    Ok(BTreeMap::from([
        (
            "approvals".to_owned(),
            category_fingerprint(events, |payload| {
                matches!(
                    payload,
                    EventPayload::ApprovalRequested { .. }
                        | EventPayload::ApprovalDecisionRecorded { .. }
                )
            })?,
        ),
        (
            "queue".to_owned(),
            category_fingerprint(events, |payload| {
                matches!(
                    payload,
                    EventPayload::UserFollowupQueued { .. }
                        | EventPayload::UserFollowupDequeued { .. }
                )
            })?,
        ),
        (
            "tools".to_owned(),
            category_fingerprint(events, |payload| {
                matches!(
                    payload,
                    EventPayload::ToolCallRequested { .. }
                        | EventPayload::ToolCallDenied { .. }
                        | EventPayload::ToolExecutionStarted { .. }
                        | EventPayload::ToolOutputChunk { .. }
                        | EventPayload::ToolExecutionCompleted { .. }
                )
            })?,
        ),
        (
            "workers".to_owned(),
            category_fingerprint(events, |payload| {
                matches!(
                    payload,
                    EventPayload::TeamStateChanged { .. }
                        | EventPayload::WorkerEvidenceRecorded { .. }
                )
            })?,
        ),
    ]))
}

fn continuity_health(
    repo: &Path,
    session_id: &str,
) -> Result<Option<RuntimeContinuityHealth>, RuntimeError> {
    let path = repo
        .join(".medusa/continuity")
        .join(format!("{session_id}.json"));
    if !path.is_file() {
        return Ok(None);
    }
    let continuity = ContinuityStore::new(path)
        .load()
        .map_err(RuntimeError::agent)?;
    if continuity.session_id != session_id {
        return Err(RuntimeError::agent(format!(
            "continuity state belongs to session {}, not {session_id}",
            continuity.session_id
        )));
    }
    Ok(Some(RuntimeContinuityHealth {
        revision: continuity.revision,
        owner_client_id: continuity.owner_client_id,
        attachment_count: continuity.attachments.len(),
    }))
}

fn payload_kind(payload: &EventPayload) -> &'static str {
    match payload {
        EventPayload::SessionCreated { .. } => "session_created",
        EventPayload::RuntimeConfigurationBound { .. } => "runtime_configuration_bound",
        EventPayload::SessionStateChanged { .. } => "session_state_changed",
        EventPayload::SessionActionAccepted { .. } => "session_action_accepted",
        EventPayload::SessionActionRejected { .. } => "session_action_rejected",
        EventPayload::SessionActionLifecycleChanged { .. } => "session_action_lifecycle_changed",
        EventPayload::SessionActionTranscriptLinked { .. } => "session_action_transcript_linked",
        EventPayload::UserPromptReceived { .. } => "user_prompt_received",
        EventPayload::UserFollowupQueued { .. } => "user_followup_queued",
        EventPayload::UserFollowupDequeued { .. } => "user_followup_dequeued",
        EventPayload::GoalUpdated { .. } => "goal_updated",
        EventPayload::ConversationCompacted { .. } => "conversation_compacted",
        EventPayload::AssumptionRecorded { .. } => "assumption_recorded",
        EventPayload::PlanCreated { .. } => "plan_created",
        EventPayload::PlanUpdated { .. } => "plan_updated",
        EventPayload::QuestionRequested { .. } => "question_requested",
        EventPayload::ApprovalRequested { .. } => "approval_requested",
        EventPayload::ApprovalDecisionRecorded { .. } => "approval_decision_recorded",
        EventPayload::AssistantMessageRecorded { .. } => "assistant_message_recorded",
        EventPayload::TeamStateChanged { .. } => "team_state_changed",
        EventPayload::WorkerEvidenceRecorded { .. } => "worker_evidence_recorded",
        EventPayload::IntegrationReceiptRecorded { .. } => "integration_receipt_recorded",
        EventPayload::RecoveryActionCompleted { .. } => "recovery_action_completed",
        EventPayload::CheckpointRestoreRequested { .. } => "checkpoint_restore_requested",
        EventPayload::CancellationRequested { .. } => "cancellation_requested",
        EventPayload::CancellationCompleted => "cancellation_completed",
        EventPayload::RuntimeTurnFinished => "runtime_turn_finished",
        EventPayload::RuntimeFailed { .. } => "runtime_failed",
        EventPayload::SessionReset { .. } => "session_reset",
        EventPayload::ModelRequestStarted { .. } => "model_request_started",
        EventPayload::ModelResponseReceived { .. } => "model_response_received",
        EventPayload::ModelRequestFailed { .. } => "model_request_failed",
        EventPayload::ProviderExecutionRecorded { .. } => "provider_execution_recorded",
        EventPayload::ToolCallRequested { .. } => "tool_call_requested",
        EventPayload::ToolCallDenied { .. } => "tool_call_denied",
        EventPayload::ToolExecutionStarted { .. } => "tool_execution_started",
        EventPayload::ToolOutputChunk { .. } => "tool_output_chunk",
        EventPayload::ToolExecutionCompleted { .. } => "tool_execution_completed",
        EventPayload::ToolExecutionTimingRecorded { .. } => "tool_execution_timing_recorded",
        EventPayload::FileTransactionCommitted { .. } => "file_transaction_committed",
        EventPayload::CheckpointCreated { .. } => "checkpoint_created",
        EventPayload::VerificationStarted { .. } => "verification_started",
        EventPayload::VerificationCompleted { .. } => "verification_completed",
        EventPayload::SessionPaused { .. } => "session_paused",
        EventPayload::SessionResumed => "session_resumed",
        EventPayload::SessionCompleted { .. } => "session_completed",
        EventPayload::SessionFailed { .. } => "session_failed",
    }
}

fn digest<T: Serialize + ?Sized>(value: &T) -> Result<String, RuntimeError> {
    let bytes = serde_json::to_vec(value).map_err(RuntimeError::agent)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn digest_lossy<T: Serialize + ?Sized>(value: &T) -> String {
    digest(value).unwrap_or_else(|_| hex::encode(Sha256::digest([])))
}

#[cfg(test)]
mod tests {
    use medusa_agent::{AgentEngine, record_session_event};
    use medusa_config::Config;
    use medusa_core::MedusaResult;
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};
    use serde_json::json;

    use super::*;

    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("session creation does not call the provider")
        }
    }

    #[test]
    fn journal_projects_checkpoint_replay_and_historical_state() {
        let repository = tempfile::tempdir().expect("repository");
        let engine = AgentEngine::new(UnusedProvider, Config::default());
        let mut session = engine
            .create_session(repository.path(), "Inspect execution history".to_owned())
            .expect("session");
        record_session_event(
            &mut session,
            medusa_protocol::Actor::Coordinator,
            EventPayload::PlanUpdated {
                update: json!({"step": "verify journal"}),
            },
        )
        .expect("plan event");
        record_session_event(
            &mut session,
            medusa_protocol::Actor::Coordinator,
            EventPayload::VerificationCompleted {
                passed: true,
                evidence: vec!["journal-ok".to_owned()],
            },
        )
        .expect("verification event");

        let health = inspect(repository.path(), &session.id.to_string()).expect("health");
        assert!(health.replay.equivalent);
        assert_eq!(health.journal_cursor, 3);
        assert_eq!(health.checkpoint.sequence, 3);
        assert_eq!(
            health.latest_state.values.get("verification_passed"),
            Some(&"true".to_owned())
        );

        let earlier =
            historical(repository.path(), &session.id.to_string(), 2).expect("historical");
        assert_eq!(earlier.cursor, 2);
        assert!(!earlier.values.contains_key("verification_passed"));
    }

    #[test]
    fn historical_cursor_beyond_journal_fails_closed() {
        let repository = tempfile::tempdir().expect("repository");
        let engine = AgentEngine::new(UnusedProvider, Config::default());
        let session = engine
            .create_session(repository.path(), "Reject invalid cursor".to_owned())
            .expect("session");

        let error = historical(repository.path(), &session.id.to_string(), 2)
            .expect_err("cursor must fail");
        assert!(error.to_string().contains("beyond"));
    }
    #[test]
    fn malformed_optional_continuity_does_not_block_resume_verification() {
        let repository = tempfile::tempdir().expect("repository");
        let engine = AgentEngine::new(UnusedProvider, Config::default());
        let session = engine
            .create_session(repository.path(), "Resume canonical journal".to_owned())
            .expect("session");
        let continuity = repository
            .path()
            .join(".medusa/continuity")
            .join(format!("{}.json", session.id));
        std::fs::create_dir_all(continuity.parent().expect("continuity parent"))
            .expect("continuity directory");
        std::fs::write(&continuity, b"{malformed").expect("malformed continuity");

        let restored = load_session(repository.path(), &session.id.to_string())
            .expect("journal-backed session");
        verify_resumed_session(repository.path(), &restored)
            .expect("optional continuity must not block resume verification");
        assert!(inspect(repository.path(), &session.id.to_string()).is_err());
    }
}
