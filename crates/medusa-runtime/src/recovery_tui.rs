use std::{
    collections::BTreeMap,
    fs, io,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use medusa_recovery_coordinator::{
    AuthorizedRecoveryAction, CheckpointPresentation, RecoveryActionExecutor,
    RecoveryActionRequest, RecoveryActionService, RecoveryExecutionOutcome,
    RecoveryExecutionReceipt, RecoveryOperation, RecoveryPreflightEvidence, RecoveryPreview,
    RecoveryView, RecoveryViewInput, VerificationState,
};
use serde::Deserialize;
use thiserror::Error;

const RECOVERY_DIRECTORY: &str = ".medusa/recovery";
const CHECKPOINT_PAYLOAD_DIRECTORY: &str = ".medusa/recovery-checkpoints";

#[derive(Debug, Deserialize)]
struct PersistedRecoveryRecord {
    session_id: String,
    last_durable_step: String,
    interrupted_operation: Option<String>,
    current_repository_fingerprint: String,
    verification: VerificationState,
    approvals_must_be_reestablished: bool,
    containment_must_be_reestablished: bool,
    checkpoints: Vec<CheckpointPresentation>,
    selected_preview: Option<RecoveryPreview>,
}

#[derive(Debug, Deserialize)]
struct PersistedCheckpointPayload {
    checkpoint_id: String,
    files: Vec<CheckpointFilePayload>,
}

#[derive(Debug, Deserialize)]
struct CheckpointFilePayload {
    path: String,
    content: Option<String>,
}

#[derive(Debug, Error)]
enum RuntimeRecoveryError {
    #[error("checkpoint payload is unavailable: {0}")]
    MissingPayload(PathBuf),
    #[error("checkpoint payload is invalid: {0}")]
    InvalidPayload(String),
    #[error("checkpoint restore I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("checkpoint payload could not be decoded: {0}")]
    Decode(#[from] serde_json::Error),
}

struct RuntimeRecoveryExecutor {
    repository_fingerprint: String,
    checkpoint_fingerprints: BTreeMap<String, String>,
}

impl RecoveryActionExecutor for RuntimeRecoveryExecutor {
    type Error = RuntimeRecoveryError;

    fn execute(
        &mut self,
        repository: &Path,
        action: &AuthorizedRecoveryAction,
    ) -> Result<RecoveryExecutionOutcome, Self::Error> {
        let outcome = match action.operation {
            RecoveryOperation::Inspect => RecoveryExecutionOutcome::succeeded(
                self.repository_fingerprint.clone(),
                VerificationState::Unknown,
            ),
            RecoveryOperation::Resume => RecoveryExecutionOutcome::succeeded(
                self.repository_fingerprint.clone(),
                VerificationState::Incomplete,
            ),
            RecoveryOperation::RetryVerification => RecoveryExecutionOutcome::succeeded(
                self.repository_fingerprint.clone(),
                VerificationState::Incomplete,
            ),
            RecoveryOperation::Abandon => {
                RecoveryExecutionOutcome::cancelled(VerificationState::Incomplete)
            }
            RecoveryOperation::RestoreCheckpoint => {
                let checkpoint_id = action.checkpoint_id.as_deref().ok_or_else(|| {
                    RuntimeRecoveryError::InvalidPayload("missing checkpoint id".to_owned())
                })?;
                restore_checkpoint(repository, &action.session_id, checkpoint_id)?;
                let fingerprint = self
                    .checkpoint_fingerprints
                    .get(checkpoint_id)
                    .cloned()
                    .ok_or_else(|| {
                        RuntimeRecoveryError::InvalidPayload(
                            "checkpoint metadata is unavailable".to_owned(),
                        )
                    })?;
                RecoveryExecutionOutcome::succeeded(fingerprint, VerificationState::Incomplete)
            }
        };
        Ok(outcome)
    }
}

pub(crate) fn execute_view_action(
    repo: &Path,
    view: &RecoveryView,
    request: &RecoveryActionRequest,
    preflight: RecoveryPreflightEvidence,
) -> Result<RecoveryExecutionReceipt, String> {
    let executor = RuntimeRecoveryExecutor {
        repository_fingerprint: preflight.repository_fingerprint_before.clone(),
        checkpoint_fingerprints: view
            .checkpoints
            .iter()
            .map(|checkpoint| {
                (
                    checkpoint.id.clone(),
                    checkpoint.repository_fingerprint.clone(),
                )
            })
            .collect(),
    };
    RecoveryActionService::new(executor)
        .execute_and_audit(repo, view, request, preflight, now_unix_ms())
        .map_err(|error| error.to_string())
}

pub(crate) fn execute_command(
    repo: &Path,
    input: Option<&str>,
) -> Result<Option<RecoveryExecutionReceipt>, String> {
    let Some(input) = input.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let mut parts = input.split_whitespace();
    let operation = match parts.next().unwrap_or_default().to_ascii_lowercase().as_str() {
        "inspect" => RecoveryOperation::Inspect,
        "resume" => RecoveryOperation::Resume,
        "verify" | "retry-verification" => RecoveryOperation::RetryVerification,
        "abandon" => RecoveryOperation::Abandon,
        "restore" => RecoveryOperation::RestoreCheckpoint,
        _ => {
            return Err(
                "usage: /recovery inspect|resume|verify|abandon or /recovery restore <checkpoint> [--confirm]"
                    .to_owned(),
            )
        }
    };
    let checkpoint_id = if operation == RecoveryOperation::RestoreCheckpoint {
        parts.next().map(str::to_owned)
    } else {
        None
    };
    let confirmed = parts.any(|part| part == "--confirm");
    let view = discover(repo)?;
    let selected = checkpoint_id.as_deref().and_then(|id| {
        view.checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id == id)
    });
    let preview = view.selected_preview.as_ref().filter(|preview| {
        checkpoint_id
            .as_deref()
            .is_none_or(|id| preview.checkpoint_id == id)
    });
    let conflicting_uncommitted_paths = preview
        .map(|preview| {
            preview
                .files
                .iter()
                .filter(|file| file.would_overwrite_uncommitted_work)
                .map(|file| file.path.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let unresolved_risks = preview
        .map(|preview| preview.unresolved_risks.clone())
        .unwrap_or_default();
    let request = RecoveryActionRequest {
        session_id: view.session_id.clone(),
        operation,
        checkpoint_id,
        confirmed_destructive_effects: confirmed,
    };
    let preflight = RecoveryPreflightEvidence {
        repository_fingerprint_before: view.current_repository_fingerprint.clone(),
        checkpoint_integrity_verified: selected
            .is_none_or(|checkpoint| checkpoint.integrity_verified),
        repository_preconditions_verified: preview.is_none_or(|preview| {
            preview.repository_matches_checkpoint_base
                && conflicting_uncommitted_paths.is_empty()
                && unresolved_risks.is_empty()
        }),
        conflicting_uncommitted_paths,
        unresolved_risks,
    };
    execute_view_action(repo, &view, &request, preflight).map(Some)
}

fn discover(repo: &Path) -> Result<RecoveryView, String> {
    let directory = repo.join(RECOVERY_DIRECTORY);
    let mut paths = fs::read_dir(&directory)
        .map_err(|_| "no recoverable session is available".to_owned())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();
    let path = paths
        .into_iter()
        .next()
        .ok_or_else(|| "no recoverable session is available".to_owned())?;
    let record: PersistedRecoveryRecord = serde_json::from_slice(
        &fs::read(&path).map_err(|error| format!("recovery record could not be read: {error}"))?,
    )
    .map_err(|error| format!("recovery record is corrupt: {error}"))?;
    Ok(RecoveryView::build(RecoveryViewInput {
        session_id: record.session_id,
        last_durable_step: record.last_durable_step,
        interrupted_operation: record.interrupted_operation,
        current_repository_fingerprint: record.current_repository_fingerprint,
        verification: record.verification,
        approvals_must_be_reestablished: record.approvals_must_be_reestablished,
        containment_must_be_reestablished: record.containment_must_be_reestablished,
        checkpoints: record.checkpoints,
        selected_preview: record.selected_preview,
        source_corrupt: false,
    }))
}

fn restore_checkpoint(
    repository: &Path,
    session_id: &str,
    checkpoint_id: &str,
) -> Result<(), RuntimeRecoveryError> {
    validate_identifier(session_id)?;
    validate_identifier(checkpoint_id)?;
    let payload_path = repository
        .join(CHECKPOINT_PAYLOAD_DIRECTORY)
        .join(session_id)
        .join(format!("{checkpoint_id}.json"));
    if !payload_path.is_file() {
        return Err(RuntimeRecoveryError::MissingPayload(payload_path));
    }
    let payload: PersistedCheckpointPayload = serde_json::from_slice(&fs::read(&payload_path)?)?;
    if payload.checkpoint_id != checkpoint_id {
        return Err(RuntimeRecoveryError::InvalidPayload(
            "payload checkpoint id does not match the selected checkpoint".to_owned(),
        ));
    }
    for file in payload.files {
        let destination = repository.join(safe_relative_path(&file.path)?);
        match file.content {
            Some(content) => {
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent)?;
                }
                let temporary = destination.with_extension("medusa-restore-tmp");
                fs::write(&temporary, content.as_bytes())?;
                if destination.exists() {
                    fs::remove_file(&destination)?;
                }
                fs::rename(temporary, destination)?;
            }
            None if destination.exists() => fs::remove_file(destination)?,
            None => {}
        }
    }
    Ok(())
}

fn validate_identifier(value: &str) -> Result<(), RuntimeRecoveryError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(RuntimeRecoveryError::InvalidPayload(
            "session and checkpoint identifiers must be path-safe".to_owned(),
        ));
    }
    Ok(())
}

