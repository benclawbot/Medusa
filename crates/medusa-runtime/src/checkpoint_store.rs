//! Deterministic, journal-anchored runtime checkpoint artifacts.
//!
//! Checkpoints are derived from the canonical session journal and never become a competing source
//! of execution truth. A checkpoint artifact must exist and verify before its corresponding
//! `CheckpointCreated` event is appended to the journal.

#[cfg(unix)]
use std::fs::File;

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use medusa_execution_checkpoint::ExecutionCheckpoint;
use medusa_protocol::EventPayload;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{RuntimeController, RuntimeError, execution_history};

#[path = "restore_transaction_lifecycle.rs"]
mod restore_transaction_lifecycle;

const CHECKPOINT_SCHEMA_VERSION: u32 = 1;
const CHECKPOINT_DIRECTORY: &str = ".medusa/checkpoints";
const RECOVERY_CHECKPOINT_DIRECTORY: &str = ".medusa/recovery-checkpoints";
const CONTINUITY_DIRECTORY: &str = ".medusa/continuity";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCheckpointRecord {
    pub schema_version: u32,
    pub session_id: String,
    pub journal_cursor: u64,
    pub journal_fingerprint: String,
    pub checkpoint: ExecutionCheckpoint,
    pub record_fingerprint: String,
}

impl RuntimeCheckpointRecord {
    fn new(
        session_id: String,
        journal_cursor: u64,
        journal_fingerprint: String,
        checkpoint: ExecutionCheckpoint,
    ) -> Result<Self, RuntimeError> {
        let mut record = Self {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            session_id,
            journal_cursor,
            journal_fingerprint,
            checkpoint,
            record_fingerprint: String::new(),
        };
        record.validate_fields()?;
        record.record_fingerprint = record.calculate_fingerprint()?;
        Ok(record)
    }

    pub fn verify(&self) -> Result<(), RuntimeError> {
        self.validate_fields()?;
        let expected = self.calculate_fingerprint()?;
        if self.record_fingerprint != expected {
            return Err(RuntimeError::agent(
                "runtime checkpoint record fingerprint does not match its contents",
            ));
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), RuntimeError> {
        if self.schema_version != CHECKPOINT_SCHEMA_VERSION {
            return Err(RuntimeError::agent(format!(
                "unsupported runtime checkpoint schema version {}",
                self.schema_version
            )));
        }
        validate_session_id(&self.session_id)?;
        validate_sha256(&self.journal_fingerprint, "journal fingerprint")?;
        self.checkpoint.verify().map_err(RuntimeError::agent)?;
        if self.checkpoint.execution_id != self.session_id {
            return Err(RuntimeError::agent(
                "runtime checkpoint belongs to a different session",
            ));
        }
        if self.checkpoint.sequence != self.journal_cursor {
            return Err(RuntimeError::agent(
                "runtime checkpoint sequence does not match its journal cursor",
            ));
        }
        if !self.record_fingerprint.is_empty() {
            validate_sha256(&self.record_fingerprint, "record fingerprint")?;
        }
        Ok(())
    }

    fn calculate_fingerprint(&self) -> Result<String, RuntimeError> {
        digest(&(
            self.schema_version,
            &self.session_id,
            self.journal_cursor,
            &self.journal_fingerprint,
            &self.checkpoint,
        ))
    }
}

impl RuntimeController {
    /// Returns the latest verified persisted checkpoint for a session.
    pub fn latest_checkpoint(
        &self,
        session_id: &str,
    ) -> Result<Option<RuntimeCheckpointRecord>, RuntimeError> {
        latest(&self.repo, session_id)
    }

    /// Returns every verified persisted checkpoint in journal order.
    pub fn checkpoints(
        &self,
        session_id: &str,
    ) -> Result<Vec<RuntimeCheckpointRecord>, RuntimeError> {
        list(&self.repo, session_id)
    }

