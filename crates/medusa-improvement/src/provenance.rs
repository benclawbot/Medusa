//! Typed, privacy-filtered provenance for improvement-relevant runtime signals.
//!
//! The graph deliberately consumes [`EventPayload`] variants instead of serialized sessions.
//! Event identifiers and ranges remain the source of truth; summaries are bounded labels and
//! typed outcome metadata, never arbitrary transcript text or hidden reasoning.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, MutexGuard, OnceLock},
};

use crate::scoped_memory::RepositoryIdentity;
use medusa_core::learning_policy::LearningAdmissionPolicy;
use medusa_protocol::{Actor, EventEnvelope, EventPayload};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use ulid::Ulid;

pub const PROVENANCE_SCHEMA_VERSION: u32 = 1;

static PROVENANCE_STORE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Every currently recorded event source is listed here. The adapter below is exhaustive over
/// `EventPayload`, so adding a new protocol variant requires an explicit disposition in CI.
pub const REGISTERED_EVENT_SOURCES: &[&str] = &[
    "session_created",
    "session_state_changed",
    "user_prompt_received",
    "user_followup_queued",
    "user_followup_dequeued",
    "session_action_accepted",
    "session_action_rejected",
    "session_action_lifecycle_changed",
    "session_action_transcript_linked",
    "goal_updated",
    "conversation_compacted",
    "assumption_recorded",
    "plan_created",
    "plan_updated",
    "question_requested",
    "approval_requested",
    "approval_decision_recorded",
    "assistant_message_recorded",
    "team_state_changed",
    "worker_evidence_recorded",
    "integration_receipt_recorded",
    "recovery_action_completed",
    "checkpoint_restore_requested",
    "cancellation_requested",
    "cancellation_completed",
    "runtime_turn_finished",
    "runtime_failed",
    "session_reset",
    "model_request_started",
    "model_response_received",
    "provider_execution_recorded",
    "tool_call_requested",
    "tool_call_denied",
    "tool_execution_started",
    "tool_output_chunk",
    "tool_execution_completed",
    "tool_execution_timing_recorded",
    "file_transaction_committed",
    "checkpoint_created",
    "verification_started",
    "verification_completed",
    "session_paused",
    "session_resumed",
    "session_completed",
    "session_failed",
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceOutcome {
    Positive,
    Negative,
    Contradictory,
    Censored,
    Unresolved,
    Neutral,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceAuthority {
    UserStatement,
    ParentReview,
    IndependentVerification,
    WorkerClaim,
    ToolResult,
    CoordinatorReceipt,
    SystemRecord,
    InferredHypothesis,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceSource {
    UserCorrection,
    Approval,
    ToolExecution,
    ToolTelemetry,
    Verification,
    WorkerEvidence,
    ParentReview,
    Integration,
    Recovery,
    Cancellation,
    RuntimeFailure,
    TerminalOutcome,
    ProviderExecution,
    Artifact,
    SessionAction,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionClass {
    Session,
    Repository,
    User,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PrivacyDecision {
    pub captured: bool,
    pub redacted_fields: Vec<String>,
    pub retention: RetentionClass,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceRange {
    pub start_sequence: u64,
    pub end_sequence: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProvenanceObservation {
    pub id: String,
    pub schema_version: u32,
    pub session_id: String,
    pub root_task_id: String,
    pub trajectory_id: String,
    pub attempt_id: String,
    pub parent_id: Option<String>,
    pub worker_id: Option<String>,
    pub tool_name: Option<String>,
    pub source: ProvenanceSource,
    pub source_event_id: String,
    pub source_range: SourceRange,
    pub repository: Option<RepositoryIdentity>,
    pub revision: Option<String>,
    pub scope: String,
    pub observed_at: OffsetDateTime,
    pub ingested_at: OffsetDateTime,
    pub privacy: PrivacyDecision,
    pub authority: ProvenanceAuthority,
    pub outcome: ProvenanceOutcome,
    pub summary: String,
    pub typed_payload_digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProvenanceRelationship {
    pub id: String,
    pub kind: String,
    pub from_observation_id: String,
    pub to_observation_id: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProvenanceGraph {
    pub schema_version: u32,
    pub observations: Vec<ProvenanceObservation>,
    pub relationships: Vec<ProvenanceRelationship>,
    pub head_digest: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ProvenanceError {
    #[error("provenance capture is disabled by privacy policy")]
    PrivacyDenied,
    #[error("invalid source event: {0}")]
    InvalidEvent(String),
    #[error("conflicting provenance observation {0}")]
    ConflictingObservation(String),
    #[error("malformed provenance record: {0}")]
    MalformedRecord(String),
    #[error("provenance storage was quarantined at {0}")]
    Quarantined(PathBuf),
    #[error("provenance I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("provenance serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl ProvenanceGraph {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            schema_version: PROVENANCE_SCHEMA_VERSION,
            head_digest: graph_digest(&[], &[]).unwrap_or_default(),
            ..Self::default()
        }
    }

    pub fn ingest_event(
        &mut self,
        event: &EventEnvelope,
        root_task_id: &str,
        repository: Option<RepositoryIdentity>,
        revision: Option<String>,
        policy: &LearningAdmissionPolicy,
        ingested_at: OffsetDateTime,
    ) -> Result<bool, ProvenanceError> {
        if !policy.capture_enabled() {
            return Err(ProvenanceError::PrivacyDenied);
        }
        event
            .validate()
            .map_err(|error| ProvenanceError::InvalidEvent(error.to_string()))?;
        let Some(observation) = adapt_event(event, root_task_id, repository, revision, ingested_at)
        else {
            return Ok(false);
        };
        self.insert(observation)
    }

    pub fn insert(&mut self, observation: ProvenanceObservation) -> Result<bool, ProvenanceError> {
        validate_observation(&observation)?;
        if let Some(existing) = self
            .observations
            .iter()
            .find(|existing| existing.id == observation.id)
        {
            let mut retry = observation.clone();
            retry.ingested_at = existing.ingested_at;
            if existing == &retry {
                return Ok(false);
            }
            return Err(ProvenanceError::ConflictingObservation(observation.id));
        }
        self.observations.push(observation);
        self.observations.sort_by(|left, right| {
            left.session_id
                .cmp(&right.session_id)
                .then_with(|| {
                    left.source_range
                        .start_sequence
                        .cmp(&right.source_range.start_sequence)
                })
                .then_with(|| left.id.cmp(&right.id))
        });
        self.relationships = self
            .observations
            .windows(2)
            .filter(|window| window[0].session_id == window[1].session_id)
            .map(|window| ProvenanceRelationship {
                id: stable_id("causal", &[&window[0].id, &window[1].id]),
                kind: "causal_sequence".to_owned(),
                from_observation_id: window[0].id.clone(),
                to_observation_id: window[1].id.clone(),
            })
            .collect();
        self.head_digest = graph_digest(&self.observations, &self.relationships)?;
        Ok(true)
    }

    #[must_use]
    pub fn authoritative_success(&self, root_task_id: &str) -> bool {
        let terminal = self
            .observations
            .iter()
            .filter(|observation| {
                observation.root_task_id == root_task_id
                    && observation.source == ProvenanceSource::TerminalOutcome
                    && observation.outcome == ProvenanceOutcome::Positive
                    && observation.authority != ProvenanceAuthority::WorkerClaim
            })
            .max_by_key(|observation| observation.source_range.end_sequence);
        let Some(terminal) = terminal else {
            return false;
        };
        !self.observations.iter().any(|observation| {
            observation.root_task_id == root_task_id
                && observation.source_range.start_sequence >= terminal.source_range.start_sequence
                && matches!(
                    observation.outcome,
                    ProvenanceOutcome::Negative | ProvenanceOutcome::Contradictory
                )
        })
    }

    pub fn tool_observations(&self) -> impl Iterator<Item = &ProvenanceObservation> {
        self.observations
            .iter()
            .filter(|observation| observation.source == ProvenanceSource::ToolExecution)
    }
}

#[derive(Debug)]
pub struct ProvenanceGraphStore {
    path: PathBuf,
    graph: ProvenanceGraph,
}

impl ProvenanceGraphStore {
    pub fn open(repo: &Path) -> Result<Self, ProvenanceError> {
        let _guard = lock_provenance_store();
        let directory = repo.join(".medusa/provenance");
        fs::create_dir_all(&directory)?;
        let path = directory.join("observations.jsonl");
        let graph = read_graph(&path)?;
        Ok(Self { path, graph })
    }

    pub fn ingest_events(
        &mut self,
        events: &[EventEnvelope],
        root_task_id: &str,
        repository: Option<RepositoryIdentity>,
        revision: Option<String>,
        policy: &LearningAdmissionPolicy,
        ingested_at: OffsetDateTime,
    ) -> Result<usize, ProvenanceError> {
        let _guard = lock_provenance_store();
        self.graph = read_graph(&self.path)?;
        let mut new_observations = Vec::new();
        for event in events {
            if !policy.capture_enabled() {
                return Err(ProvenanceError::PrivacyDenied);
            }
            event
                .validate()
                .map_err(|error| ProvenanceError::InvalidEvent(error.to_string()))?;
            if let Some(observation) = adapt_event(
                event,
                root_task_id,
                repository.clone(),
                revision.clone(),
                ingested_at,
            ) {
                if self.graph.insert(observation.clone())? {
                    new_observations.push(observation);
                }
            }
        }
        if new_observations.is_empty() {
            return Ok(0);
        }
        let mut serialized = Vec::new();
        for observation in &new_observations {
            serde_json::to_writer(&mut serialized, observation)?;
            serialized.push(b'\n');
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(&serialized)?;
        file.sync_data()?;
        Ok(new_observations.len())
    }

    #[must_use]
    pub fn graph(&self) -> &ProvenanceGraph {
        &self.graph
    }
}

fn lock_provenance_store() -> MutexGuard<'static, ()> {
    PROVENANCE_STORE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn repository_identity(repo: &Path) -> Option<RepositoryIdentity> {
    let common = git_common_directory(repo)?;
    let origin = read_origin(&common)
        .or_else(|| git_output(repo, &["rev-list", "--max-parents=0", "HEAD"]))?;
    RepositoryIdentity::new(&origin, &common.to_string_lossy()).ok()
}

pub fn repository_revision(repo: &Path) -> Option<String> {
    git_output(repo, &["rev-parse", "HEAD"])
}

fn adapt_event(
    event: &EventEnvelope,
    root_task_id: &str,
    repository: Option<RepositoryIdentity>,
    revision: Option<String>,
    ingested_at: OffsetDateTime,
) -> Option<ProvenanceObservation> {
    let (source, outcome, authority, summary, tool_name): (
        ProvenanceSource,
        ProvenanceOutcome,
        ProvenanceAuthority,
        String,
        Option<String>,
    ) = match &event.payload {
        EventPayload::UserPromptReceived { .. }
        | EventPayload::UserFollowupQueued { .. }
        | EventPayload::UserFollowupDequeued { .. } => (
            ProvenanceSource::UserCorrection,
            ProvenanceOutcome::Neutral,
            ProvenanceAuthority::UserStatement,
            "user instruction or correction recorded".to_owned(),
            None,
        ),
        EventPayload::SessionActionAccepted { .. }
        | EventPayload::SessionActionRejected { .. }
        | EventPayload::SessionActionLifecycleChanged { .. }
        | EventPayload::SessionActionTranscriptLinked { .. } => (
            ProvenanceSource::SessionAction,
            ProvenanceOutcome::Neutral,
            ProvenanceAuthority::UserStatement,
            "durable session action recorded".to_owned(),
            None,
        ),
        EventPayload::ApprovalRequested { .. } | EventPayload::ApprovalDecisionRecorded { .. } => (
            ProvenanceSource::Approval,
            ProvenanceOutcome::Neutral,
            ProvenanceAuthority::UserStatement,
            "approval decision recorded".to_owned(),
            None,
        ),
        EventPayload::ToolCallDenied { tool, .. } => (
            ProvenanceSource::ToolExecution,
            ProvenanceOutcome::Negative,
            ProvenanceAuthority::ToolResult,
            format!("tool denied: {}", bounded_label(tool)),
            Some(tool.clone()),
        ),
        EventPayload::ToolExecutionCompleted { tool, exit_code } => (
            ProvenanceSource::ToolExecution,
            match exit_code {
                Some(0) => ProvenanceOutcome::Positive,
                Some(_) => ProvenanceOutcome::Negative,
                None => ProvenanceOutcome::Unresolved,
            },
            ProvenanceAuthority::ToolResult,
            format!("tool completed: {}", bounded_label(tool)),
            Some(tool.clone()),
        ),
        EventPayload::ToolExecutionTimingRecorded { tool, .. } => (
            ProvenanceSource::ToolTelemetry,
            ProvenanceOutcome::Neutral,
            ProvenanceAuthority::ToolResult,
            format!("tool timing recorded: {}", bounded_label(tool)),
            Some(tool.clone()),
        ),
        EventPayload::ToolCallRequested { tool, .. }
        | EventPayload::ToolExecutionStarted { tool } => (
            ProvenanceSource::ToolExecution,
            ProvenanceOutcome::Unresolved,
            ProvenanceAuthority::SystemRecord,
            format!("tool requested: {}", bounded_label(tool)),
            Some(tool.clone()),
        ),
        EventPayload::ToolOutputChunk { .. } => (
            ProvenanceSource::ToolExecution,
            ProvenanceOutcome::Neutral,
            ProvenanceAuthority::ToolResult,
            "tool artifact output recorded".to_owned(),
            None,
        ),
        EventPayload::VerificationStarted { .. } => (
            ProvenanceSource::Verification,
            ProvenanceOutcome::Unresolved,
            ProvenanceAuthority::IndependentVerification,
            "verification started".to_owned(),
            None,
        ),
        EventPayload::VerificationCompleted { passed, .. } => (
            ProvenanceSource::Verification,
            if *passed {
                ProvenanceOutcome::Positive
            } else {
                ProvenanceOutcome::Negative
            },
            ProvenanceAuthority::IndependentVerification,
            "verification completed".to_owned(),
            None,
        ),
        EventPayload::WorkerEvidenceRecorded { .. } => (
            ProvenanceSource::WorkerEvidence,
            ProvenanceOutcome::Unresolved,
            ProvenanceAuthority::WorkerClaim,
            "worker evidence recorded".to_owned(),
            None,
        ),
        EventPayload::IntegrationReceiptRecorded { .. } => (
            ProvenanceSource::Integration,
            ProvenanceOutcome::Positive,
            ProvenanceAuthority::ParentReview,
            "parent integration receipt recorded".to_owned(),
            None,
        ),
        EventPayload::RecoveryActionCompleted { .. } => (
            ProvenanceSource::Recovery,
            ProvenanceOutcome::Positive,
            ProvenanceAuthority::CoordinatorReceipt,
            "recovery action completed".to_owned(),
            None,
        ),
        EventPayload::CancellationCompleted | EventPayload::SessionReset { .. } => (
            ProvenanceSource::Cancellation,
            ProvenanceOutcome::Censored,
            ProvenanceAuthority::SystemRecord,
            "session execution was cancelled or reset".to_owned(),
            None,
        ),
        EventPayload::RuntimeFailed { .. } | EventPayload::SessionFailed { .. } => (
            ProvenanceSource::RuntimeFailure,
            ProvenanceOutcome::Negative,
            ProvenanceAuthority::SystemRecord,
            "runtime failure recorded".to_owned(),
            None,
        ),
        EventPayload::SessionCompleted { .. } => (
            ProvenanceSource::TerminalOutcome,
            ProvenanceOutcome::Positive,
            if matches!(event.actor, Actor::Worker(_)) {
                ProvenanceAuthority::WorkerClaim
            } else {
                ProvenanceAuthority::CoordinatorReceipt
            },
            "session terminal outcome recorded".to_owned(),
            None,
        ),
        EventPayload::FileTransactionCommitted { .. }
        | EventPayload::CheckpointCreated { .. }
        | EventPayload::CheckpointRestoreRequested { .. } => (
            ProvenanceSource::Artifact,
            ProvenanceOutcome::Positive,
            ProvenanceAuthority::SystemRecord,
            "artifact transaction recorded".to_owned(),
            None,
        ),
        EventPayload::ProviderExecutionRecorded { .. }
        | EventPayload::ModelRequestStarted { .. }
        | EventPayload::ModelResponseReceived { .. } => (
            ProvenanceSource::ProviderExecution,
            ProvenanceOutcome::Unresolved,
            ProvenanceAuthority::SystemRecord,
            "provider execution recorded".to_owned(),
            None,
        ),
        EventPayload::SessionCreated { .. }
        | EventPayload::SessionStateChanged { .. }
        | EventPayload::GoalUpdated { .. }
        | EventPayload::ConversationCompacted { .. }
        | EventPayload::AssumptionRecorded { .. }
        | EventPayload::PlanCreated { .. }
        | EventPayload::PlanUpdated { .. }
        | EventPayload::QuestionRequested { .. }
        | EventPayload::AssistantMessageRecorded { .. }
        | EventPayload::TeamStateChanged { .. }
        | EventPayload::CancellationRequested { .. }
        | EventPayload::RuntimeTurnFinished
        | EventPayload::SessionPaused { .. }
        | EventPayload::SessionResumed => return None,
    };
    let worker_id = match &event.actor {
        Actor::Worker(id) => Some(id.clone()),
        _ => None,
    };
    let source_key = source_key(source);
    let id = stable_id(source_key, &[&event.event_id.to_string()]);
    Some(ProvenanceObservation {
        id,
        schema_version: PROVENANCE_SCHEMA_VERSION,
        session_id: event.session_id.to_string(),
        root_task_id: root_task_id.to_owned(),
        trajectory_id: event.correlation_id.to_string(),
        attempt_id: format!("{}:{}", event.session_id, event.sequence),
        parent_id: (event.session_id.to_string() != root_task_id).then(|| root_task_id.to_owned()),
        worker_id,
        tool_name,
        source,
        source_event_id: event.event_id.to_string(),
        source_range: SourceRange {
            start_sequence: event.sequence,
            end_sequence: event.sequence,
        },
        repository,
        revision,
        scope: "repository".to_owned(),
        observed_at: event.timestamp,
        ingested_at,
        privacy: PrivacyDecision {
            captured: true,
            redacted_fields: vec!["payload_text".to_owned(), "hidden_reasoning".to_owned()],
            retention: RetentionClass::Repository,
        },
        authority,
        outcome,
        summary: bounded_label(&summary),
        typed_payload_digest: payload_digest(&event.payload),
    })
}

fn validate_observation(observation: &ProvenanceObservation) -> Result<(), ProvenanceError> {
    if observation.schema_version != PROVENANCE_SCHEMA_VERSION
        || observation.id.trim().is_empty()
        || observation.source_event_id.trim().is_empty()
        || observation.source_range.start_sequence > observation.source_range.end_sequence
        || !observation.privacy.captured
    {
        return Err(ProvenanceError::MalformedRecord(observation.id.clone()));
    }
    Ok(())
}

fn read_graph(path: &Path) -> Result<ProvenanceGraph, ProvenanceError> {
    let mut graph = ProvenanceGraph::empty();
    if !path.exists() {
        return Ok(graph);
    }
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let observation = match serde_json::from_str::<ProvenanceObservation>(&line) {
            Ok(observation) => observation,
            Err(error) => return quarantine(path, error.to_string()),
        };
        if let Err(error) = graph.insert(observation) {
            return quarantine(path, error.to_string());
        }
    }
    Ok(graph)
}

fn quarantine<T>(path: &Path, reason: String) -> Result<T, ProvenanceError> {
    let directory = path
        .parent()
        .map(|parent| parent.join("quarantine"))
        .ok_or_else(|| ProvenanceError::MalformedRecord(reason.clone()))?;
    fs::create_dir_all(&directory)?;
    let target = directory.join(format!(
        "observations-{}-{}.jsonl",
        OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "unknown".to_owned())
            .replace(':', "-"),
        Ulid::new()
    ));
    fs::rename(path, &target)?;
    Err(ProvenanceError::Quarantined(target))
}

fn graph_digest(
    observations: &[ProvenanceObservation],
    relationships: &[ProvenanceRelationship],
) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(&(observations, relationships))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn payload_digest(payload: &EventPayload) -> String {
    serde_json::to_vec(payload)
        .map(|bytes| hex::encode(Sha256::digest(bytes)))
        .unwrap_or_default()
}

fn stable_id(kind: &str, values: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    for value in values {
        hasher.update([0]);
        hasher.update(value.as_bytes());
    }
    format!("{kind}-{}", hex::encode(hasher.finalize()))
}

fn source_key(source: ProvenanceSource) -> &'static str {
    match source {
        ProvenanceSource::UserCorrection => "user_correction",
        ProvenanceSource::Approval => "approval",
        ProvenanceSource::ToolExecution => "tool_execution",
        ProvenanceSource::ToolTelemetry => "tool_telemetry",
        ProvenanceSource::Verification => "verification",
        ProvenanceSource::WorkerEvidence => "worker_evidence",
        ProvenanceSource::ParentReview => "parent_review",
        ProvenanceSource::Integration => "integration",
        ProvenanceSource::Recovery => "recovery",
        ProvenanceSource::Cancellation => "cancellation",
        ProvenanceSource::RuntimeFailure => "runtime_failure",
        ProvenanceSource::TerminalOutcome => "terminal_outcome",
        ProvenanceSource::ProviderExecution => "provider_execution",
        ProvenanceSource::Artifact => "artifact",
        ProvenanceSource::SessionAction => "session_action",
    }
}

fn bounded_label(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    normalized.chars().take(160).collect()
}

fn git_output(repo: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(repo)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn git_common_directory(repo: &Path) -> Option<PathBuf> {
    let dot_git = repo.join(".git");
    if dot_git.is_dir() {
        return fs::canonicalize(dot_git).ok();
    }
    let pointer = fs::read_to_string(&dot_git).ok()?;
    let git_dir = pointer.trim().strip_prefix("gitdir:")?.trim();
    let git_dir = if Path::new(git_dir).is_absolute() {
        PathBuf::from(git_dir)
    } else {
        repo.join(git_dir)
    };
    let common = fs::read_to_string(git_dir.join("commondir")).ok();
    let common = match common {
        Some(common) => git_dir.join(common.trim()),
        None => git_dir,
    };
    fs::canonicalize(common).ok()
}

fn read_origin(common: &Path) -> Option<String> {
    let config = fs::read_to_string(common.join("config")).ok()?;
    let mut in_origin = false;
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_origin = trimmed.eq_ignore_ascii_case("[remote \"origin\"]");
        } else if in_origin && trimmed.starts_with("url") {
            return trimmed
                .split_once('=')
                .map(|(_, value)| value.trim().to_owned())
                .filter(|value| !value.is_empty());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        sync::{Arc, Barrier},
        thread,
    };

    use medusa_core::{CorrelationId, SessionId, learning_policy::LearningPrivacyPolicy};
    use medusa_protocol::{Actor, EventEnvelope, EventPayload};
    use serde_json::json;

    use super::*;

    fn policy() -> LearningAdmissionPolicy {
        LearningAdmissionPolicy::from_privacy(LearningPrivacyPolicy::private_by_default())
    }

    fn event(sequence: u64, payload: EventPayload, actor: Actor) -> EventEnvelope {
        EventEnvelope::new(
            sequence,
            SessionId::new(),
            actor,
            CorrelationId::new(),
            payload,
            None,
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("event")
    }

    #[test]
    fn tool_exit_codes_are_typed_and_not_message_text() {
        let session_id = SessionId::new().to_string();
        let mut graph = ProvenanceGraph::empty();
        let success = event(
            1,
            EventPayload::ToolExecutionCompleted {
                tool: "shell_run".to_owned(),
                exit_code: Some(0),
            },
            Actor::Coordinator,
        );
        let failure = event(
            2,
            EventPayload::ToolExecutionCompleted {
                tool: "shell_run".to_owned(),
                exit_code: Some(1),
            },
            Actor::Coordinator,
        );
        // The helper assigns its own session IDs; use the event's ID as the root to keep this
        // fixture focused on typed outcome mapping.
        graph
            .ingest_event(
                &success,
                &session_id,
                None,
                None,
                &policy(),
                OffsetDateTime::UNIX_EPOCH,
            )
            .expect("success");
        graph
            .ingest_event(
                &failure,
                &session_id,
                None,
                None,
                &policy(),
                OffsetDateTime::UNIX_EPOCH,
            )
            .expect("failure");
        let outcomes = graph
            .tool_observations()
            .map(|observation| (observation.source_range.start_sequence, observation.outcome))
            .collect::<Vec<_>>();
        let mut outcomes = outcomes;
        outcomes.sort_by_key(|(sequence, _)| *sequence);
        let outcomes = outcomes
            .into_iter()
            .map(|(_, outcome)| outcome)
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes,
            vec![ProvenanceOutcome::Positive, ProvenanceOutcome::Negative]
        );
        assert!(
            graph
                .observations
                .iter()
                .all(|observation| !observation.summary.contains("runtime failed"))
        );
    }

    #[test]
    fn worker_terminal_success_is_unresolved_until_parent_receipt() {
        let worker = event(
            1,
            EventPayload::SessionCompleted {
                report_ref: "worker-report".to_owned(),
            },
            Actor::Worker("implementer".to_owned()),
        );
        let mut graph = ProvenanceGraph::empty();
        graph
            .ingest_event(
                &worker,
                "root",
                None,
                None,
                &policy(),
                OffsetDateTime::UNIX_EPOCH,
            )
            .expect("worker");
        assert!(!graph.authoritative_success("root"));
        assert_eq!(
            graph.observations[0].authority,
            ProvenanceAuthority::WorkerClaim
        );
    }

    #[test]
    fn duplicate_ingestion_is_idempotent_and_malformed_storage_is_quarantined() {
        let repo = tempfile::tempdir().expect("repo");
        let mut store = ProvenanceGraphStore::open(repo.path()).expect("store");
        let source = event(
            1,
            EventPayload::VerificationCompleted {
                passed: true,
                evidence: vec!["check".to_owned()],
            },
            Actor::Coordinator,
        );
        assert_eq!(
            store
                .ingest_events(
                    std::slice::from_ref(&source),
                    "root",
                    None,
                    None,
                    &policy(),
                    OffsetDateTime::UNIX_EPOCH,
                )
                .expect("first"),
            1
        );
        assert_eq!(
            store
                .ingest_events(
                    std::slice::from_ref(&source),
                    "root",
                    None,
                    None,
                    &policy(),
                    OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
                )
                .expect("retry"),
            0
        );
        fs::write(
            repo.path().join(".medusa/provenance/observations.jsonl"),
            b"not json\n",
        )
        .expect("corrupt");
        let error = ProvenanceGraphStore::open(repo.path()).expect_err("quarantine");
        assert!(matches!(error, ProvenanceError::Quarantined(_)));
        assert!(repo.path().join(".medusa/provenance/quarantine").is_dir());
    }

    #[test]
    fn concurrent_stores_publish_complete_observations() {
        const WORKERS: usize = 32;

        let repo = tempfile::tempdir().expect("repo");
        let repo_path = repo.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(WORKERS));
        let handles = (0..WORKERS)
            .map(|worker| {
                let repo_path = repo_path.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let mut store = ProvenanceGraphStore::open(&repo_path).expect("store");
                    let source = event(
                        worker as u64 + 1,
                        EventPayload::VerificationCompleted {
                            passed: true,
                            evidence: vec![format!("worker-{worker}")],
                        },
                        Actor::Worker(format!("worker-{worker}")),
                    );
                    barrier.wait();
                    store
                        .ingest_events(
                            &[source],
                            &format!("root-{worker}"),
                            None,
                            None,
                            &policy(),
                            OffsetDateTime::UNIX_EPOCH,
                        )
                        .expect("concurrent ingestion");
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle.join().expect("worker");
        }

        let store = ProvenanceGraphStore::open(&repo_path).expect("reopen complete store");
        assert_eq!(store.graph().observations.len(), WORKERS);
        assert!(!repo_path.join(".medusa/provenance/quarantine").exists());
    }

    #[test]
    fn registry_is_nonempty_and_repository_identity_does_not_use_worktree_path() {
        assert!(REGISTERED_EVENT_SOURCES.len() >= 40);
        assert!(repository_identity(Path::new(env!("CARGO_MANIFEST_DIR"))).is_none());
    }

    #[test]
    fn arbitrary_message_text_does_not_create_a_failure_observation() {
        let source = event(
            1,
            EventPayload::AssistantMessageRecorded {
                message: json!({"text": "runtime failed but this is only quoted prose"}),
            },
            Actor::Coordinator,
        );
        let mut graph = ProvenanceGraph::empty();
        graph
            .ingest_event(
                &source,
                "root",
                None,
                None,
                &policy(),
                OffsetDateTime::UNIX_EPOCH,
            )
            .expect("message");
        assert!(graph.observations.is_empty());
    }

    #[test]
    fn clone_worktrees_share_the_same_repository_identity() {
        let root = tempfile::tempdir().expect("root");
        let common = root.path().join(".git");
        fs::create_dir_all(common.join("worktrees/child")).expect("worktree metadata");
        fs::write(
            common.join("config"),
            "[remote \"origin\"]\n\turl = https://example.test/owner/repo.git\n",
        )
        .expect("config");
        fs::write(common.join("worktrees/child/commondir"), "../..\n").expect("commondir");
        let worktree = root.path().join("child");
        fs::create_dir_all(&worktree).expect("child");
        fs::write(worktree.join(".git"), "gitdir: ../.git/worktrees/child\n").expect("pointer");
        let original = repository_identity(root.path()).expect("original identity");
        let child = repository_identity(&worktree).expect("worktree identity");
        assert_eq!(original, child);
    }
}