fn safe_relative_path(value: &str) -> Result<PathBuf, RuntimeRecoveryError> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(RuntimeRecoveryError::InvalidPayload(format!(
            "unsafe repository path: {value}"
        )));
    }
    Ok(path.to_path_buf())
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn restores_files_and_deletions() {
        let repo = tempdir().expect("temporary repository");
        fs::create_dir_all(repo.path().join("src")).expect("create src");
        fs::write(repo.path().join("src/lib.rs"), "current").expect("write current");
        fs::write(repo.path().join("obsolete.txt"), "remove").expect("write obsolete");
        let payload_dir = repo
            .path()
            .join(CHECKPOINT_PAYLOAD_DIRECTORY)
            .join("session-1");
        fs::create_dir_all(&payload_dir).expect("create payload directory");
        fs::write(
            payload_dir.join("checkpoint-1.json"),
            serde_json::to_vec(&serde_json::json!({
                "checkpoint_id": "checkpoint-1",
                "files": [
                    {"path": "src/lib.rs", "content": "restored"},
                    {"path": "obsolete.txt", "content": null}
                ]
            }))
            .expect("serialize payload"),
        )
        .expect("write payload");

        restore_checkpoint(repo.path(), "session-1", "checkpoint-1").expect("restore checkpoint");
        assert_eq!(
            fs::read_to_string(repo.path().join("src/lib.rs")).expect("read restored"),
            "restored"
        );
        assert!(!repo.path().join("obsolete.txt").exists());
    }

    #[test]
    fn rejects_path_traversal() {
        let repo = tempdir().expect("temporary repository");
        let payload_dir = repo
            .path()
            .join(CHECKPOINT_PAYLOAD_DIRECTORY)
            .join("session-1");
        fs::create_dir_all(&payload_dir).expect("create payload directory");
        fs::write(
            payload_dir.join("checkpoint-1.json"),
            serde_json::to_vec(&serde_json::json!({
                "checkpoint_id": "checkpoint-1",
                "files": [{"path": "../escape", "content": "bad"}]
            }))
            .expect("serialize payload"),
        )
        .expect("write payload");
        assert!(restore_checkpoint(repo.path(), "session-1", "checkpoint-1").is_err());
    }
}
