//! Durable structured progress events and restart-safe checkpoints.

use std::{fs, path::PathBuf};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use medusa_goal::{CompletionEvidence, GoalContract};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressKind {
    Started,
    PlanUpdated,
    ToolStarted,
    ToolFinished,
    CheckpointCreated,
    Blocked,
    Retrying,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProgressEvent {
    pub sequence: u64,
    pub kind: ProgressKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<String>,
}

impl ProgressEvent {
    pub fn new(
        sequence: u64,
        kind: ProgressKind,
        message: impl Into<String>,
    ) -> MedusaResult<Self> {
        let message = message.into();
        if message.trim().is_empty() {
            return Err(validation("progress message cannot be empty"));
        }
        Ok(Self {
            sequence,
            kind,
            message,
            step_id: None,
            checkpoint_id: None,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionCheckpoint {
    pub schema_version: u32,
    pub checkpoint_id: String,
    pub session_id: SessionId,
    pub sequence: u64,
    pub goal: GoalContract,
    #[serde(default)]
    pub evidence: Vec<CompletionEvidence>,
    #[serde(default)]
    pub progress: Vec<ProgressEvent>,
    pub state: serde_json::Value,
    pub checksum: String,
}

impl ExecutionCheckpoint {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn new(
        session_id: SessionId,
        sequence: u64,
        goal: GoalContract,
        evidence: Vec<CompletionEvidence>,
        progress: Vec<ProgressEvent>,
        state: serde_json::Value,
    ) -> MedusaResult<Self> {
        validate_progress(sequence, &progress)?;
        let checkpoint_id = format!("chk-{}-{sequence}", session_id.as_str());
        let mut checkpoint = Self {
            schema_version: Self::SCHEMA_VERSION,
            checkpoint_id,
            session_id,
            sequence,
            goal,
            evidence,
            progress,
            state,
            checksum: String::new(),
        };
        checkpoint.checksum = checkpoint.calculate_checksum()?;
        Ok(checkpoint)
    }

    pub fn verify(&self) -> MedusaResult<()> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(validation("unsupported checkpoint schema version"));
        }
        validate_progress(self.sequence, &self.progress)?;
        if self.checksum != self.calculate_checksum()? {
            return Err(MedusaError::new(
                ErrorCode::ChecksumMismatch,
                ErrorCategory::Persistence,
                "checkpoint checksum mismatch",
            ));
        }
        Ok(())
    }

    fn calculate_checksum(&self) -> MedusaResult<String> {
        let mut unsigned = self.clone();
        unsigned.checksum.clear();
        let encoded = serde_json::to_vec(&unsigned)?;
        Ok(hex::encode(Sha256::digest(encoded)))
    }
}

#[derive(Clone, Debug)]
pub struct CheckpointStore {
    root: PathBuf,
}

impl CheckpointStore {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn save(&self, checkpoint: &ExecutionCheckpoint) -> MedusaResult<PathBuf> {
        checkpoint.verify()?;
        let directory = self.root.join(checkpoint.session_id.as_str());
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{:020}.json", checkpoint.sequence));
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(checkpoint)?)?;
        fs::rename(&temporary, &path)?;
        Ok(path)
    }

    pub fn load_latest(&self, session_id: &SessionId) -> MedusaResult<Option<ExecutionCheckpoint>> {
        let directory = self.root.join(session_id.as_str());
        if !directory.is_dir() {
            return Ok(None);
        }
        let mut paths = fs::read_dir(directory)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect::<Vec<_>>();
        paths.sort();
        let Some(path) = paths.pop() else {
            return Ok(None);
        };
        let checkpoint: ExecutionCheckpoint = serde_json::from_slice(&fs::read(path)?)?;
        checkpoint.verify()?;
        Ok(Some(checkpoint))
    }
}

fn validate_progress(sequence: u64, progress: &[ProgressEvent]) -> MedusaResult<()> {
    let mut previous = None;
    for event in progress {
        if event.sequence > sequence {
            return Err(validation("progress event is newer than its checkpoint"));
        }
        if previous.is_some_and(|value| event.sequence <= value) {
            return Err(validation(
                "progress event sequence must be strictly increasing",
            ));
        }
        previous = Some(event.sequence);
    }
    Ok(())
}

fn validation(message: &'static str) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidInput, ErrorCategory::Validation, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_goal::{AcceptanceCriterion, AcceptanceCriterionId, EvidenceKind};
    use serde_json::json;

    fn goal() -> GoalContract {
        GoalContract::new(
            "ship a verified change",
            vec![
                AcceptanceCriterion::new(
                    AcceptanceCriterionId::parse("tests-pass").unwrap(),
                    "tests pass",
                    [EvidenceKind::Test],
                )
                .unwrap(),
            ],
        )
        .unwrap()
    }

    #[test]
    fn latest_checkpoint_survives_restart() {
        let directory = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(directory.path());
        let session = SessionId::new();
        for sequence in 1..=2 {
            let checkpoint = ExecutionCheckpoint::new(
                session.clone(),
                sequence,
                goal(),
                Vec::new(),
                vec![
                    ProgressEvent::new(sequence, ProgressKind::CheckpointCreated, "saved").unwrap(),
                ],
                json!({"turn": sequence}),
            )
            .unwrap();
            store.save(&checkpoint).unwrap();
        }
        let restored = CheckpointStore::new(directory.path())
            .load_latest(&session)
            .unwrap()
            .unwrap();
        assert_eq!(restored.sequence, 2);
        assert_eq!(restored.state, json!({"turn": 2}));
    }

    #[test]
    fn corrupted_checkpoint_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(directory.path());
        let session = SessionId::new();
        let checkpoint = ExecutionCheckpoint::new(
            session.clone(),
            1,
            goal(),
            Vec::new(),
            Vec::new(),
            json!({}),
        )
        .unwrap();
        let path = store.save(&checkpoint).unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["state"] = json!({"tampered": true});
        fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        assert!(store.load_latest(&session).is_err());
    }

    #[test]
    fn non_monotonic_progress_is_rejected() {
        let events = vec![
            ProgressEvent::new(2, ProgressKind::ToolFinished, "done").unwrap(),
            ProgressEvent::new(1, ProgressKind::CheckpointCreated, "saved").unwrap(),
        ];
        assert!(
            ExecutionCheckpoint::new(SessionId::new(), 2, goal(), Vec::new(), events, json!({}),)
                .is_err()
        );
    }
}
