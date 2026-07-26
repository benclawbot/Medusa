//! Runtime-owned deterministic execution lifecycle.
//!
//! The orchestration crates model state; this module owns persistence and exposes
//! safe runtime operations. Storage policy is supplied by an adapter.

use std::{collections::BTreeMap, error::Error, fmt};

use medusa_continuation::ContinuationDecision;
use medusa_execution_checkpoint::{ExecutionCheckpoint as DurableCheckpoint, ExecutionLog};
use medusa_execution_orchestrator::{ExecutionStage, ExecutionState};
use medusa_execution_replay::{ExecutionTrace, ReplayReport, verify as verify_replay};
use medusa_time_travel::{ExecutionState as HistoricalState, FullSnapshot};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Storage boundary for lifecycle persistence. Implementations may use files,
/// databases, or daemon-owned storage without changing orchestration policy.
pub trait LifecycleStorage: Send + Sync {
    fn load(&self, execution_id: &str) -> Result<Option<Vec<u8>>, LifecycleError>;
    fn save(&self, execution_id: &str, bytes: &[u8]) -> Result<(), LifecycleError>;
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LifecycleProtocolEvent {
    pub execution_id: String,
    pub stage: ExecutionStage,
    pub retry_count: u32,
    pub resumed: bool,
    pub replay_provenance: Option<String>,
    pub checkpoint_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StageTraceEntry {
    pub stage: ExecutionStage,
    pub artifact_fingerprints: Vec<String>,
    pub checkpoint_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StageTrace {
    pub execution_id: String,
    pub entries: Vec<StageTraceEntry>,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TimeTravelPreview {
    pub execution_id: String,
    pub checkpoint_fingerprint: String,
    pub snapshot: FullSnapshot,
    pub confirmation_token: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DurableExecution {
    schema_version: u32,
    state: ExecutionState,
    log: ExecutionLog,
    continuation: Option<ContinuationDecision>,
    resumed: bool,
}

#[derive(Debug)]
pub struct ExecutionLifecycleService<S> {
    storage: S,
    current: DurableExecution,
}

impl<S: LifecycleStorage> ExecutionLifecycleService<S> {
    pub fn start(
        storage: S,
        execution_id: impl Into<String>,
        snapshot_fingerprint: impl Into<String>,
    ) -> Result<Self, LifecycleError> {
        let execution_id = execution_id.into();
        if storage.load(&execution_id)?.is_some() {
            return Err(LifecycleError::AlreadyExists(execution_id));
        }
        let state = ExecutionState::start(execution_id.clone(), snapshot_fingerprint)
            .map_err(LifecycleError::InvalidState)?;
        let current = DurableExecution {
            schema_version: 1,
            state,
            log: ExecutionLog::new(execution_id).map_err(LifecycleError::checkpoint)?,
            continuation: None,
            resumed: false,
        };
        let service = Self { storage, current };
        service.persist()?;
        Ok(service)
    }

    pub fn resume(storage: S, execution_id: &str) -> Result<Self, LifecycleError> {
        let bytes = storage
            .load(execution_id)?
            .ok_or_else(|| LifecycleError::NotFound(execution_id.to_owned()))?;
        let mut durable: DurableExecution = serde_json::from_slice(&bytes)
            .map_err(|error| LifecycleError::CorruptCheckpoint(error.to_string()))?;
        if durable.schema_version != 1 {
            return Err(LifecycleError::IncompatibleCheckpoint {
                expected: 1,
                actual: durable.schema_version,
            });
        }
        durable
            .state
            .validate()
            .map_err(LifecycleError::InvalidState)?;
        durable.log.verify().map_err(LifecycleError::checkpoint)?;
        let latest = durable
            .state
            .checkpoints
            .last()
            .cloned()
            .ok_or(LifecycleError::NoCheckpoint)?;
        durable.state = ExecutionState::resume(latest).map_err(LifecycleError::InvalidState)?;
        durable.resumed = true;
        let service = Self {
            storage,
            current: durable,
        };
        service.persist()?;
        Ok(service)
    }

    /// Commits one stage. The artifact-bearing checkpoint is fully constructed
    /// and validated before the storage adapter is called.
    pub fn complete_stage(
        &mut self,
        stage: ExecutionStage,
        artifacts: Vec<String>,
    ) -> Result<LifecycleProtocolEvent, LifecycleError> {
        let mut candidate = self.current.clone();
        candidate
            .state
            .complete_stage(stage, artifacts)
            .map_err(LifecycleError::InvalidState)?;
        let checkpoint = candidate
            .state
            .checkpoints
            .last()
            .cloned()
            .ok_or(LifecycleError::NoCheckpoint)?;
        let event = candidate
            .log
            .append_event(stage_kind(stage), checkpoint.fingerprint.clone())
            .map_err(LifecycleError::checkpoint)?
            .clone();
        let durable_checkpoint = DurableCheckpoint::new(
            candidate.state.execution_id.clone(),
            event.sequence,
            candidate.state.fingerprint.clone(),
            candidate.state.snapshot_fingerprint.clone(),
            Some(event.fingerprint),
            BTreeMap::from([
                (
                    "orchestrator".to_owned(),
                    candidate.state.fingerprint.clone(),
                ),
                ("stage".to_owned(), checkpoint.fingerprint.clone()),
            ]),
        )
        .map_err(LifecycleError::checkpoint)?;
        candidate
            .log
            .add_checkpoint(durable_checkpoint)
            .map_err(LifecycleError::checkpoint)?;
        self.save_candidate(candidate)?;
        Ok(self.protocol_event(None))
    }

    pub fn record_continuation(
        &mut self,
        decision: ContinuationDecision,
    ) -> Result<(), LifecycleError> {
        let mut candidate = self.current.clone();
        candidate.continuation = Some(decision);
        self.save_candidate(candidate)
    }

    pub fn protocol_event(&self, replay_provenance: Option<String>) -> LifecycleProtocolEvent {
        LifecycleProtocolEvent {
            execution_id: self.current.state.execution_id.clone(),
            stage: self.current.state.current_stage,
            retry_count: self.current.state.attempt.saturating_sub(1),
            resumed: self.current.resumed,
            replay_provenance,
            checkpoint_fingerprint: self
                .current
                .state
                .checkpoints
                .last()
                .map(|checkpoint| checkpoint.fingerprint.clone()),
        }
    }

    pub fn stage_trace(&self) -> Result<StageTrace, LifecycleError> {
        let entries = self
            .current
            .state
            .checkpoints
            .iter()
            .map(|checkpoint| StageTraceEntry {
                stage: checkpoint.completed_stage,
                artifact_fingerprints: checkpoint.artifact_fingerprints.clone(),
                checkpoint_fingerprint: checkpoint.fingerprint.clone(),
            })
            .collect::<Vec<_>>();
        let fingerprint = hash_json(&(self.current.state.execution_id.as_str(), &entries))?;
        Ok(StageTrace {
            execution_id: self.current.state.execution_id.clone(),
            entries,
            fingerprint,
        })
    }

    /// Produces an immutable diagnostic replay report.
    pub fn replay_against(&self, actual: &StageTrace) -> Result<ReplayReport, LifecycleError> {
        let expected = self.stage_trace()?;
        if expected.execution_id != actual.execution_id {
            return Err(LifecycleError::Replay(
                "execution identifiers differ".to_owned(),
            ));
        }
        let expected_trace = replay_trace(&expected)?;
        let actual_trace = replay_trace(actual)?;
        verify_replay(&expected_trace, &actual_trace)
            .map_err(|error| LifecycleError::Replay(error.to_owned()))
    }

    /// Creates a non-mutating historical view and a token required for restore.
    pub fn preview_time_travel(
        &self,
        checkpoint_fingerprint: &str,
    ) -> Result<TimeTravelPreview, LifecycleError> {
        let (index, checkpoint) = self
            .current
            .state
            .checkpoints
            .iter()
            .enumerate()
            .find(|(_, checkpoint)| checkpoint.fingerprint == checkpoint_fingerprint)
            .ok_or_else(|| LifecycleError::UnknownCheckpoint(checkpoint_fingerprint.to_owned()))?;
        let historical = HistoricalState {
            execution_id: self.current.state.execution_id.clone(),
            sequence: index as u64 + 1,
            values: BTreeMap::from([
                (
                    "stage".to_owned(),
                    format!("{:?}", checkpoint.completed_stage),
                ),
                ("checkpoint".to_owned(), checkpoint.fingerprint.clone()),
            ]),
        };
        let snapshot = FullSnapshot::new(historical);
        let confirmation_token = digest(format!(
            "restore:{}:{}",
            self.current.state.execution_id, checkpoint.fingerprint
        ));
        Ok(TimeTravelPreview {
            execution_id: self.current.state.execution_id.clone(),
            checkpoint_fingerprint: checkpoint.fingerprint.clone(),
            snapshot,
            confirmation_token,
        })
    }

    pub fn confirm_restore(
        &mut self,
        preview: &TimeTravelPreview,
        confirmation_token: &str,
    ) -> Result<LifecycleProtocolEvent, LifecycleError> {
        if preview.confirmation_token != confirmation_token {
            return Err(LifecycleError::RestoreNotConfirmed);
        }
        let checkpoint = self
            .current
            .state
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.fingerprint == preview.checkpoint_fingerprint)
            .cloned()
            .ok_or_else(|| {
                LifecycleError::UnknownCheckpoint(preview.checkpoint_fingerprint.clone())
            })?;
        let mut candidate = self.current.clone();
        candidate.state =
            ExecutionState::resume(checkpoint).map_err(LifecycleError::InvalidState)?;
        candidate.resumed = true;
        self.save_candidate(candidate)?;
        Ok(self.protocol_event(Some(preview.snapshot.fingerprint.clone())))
    }

    pub fn state(&self) -> &ExecutionState {
        &self.current.state
    }

    fn save_candidate(&mut self, candidate: DurableExecution) -> Result<(), LifecycleError> {
        let bytes = serde_json::to_vec_pretty(&candidate)
            .map_err(|error| LifecycleError::Serialization(error.to_string()))?;
        self.storage.save(&candidate.state.execution_id, &bytes)?;
        self.current = candidate;
        Ok(())
    }

    fn persist(&self) -> Result<(), LifecycleError> {
        let bytes = serde_json::to_vec_pretty(&self.current)
            .map_err(|error| LifecycleError::Serialization(error.to_string()))?;
        self.storage.save(&self.current.state.execution_id, &bytes)
    }
}

fn replay_trace(trace: &StageTrace) -> Result<ExecutionTrace, LifecycleError> {
    let ordered = trace
        .entries
        .iter()
        .map(|entry| format!("{:?}:{}", entry.stage, entry.checkpoint_fingerprint))
        .collect::<Vec<_>>();
    let tasks = trace
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (index.to_string(), digest(&entry.artifact_fingerprints)))
        .collect();
    ExecutionTrace::new(
        trace.execution_id.clone(),
        digest("snapshot"),
        digest(&ordered),
        digest("leases"),
        digest("barrier"),
        None,
        tasks,
        digest(&trace.fingerprint),
    )
    .map_err(|error| LifecycleError::Replay(error.to_owned()))
}

fn stage_kind(stage: ExecutionStage) -> &'static str {
    match stage {
        ExecutionStage::Snapshot => "stage.snapshot",
        ExecutionStage::Context => "stage.context",
        ExecutionStage::Memory => "stage.memory",
        ExecutionStage::Plan => "stage.plan",
        ExecutionStage::Workers => "stage.workers",
        ExecutionStage::ReadSetValidation => "stage.read_set_validation",
        ExecutionStage::PatchTransaction => "stage.patch_transaction",
        ExecutionStage::Verification => "stage.verification",
        ExecutionStage::MemoryConsolidation => "stage.memory_consolidation",
        ExecutionStage::MemoryWriteback => "stage.memory_writeback",
        ExecutionStage::Manifest => "stage.manifest",
        ExecutionStage::Complete => "stage.complete",
    }
}

fn digest<T: Serialize>(value: T) -> String {
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

fn hash_json<T: Serialize>(value: &T) -> Result<String, LifecycleError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| LifecycleError::Serialization(error.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LifecycleError {
    AlreadyExists(String),
    NotFound(String),
    NoCheckpoint,
    UnknownCheckpoint(String),
    RestoreNotConfirmed,
    InvalidState(&'static str),
    CorruptCheckpoint(String),
    IncompatibleCheckpoint { expected: u32, actual: u32 },
    Checkpoint(String),
    Replay(String),
    Serialization(String),
    Storage(String),
}

impl LifecycleError {
    fn checkpoint(error: impl fmt::Display) -> Self {
        Self::Checkpoint(error.to_string())
    }
}

impl fmt::Display for LifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyExists(id) => write!(formatter, "execution already exists: {id}"),
            Self::NotFound(id) => write!(formatter, "execution was not found: {id}"),
            Self::NoCheckpoint => formatter.write_str("execution has no durable checkpoint"),
            Self::UnknownCheckpoint(value) => write!(formatter, "unknown checkpoint: {value}"),
            Self::RestoreNotConfirmed => {
                formatter.write_str("time-travel restore was not confirmed")
            }
            Self::InvalidState(value) => write!(formatter, "invalid execution state: {value}"),
            Self::CorruptCheckpoint(value) => write!(formatter, "corrupt checkpoint: {value}"),
            Self::IncompatibleCheckpoint { expected, actual } => write!(
                formatter,
                "incompatible checkpoint schema: expected {expected}, found {actual}"
            ),
            Self::Checkpoint(value) => write!(formatter, "checkpoint error: {value}"),
            Self::Replay(value) => write!(formatter, "replay error: {value}"),
            Self::Serialization(value) => write!(formatter, "serialization error: {value}"),
            Self::Storage(value) => write!(formatter, "storage error: {value}"),
        }
    }
}

impl Error for LifecycleError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug, Default)]
    struct MemoryStorage(Arc<Mutex<BTreeMap<String, Vec<u8>>>>);

    impl LifecycleStorage for MemoryStorage {
        fn load(&self, execution_id: &str) -> Result<Option<Vec<u8>>, LifecycleError> {
            Ok(self.0.lock().unwrap().get(execution_id).cloned())
        }

        fn save(&self, execution_id: &str, bytes: &[u8]) -> Result<(), LifecycleError> {
            self.0
                .lock()
                .unwrap()
                .insert(execution_id.to_owned(), bytes.to_vec());
            Ok(())
        }
    }

    fn artifact(label: &str) -> String {
        digest(label)
    }

    #[test]
    fn restart_after_three_stages_skips_completed_work() {
        let storage = MemoryStorage::default();
        let mut first =
            ExecutionLifecycleService::start(storage.clone(), "run-1", artifact("snapshot"))
                .unwrap();
        for stage in [
            ExecutionStage::Snapshot,
            ExecutionStage::Context,
            ExecutionStage::Memory,
        ] {
            first
                .complete_stage(stage, vec![artifact(&format!("{stage:?}"))])
                .unwrap();
        }
        drop(first);

        let resumed = ExecutionLifecycleService::resume(storage, "run-1").unwrap();
        assert_eq!(resumed.state().current_stage, ExecutionStage::Plan);
        assert_eq!(resumed.state().checkpoints.len(), 1);
        assert!(resumed.protocol_event(None).resumed);
    }

    #[test]
    fn deterministic_replay_matches_stage_order_and_artifacts() {
        let storage = MemoryStorage::default();
        let mut service =
            ExecutionLifecycleService::start(storage, "run-2", artifact("snapshot")).unwrap();
        service
            .complete_stage(ExecutionStage::Snapshot, vec![artifact("a")])
            .unwrap();
        service
            .complete_stage(ExecutionStage::Context, vec![artifact("b")])
            .unwrap();
        let trace = service.stage_trace().unwrap();
        let report = service.replay_against(&trace).unwrap();
        assert!(report.equivalent);
    }

    #[test]
    fn corrupt_checkpoint_fails_closed() {
        let storage = MemoryStorage::default();
        storage.save("broken", b"{not json").unwrap();
        let error = ExecutionLifecycleService::resume(storage, "broken").unwrap_err();
        assert!(matches!(error, LifecycleError::CorruptCheckpoint(_)));
    }

    #[test]
    fn time_travel_requires_explicit_confirmation() {
        let storage = MemoryStorage::default();
        let mut service =
            ExecutionLifecycleService::start(storage, "run-3", artifact("snapshot")).unwrap();
        service
            .complete_stage(ExecutionStage::Snapshot, vec![artifact("a")])
            .unwrap();
        let fingerprint = service.state().checkpoints[0].fingerprint.clone();
        let preview = service.preview_time_travel(&fingerprint).unwrap();
        let before = service.state().fingerprint.clone();
        assert_eq!(before, service.state().fingerprint);
        assert_eq!(
            service.confirm_restore(&preview, "wrong").unwrap_err(),
            LifecycleError::RestoreNotConfirmed
        );
        assert_eq!(before, service.state().fingerprint);
        service
            .confirm_restore(&preview, &preview.confirmation_token)
            .unwrap();
    }
}