    /// Disposes a completed session across the canonical agent authority and runtime projections.
    pub fn dispose_completed_session(&self, session_id: &str) -> Result<(), RuntimeError> {
        dispose_completed_session(&self.repo, session_id)
    }
}

/// Disposes a completed session and removes runtime-owned checkpoint/recovery/continuity copies.
///
/// Runtime restore transactions are classified from verified checkpoint payloads before the agent
/// writes its fail-closed journal tombstone. Corrupt ownership metadata therefore blocks the
/// deletion claim instead of leaving unclassified repository backups behind. All removals are
/// idempotent so an interrupted operation can be retried without resurrecting canonical state.
pub fn dispose_completed_session(repo: &Path, session_id: &str) -> Result<(), RuntimeError> {
    validate_session_id(session_id)?;
    let restore_transactions =
        restore_transaction_lifecycle::owned_transactions(repo, session_id)?;
    medusa_agent::session_browser::dispose_completed_session(repo, session_id)
        .map_err(RuntimeError::agent)?;
    restore_transaction_lifecycle::remove_transactions(&restore_transactions)?;
    remove_dir_if_present(&checkpoint_directory(repo, session_id))?;
    remove_dir_if_present(&repo.join(RECOVERY_CHECKPOINT_DIRECTORY).join(session_id))?;
    remove_file_if_present(
        &repo
            .join(CONTINUITY_DIRECTORY)
            .join(format!("{session_id}.json")),
    )?;
    Ok(())
}

pub(crate) fn is_checkpoint_boundary(payload: &EventPayload) -> bool {
    matches!(
        payload,
        EventPayload::QuestionRequested { .. }
            | EventPayload::ApprovalRequested { .. }
            | EventPayload::ApprovalDecisionRecorded { .. }
            | EventPayload::FileTransactionCommitted { .. }
            | EventPayload::IntegrationReceiptRecorded { .. }
            | EventPayload::RecoveryActionCompleted { .. }
            | EventPayload::VerificationCompleted { .. }
            | EventPayload::CancellationCompleted
            | EventPayload::RuntimeTurnFinished
            | EventPayload::RuntimeFailed { .. }
            | EventPayload::SessionReset { .. }
            | EventPayload::SessionPaused { .. }
            | EventPayload::SessionCompleted { .. }
            | EventPayload::SessionFailed { .. }
    )
}

pub(crate) fn materialize(
    repo: &Path,
    session_id: &str,
) -> Result<RuntimeCheckpointRecord, RuntimeError> {
    validate_session_id(session_id)?;
    let health = execution_history::inspect(repo, session_id)?;
    let record = RuntimeCheckpointRecord::new(
        health.session_id,
        health.journal_cursor,
        health.journal_fingerprint,
        health.checkpoint,
    )?;
    persist(repo, &record)?;
    crate::checkpoint_payload::materialize(repo, &record)?;
    Ok(record)
}

pub fn latest(
    repo: &Path,
    session_id: &str,
) -> Result<Option<RuntimeCheckpointRecord>, RuntimeError> {
    Ok(list(repo, session_id)?.into_iter().last())
}

pub fn list(repo: &Path, session_id: &str) -> Result<Vec<RuntimeCheckpointRecord>, RuntimeError> {
    validate_session_id(session_id)?;
    let directory = checkpoint_directory(repo, session_id);
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(&directory).map_err(RuntimeError::agent)? {
        let entry = entry.map_err(RuntimeError::agent)?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let record = load_record(&path)?;
        if record.session_id != session_id {
            return Err(RuntimeError::agent(format!(
                "checkpoint artifact {} belongs to session {}",
                path.display(),
                record.session_id
            )));
        }
        let expected_name = format!("{}.json", record.checkpoint.fingerprint);
        if path.file_name().and_then(|value| value.to_str()) != Some(expected_name.as_str()) {
            return Err(RuntimeError::agent(format!(
                "checkpoint artifact {} is not named for its verified fingerprint",
                path.display()
            )));
        }
        records.push(record);
    }
    records.sort_by(|left, right| {
        left.journal_cursor
            .cmp(&right.journal_cursor)
            .then_with(|| {
                left.checkpoint
                    .fingerprint
                    .cmp(&right.checkpoint.fingerprint)
            })
    });
    for pair in records.windows(2) {
        if pair[0].journal_cursor >= pair[1].journal_cursor {
            return Err(RuntimeError::agent(
                "persisted runtime checkpoints are not strictly monotonic",
            ));
        }
    }
    Ok(records)
}

fn persist(repo: &Path, record: &RuntimeCheckpointRecord) -> Result<(), RuntimeError> {
    record.verify()?;
    let directory = checkpoint_directory(repo, &record.session_id);
    fs::create_dir_all(&directory).map_err(RuntimeError::agent)?;
    let destination = directory.join(format!("{}.json", record.checkpoint.fingerprint));
    if destination.exists() {
        let existing = load_record(&destination)?;
        if existing == *record {
            return Ok(());
        }
        return Err(RuntimeError::agent(format!(
            "checkpoint fingerprint {} is already bound to conflicting content",
            record.checkpoint.fingerprint
        )));
    }

    let temporary = temporary_path(&directory, &record.checkpoint.fingerprint);
    let bytes = serde_json::to_vec_pretty(record).map_err(RuntimeError::agent)?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(RuntimeError::agent)?;
    if let Err(error) = (|| -> std::io::Result<()> {
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &destination)?;
        sync_parent(&directory)?;
        Ok(())
    })() {
        let _ = fs::remove_file(&temporary);
        return Err(RuntimeError::agent(error));
    }
    Ok(())
}

