use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::RecoveryAuditRecord;

const AUDIT_DIRECTORY: &str = ".medusa/recovery-audit";

#[derive(Debug, Error)]
pub enum RecoveryAuditStoreError {
    #[error("recovery audit record failed integrity verification")]
    InvalidRecord,
    #[error("recovery audit record already exists: {0}")]
    AlreadyExists(PathBuf),
    #[error("failed to persist recovery audit record: {0}")]
    Io(#[from] io::Error),
    #[error("failed to serialize recovery audit record: {0}")]
    Serialize(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct RecoveryAuditStore {
    root: PathBuf,
}

impl RecoveryAuditStore {
    #[must_use]
    pub fn for_repository(repo: impl AsRef<Path>) -> Self {
        Self {
            root: repo.as_ref().join(AUDIT_DIRECTORY),
        }
    }

    pub fn append(&self, record: &RecoveryAuditRecord) -> Result<PathBuf, RecoveryAuditStoreError> {
        if !record.verify() {
            return Err(RecoveryAuditStoreError::InvalidRecord);
        }

        fs::create_dir_all(&self.root)?;
        let file_name = file_name(record);
        let destination = self.root.join(file_name);
        if destination.exists() {
            return Err(RecoveryAuditStoreError::AlreadyExists(destination));
        }

        let temporary = self.root.join(format!(
            ".{}.tmp-{}",
            destination
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("recovery-audit"),
            std::process::id()
        ));
        let bytes = serde_json::to_vec_pretty(record)?;

        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);

        match fs::rename(&temporary, &destination) {
            Ok(()) => Ok(destination),
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                if destination.exists() {
                    Err(RecoveryAuditStoreError::AlreadyExists(destination))
                } else {
                    Err(RecoveryAuditStoreError::Io(error))
                }
            }
        }
    }

    pub fn read_verified(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<RecoveryAuditRecord, RecoveryAuditStoreError> {
        let bytes = fs::read(path)?;
        let record: RecoveryAuditRecord = serde_json::from_slice(&bytes)?;
        if record.verify() {
            Ok(record)
        } else {
            Err(RecoveryAuditStoreError::InvalidRecord)
        }
    }
}

fn file_name(record: &RecoveryAuditRecord) -> String {
    let session = sanitize_component(&record.session_id);
    let operation = match record.operation {
        crate::RecoveryOperation::Inspect => "inspect",
        crate::RecoveryOperation::Resume => "resume",
        crate::RecoveryOperation::RestoreCheckpoint => "restore-checkpoint",
        crate::RecoveryOperation::RetryVerification => "retry-verification",
        crate::RecoveryOperation::Abandon => "abandon",
    };
    format!(
        "{}-{session}-{operation}-{}.json",
        record.recorded_at_unix_ms,
        &record.evidence_fingerprint[..16]
    )
}

fn sanitize_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let trimmed = sanitized.trim_matches('-');
    if trimmed.is_empty() {
        "unknown-session".to_owned()
    } else {
        trimmed.chars().take(80).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AuthorizedRecoveryAction, RecoveryActionOutcome, RecoveryOperation,
        RecoveryPreflightEvidence, VerificationState,
    };
    use tempfile::tempdir;

    fn record() -> RecoveryAuditRecord {
        RecoveryAuditRecord::new(
            1_700_000_000_000,
            &AuthorizedRecoveryAction {
                session_id: "session/one".into(),
                operation: RecoveryOperation::Resume,
                checkpoint_id: None,
                confirmation_recorded: false,
                authorization_reason: "resume authorized".into(),
            },
            RecoveryPreflightEvidence {
                repository_fingerprint_before: "a".repeat(64),
                checkpoint_integrity_verified: true,
                repository_preconditions_verified: true,
                conflicting_uncommitted_paths: Vec::new(),
                unresolved_risks: Vec::new(),
            },
            RecoveryActionOutcome::Succeeded,
            Some("b".repeat(64)),
            VerificationState::Incomplete,
        )
    }

    #[test]
    fn append_is_atomic_and_round_trips_verified_records() {
        let repo = tempdir().expect("temporary repository");
        let store = RecoveryAuditStore::for_repository(repo.path());
        let record = record();

        let path = store.append(&record).expect("persist audit record");
        assert!(path.exists());
        assert!(!path.to_string_lossy().contains("session/one"));
        assert_eq!(store.read_verified(&path).unwrap(), record);
    }

    #[test]
    fn duplicate_evidence_is_never_overwritten() {
        let repo = tempdir().expect("temporary repository");
        let store = RecoveryAuditStore::for_repository(repo.path());
        let record = record();
        store.append(&record).expect("first append");

        assert!(matches!(
            store.append(&record),
            Err(RecoveryAuditStoreError::AlreadyExists(_))
        ));
    }

    #[test]
    fn tampered_records_are_rejected_before_persistence() {
        let repo = tempdir().expect("temporary repository");
        let store = RecoveryAuditStore::for_repository(repo.path());
        let mut record = record();
        record.authorization_reason.push_str(" altered");

        assert!(matches!(
            store.append(&record),
            Err(RecoveryAuditStoreError::InvalidRecord)
        ));
    }
}
