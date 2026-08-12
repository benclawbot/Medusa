use std::{fs, path::{Path, PathBuf}};

use crate::{RuntimeError, checkpoint_payload::RuntimeCheckpointPayload};

const PAYLOAD_DIRECTORY: &str = ".medusa/recovery-checkpoints";
const RESTORE_TRANSACTION_DIRECTORY: &str = ".medusa/restore-transactions";

/// Returns restore-transaction directories provably owned by one session.
///
/// Recovery payloads are the authority for the content-addressed transaction fingerprint. A
/// corrupt payload fails closed: session disposition must not guess ownership of repository backup
/// material and report deletion complete while an unclassified copy may remain.
pub(super) fn owned_transactions(
    repo: &Path,
    session_id: &str,
) -> Result<Vec<PathBuf>, RuntimeError> {
    validate_identifier(session_id)?;
    let payload_directory = repo.join(PAYLOAD_DIRECTORY).join(session_id);
    let entries = match fs::read_dir(&payload_directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(RuntimeError::agent(error)),
    };

    let mut transactions = Vec::new();
    for entry in entries {
        let entry = entry.map_err(RuntimeError::agent)?;
        if !entry.file_type().map_err(RuntimeError::agent)?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path).map_err(RuntimeError::agent)?;
        let payload: RuntimeCheckpointPayload =
            serde_json::from_slice(&bytes).map_err(RuntimeError::agent)?;
        payload.verify()?;
        if payload.session_id != session_id {
            return Err(RuntimeError::agent(format!(
                "checkpoint payload {} belongs to a different session",
                path.display()
            )));
        }
        let expected_name = format!("{}.json", payload.checkpoint_id);
        if path.file_name().and_then(|value| value.to_str()) != Some(expected_name.as_str()) {
            return Err(RuntimeError::agent(format!(
                "checkpoint payload {} is not named for its verified checkpoint",
                path.display()
            )));
        }
        let transaction = repo
            .join(RESTORE_TRANSACTION_DIRECTORY)
            .join(&payload.payload_fingerprint);
        if transaction.exists() {
            transactions.push(transaction);
        }
    }
    transactions.sort();
    transactions.dedup();
    Ok(transactions)
}

pub(super) fn remove_transactions(paths: &[PathBuf]) -> Result<(), RuntimeError> {
    for path in paths {
        match fs::remove_dir_all(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(RuntimeError::agent(error)),
        }
    }
    Ok(())
}

fn validate_identifier(value: &str) -> Result<(), RuntimeError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(RuntimeError::agent(
            "restore-transaction lifecycle session id is not path-safe",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint_payload::CheckpointFilePayload;

    fn payload(session_id: &str, checkpoint_id: &str, payload_fingerprint: &str) -> RuntimeCheckpointPayload {
        RuntimeCheckpointPayload {
            schema_version: 1,
            session_id: session_id.to_owned(),
            checkpoint_id: checkpoint_id.to_owned(),
            journal_cursor: 0,
            repository_fingerprint: "0".repeat(64),
            files: Vec::<CheckpointFilePayload>::new(),
            unresolved_risks: Vec::new(),
            payload_fingerprint: payload_fingerprint.to_owned(),
        }
    }

    #[test]
    fn missing_payload_directory_has_no_owned_transactions() {
        let repository = tempfile::tempdir().expect("repository");
        assert!(
            owned_transactions(repository.path(), "ses-safe")
                .expect("classify")
                .is_empty()
        );
    }

    #[test]
    fn corrupt_payload_fails_closed() {
        let repository = tempfile::tempdir().expect("repository");
        let directory = repository
            .path()
            .join(PAYLOAD_DIRECTORY)
            .join("ses-corrupt");
        fs::create_dir_all(&directory).expect("payload directory");
        fs::write(directory.join("bad.json"), b"not-json").expect("corrupt payload");
        assert!(owned_transactions(repository.path(), "ses-corrupt").is_err());
    }

    #[test]
    fn only_verified_session_payloads_can_classify_transactions() {
        // Constructing a fully verified payload requires repository fingerprints that are computed
        // by the production checkpoint path. This unit test guards the important negative property:
        // an arbitrary session/fingerprint mapping cannot bypass payload verification.
        let repository = tempfile::tempdir().expect("repository");
        let directory = repository
            .path()
            .join(PAYLOAD_DIRECTORY)
            .join("ses-unverified");
        fs::create_dir_all(&directory).expect("payload directory");
        let checkpoint = "1".repeat(64);
        let fingerprint = "2".repeat(64);
        fs::write(
            directory.join(format!("{checkpoint}.json")),
            serde_json::to_vec_pretty(&payload("ses-unverified", &checkpoint, &fingerprint))
                .expect("json"),
        )
        .expect("payload");
        assert!(owned_transactions(repository.path(), "ses-unverified").is_err());
    }
}