fn load_record(path: &Path) -> Result<RuntimeCheckpointRecord, RuntimeError> {
    let bytes = fs::read(path).map_err(RuntimeError::agent)?;
    let record: RuntimeCheckpointRecord =
        serde_json::from_slice(&bytes).map_err(RuntimeError::agent)?;
    record.verify()?;
    Ok(record)
}

fn checkpoint_directory(repo: &Path, session_id: &str) -> PathBuf {
    repo.join(CHECKPOINT_DIRECTORY).join(session_id)
}

fn temporary_path(directory: &Path, fingerprint: &str) -> PathBuf {
    directory.join(format!(
        ".{fingerprint}.{}.{}.tmp",
        std::process::id(),
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    ))
}

fn remove_dir_if_present(path: &Path) -> Result<(), RuntimeError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RuntimeError::agent(error)),
    }
}

fn remove_file_if_present(path: &Path) -> Result<(), RuntimeError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RuntimeError::agent(error)),
    }
}

#[cfg(unix)]
fn sync_parent(directory: &Path) -> std::io::Result<()> {
    File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_: &Path) -> std::io::Result<()> {
    Ok(())
}

fn validate_session_id(session_id: &str) -> Result<(), RuntimeError> {
    if session_id.is_empty()
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(RuntimeError::agent(
            "runtime checkpoint session id is not a safe path component",
        ));
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<(), RuntimeError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(RuntimeError::agent(format!(
            "runtime checkpoint {label} is not a lowercase SHA-256 digest"
        )));
    }
    Ok(())
}

fn digest<T: Serialize>(value: &T) -> Result<String, RuntimeError> {
    let bytes = serde_json::to_vec(value).map_err(RuntimeError::agent)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use medusa_agent::{
        AgentEngine, persist_session, record_session_event, session_browser::load_session,
    };
    use medusa_config::Config;
    use medusa_core::MedusaResult;
    use medusa_protocol::{Actor, EventPayload};
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};
    use medusa_session_continuity::ContinuityStore;

    use super::*;

    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("session creation does not call the provider")
        }
    }

    fn session(repo: &Path) -> medusa_agent::AgentSession {
        AgentEngine::new(UnusedProvider, Config::default())
            .create_session(repo, "Checkpoint runtime state".to_owned())
            .expect("session")
    }

    #[test]
    fn materialization_is_deterministic_and_idempotent() {
        let repository = tempfile::tempdir().expect("repository");
        let session = session(repository.path());
        let first = materialize(repository.path(), session.id.as_str()).expect("checkpoint");
        let second = materialize(repository.path(), session.id.as_str()).expect("checkpoint");
        assert_eq!(first, second);
        assert_eq!(
            list(repository.path(), session.id.as_str()).unwrap(),
            vec![first]
        );
    }

    #[test]
    fn safe_boundary_persists_before_checkpoint_event() {
        let repository = tempfile::tempdir().expect("repository");
        let session = session(repository.path());
        crate::record_controller_event(
            repository.path(),
            session.id.as_str(),
            Actor::Coordinator,
            EventPayload::VerificationCompleted {
                passed: true,
                evidence: vec!["verified".to_owned()],
            },
        )
        .expect("boundary event");

        let loaded = load_session(repository.path(), session.id.as_str()).expect("session");
        let EventPayload::CheckpointCreated { checkpoint_id } =
            &loaded.events.last().expect("checkpoint event").payload
        else {
            panic!("expected checkpoint event");
        };
        let record = latest(repository.path(), session.id.as_str())
            .expect("latest")
            .expect("checkpoint");
        assert_eq!(checkpoint_id, &record.checkpoint.fingerprint);
        assert_eq!(record.journal_cursor + 1, loaded.events.len() as u64);
        assert_eq!(record.checkpoint.sequence, record.journal_cursor);
    }

    #[test]
    fn tampered_artifact_fails_closed() {
        let repository = tempfile::tempdir().expect("repository");
        let session = session(repository.path());
        let record = materialize(repository.path(), session.id.as_str()).expect("checkpoint");
        let path = checkpoint_directory(repository.path(), session.id.as_str())
            .join(format!("{}.json", record.checkpoint.fingerprint));
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("artifact")).expect("json");
        value["journal_cursor"] = serde_json::json!(99);
        fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).expect("tamper");
        assert!(latest(repository.path(), session.id.as_str()).is_err());
    }

    #[test]
    fn only_safe_boundaries_trigger_materialization() {
        assert!(is_checkpoint_boundary(&EventPayload::RuntimeTurnFinished));
        assert!(is_checkpoint_boundary(&EventPayload::CancellationCompleted));
        assert!(!is_checkpoint_boundary(
            &EventPayload::ModelRequestStarted {
                provider: "provider".to_owned(),
                model: "model".to_owned(),
                request_id: None,
                request_fingerprint: None,
                manifest_ref: None,
                attempt_ordinal: 0,
                parent_request_id: None,
            }
        ));
        assert!(!is_checkpoint_boundary(&EventPayload::CheckpointCreated {
            checkpoint_id: "checkpoint".to_owned(),
        }));
    }

    #[test]
    fn independently_recorded_boundary_can_be_materialized() {
        let repository = tempfile::tempdir().expect("repository");
        let mut session = session(repository.path());
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::RuntimeTurnFinished,
        )
        .expect("turn boundary");
        let record = materialize(repository.path(), session.id.as_str()).expect("checkpoint");
        assert_eq!(record.journal_cursor, 2);
    }

    #[test]
    fn completed_session_disposition_removes_runtime_projections() {
        let repository = tempfile::tempdir().expect("repository");
        let mut session = session(repository.path());
        let record = materialize(repository.path(), session.id.as_str()).expect("checkpoint");
        let payload_path = repository
            .path()
            .join(RECOVERY_CHECKPOINT_DIRECTORY)
            .join(session.id.as_str())
            .join(format!("{}.json", record.checkpoint.fingerprint));
        let payload: crate::checkpoint_payload::RuntimeCheckpointPayload =
            serde_json::from_slice(&fs::read(&payload_path).expect("payload")).expect("payload json");
        let restore_transaction = repository
            .path()
            .join(".medusa/restore-transactions")
            .join(&payload.payload_fingerprint);
        fs::create_dir_all(restore_transaction.join("backups")).expect("restore transaction");
        fs::write(
            restore_transaction.join("backups/0"),
            b"PRIVATE_RESTORE_BACKUP_MARKER",
        )
        .expect("restore backup");
        let continuity_path = repository
            .path()
            .join(CONTINUITY_DIRECTORY)
            .join(format!("{}.json", session.id));
        ContinuityStore::new(&continuity_path)
            .create(session.id.to_string())
            .expect("continuity");
        session.completed = true;
        persist_session(&session).expect("completed session");

        dispose_completed_session(repository.path(), session.id.as_str()).expect("dispose");

        assert!(!checkpoint_directory(repository.path(), session.id.as_str()).exists());
        assert!(
            !repository
                .path()
                .join(RECOVERY_CHECKPOINT_DIRECTORY)
                .join(session.id.as_str())
                .exists()
        );
        assert!(!restore_transaction.exists());
        assert!(!continuity_path.exists());
        assert!(load_session(repository.path(), session.id.as_str()).is_err());
        assert!(list(repository.path(), session.id.as_str()).unwrap().is_empty());

        dispose_completed_session(repository.path(), session.id.as_str()).expect("retry");
    }
}
