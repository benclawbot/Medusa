use std::{
    convert::Infallible,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use medusa_recovery_coordinator::{
    AuthorizedRecoveryAction, CheckpointPresentation, RecoveryActionExecutor,
    RecoveryActionRequest, RecoveryActionService, RecoveryExecutionOutcome,
    RecoveryExecutionReceipt, RecoveryOperation, RecoveryPreflightEvidence, RecoveryPreview,
    RecoveryView, RecoveryViewInput, VerificationState,
};
use serde::Deserialize;

use super::RuntimeEvent;

const RECOVERY_DIRECTORY: &str = ".medusa/recovery";

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

struct RuntimeRecoveryExecutor {
    repository_fingerprint: String,
}

impl RecoveryActionExecutor for RuntimeRecoveryExecutor {
    type Error = Infallible;

    fn execute(
        &mut self,
        _repository: &Path,
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
            RecoveryOperation::RestoreCheckpoint => RecoveryExecutionOutcome::failed_closed(
                "checkpoint payload restoration is not available in the runtime executor",
                Some(self.repository_fingerprint.clone()),
                VerificationState::Incomplete,
            ),
        };
        Ok(outcome)
    }
}

pub(crate) fn action_context(
    repo: &Path,
    request: &RecoveryActionRequest,
) -> Result<(RecoveryView, RecoveryPreflightEvidence), String> {
    let view = discover(repo)
        .into_iter()
        .find(|view| view.session_id == request.session_id)
        .ok_or_else(|| {
            format!(
                "recovery session {} is no longer available; refresh recovery state",
                request.session_id
            )
        })?;

    let selected_checkpoint = request.checkpoint_id.as_deref().and_then(|checkpoint_id| {
        view.checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id == checkpoint_id)
    });
    let checkpoint_integrity_verified = match request.operation {
        RecoveryOperation::RestoreCheckpoint => {
            selected_checkpoint.is_some_and(|checkpoint| checkpoint.integrity_verified)
        }
        _ => true,
    };
    let matching_preview = request.checkpoint_id.as_deref().and_then(|checkpoint_id| {
        view.selected_preview
            .as_ref()
            .filter(|preview| preview.checkpoint_id == checkpoint_id)
    });
    let conflicting_uncommitted_paths = matching_preview
        .map(|preview| {
            preview
                .files
                .iter()
                .filter(|file| file.would_overwrite_uncommitted_work)
                .map(|file| file.path.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut unresolved_risks = matching_preview
        .map(|preview| preview.unresolved_risks.clone())
        .unwrap_or_default();
    if matches!(request.operation, RecoveryOperation::RestoreCheckpoint)
        && matching_preview.is_none()
    {
        unresolved_risks.push(
            "No authoritative preview exists for the selected checkpoint; regenerate it."
                .to_owned(),
        );
    }
    if matches!(request.operation, RecoveryOperation::RestoreCheckpoint)
        && !checkpoint_integrity_verified
    {
        unresolved_risks.push(
            "The selected checkpoint is missing or failed integrity verification.".to_owned(),
        );
    }
    let repository_preconditions_verified = match request.operation {
        RecoveryOperation::RestoreCheckpoint => matching_preview.is_some_and(|preview| {
            preview.repository_matches_checkpoint_base
                && checkpoint_integrity_verified
                && conflicting_uncommitted_paths.is_empty()
                && unresolved_risks.is_empty()
        }),
        _ => !view.current_repository_fingerprint.is_empty(),
    };
    let evidence = RecoveryPreflightEvidence {
        repository_fingerprint_before: view.current_repository_fingerprint.clone(),
        checkpoint_integrity_verified,
        repository_preconditions_verified,
        conflicting_uncommitted_paths,
        unresolved_risks,
    };
    Ok((view, evidence))
}

pub(crate) fn execute_action(
    repo: &Path,
    view: &RecoveryView,
    request: &RecoveryActionRequest,
    preflight: RecoveryPreflightEvidence,
) -> Result<RecoveryExecutionReceipt, String> {
    let executor = RuntimeRecoveryExecutor {
        repository_fingerprint: preflight.repository_fingerprint_before.clone(),
    };
    let mut service = RecoveryActionService::new(executor);
    service
        .execute_and_audit(repo, view, request, preflight, now_unix_ms())
        .map_err(|error| error.to_string())
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

pub(crate) fn startup_events(repo: &Path) -> Vec<RuntimeEvent> {
    discover(repo)
        .into_iter()
        .map(RuntimeEvent::RecoveryAvailable)
        .collect()
}

pub(crate) fn discover(repo: &Path) -> Vec<RecoveryView> {
    let directory = repo.join(RECOVERY_DIRECTORY);
    let Ok(entries) = fs::read_dir(&directory) else {
        return Vec::new();
    };

    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();

    paths
        .into_iter()
        .map(|path| match fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<PersistedRecoveryRecord>(&contents) {
                Ok(record) => RecoveryView::build(RecoveryViewInput {
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
                }),
                Err(_) => corrupt_view(&path),
            },
            Err(_) => corrupt_view(&path),
        })
        .collect()
}

fn corrupt_view(path: &Path) -> RecoveryView {
    let session_id = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("unknown-recovery-record")
        .to_owned();
    RecoveryView::build(RecoveryViewInput {
        session_id,
        last_durable_step: "Unknown because the recovery record could not be read".to_owned(),
        interrupted_operation: None,
        current_repository_fingerprint: String::new(),
        verification: VerificationState::Unknown,
        approvals_must_be_reestablished: true,
        containment_must_be_reestablished: true,
        checkpoints: Vec::new(),
        selected_preview: None,
        source_corrupt: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_recovery_coordinator::{CheckpointPresentation, RecoveryHealth};
    use tempfile::tempdir;

    fn checkpoint() -> CheckpointPresentation {
        CheckpointPresentation {
            id: "checkpoint-1".to_owned(),
            sequence: 1,
            created_at_unix_ms: 1_700_000_000_000,
            task_step: "implement".to_owned(),
            reason: "durable progress".to_owned(),
            repository_fingerprint: "a".repeat(64),
            verification: VerificationState::Incomplete,
            provenance: "execution-checkpoint/v1".to_owned(),
            integrity_verified: true,
        }
    }

    fn write_record(repo: &Path, preview: Option<RecoveryPreview>) {
        let directory = repo.join(RECOVERY_DIRECTORY);
        fs::create_dir_all(&directory).expect("create recovery directory");
        let record = serde_json::json!({
            "session_id": "session-a",
            "last_durable_step": "implement",
            "interrupted_operation": "cargo test",
            "current_repository_fingerprint": "b".repeat(64),
            "verification": "Incomplete",
            "approvals_must_be_reestablished": true,
            "containment_must_be_reestablished": true,
            "checkpoints": [checkpoint()],
            "selected_preview": preview
        });
        fs::write(
            directory.join("a.json"),
            serde_json::to_vec_pretty(&record).expect("serialize recovery record"),
        )
        .expect("write recovery record");
    }

    #[test]
    fn discovers_recovery_records_in_stable_filename_order() {
        let repo = tempdir().expect("temporary repository");
        let directory = repo.path().join(RECOVERY_DIRECTORY);
        fs::create_dir_all(&directory).expect("create recovery directory");
        for (name, session_id) in [("b.json", "session-b"), ("a.json", "session-a")] {
            let record = serde_json::json!({
                "session_id": session_id,
                "last_durable_step": "implement",
                "interrupted_operation": "cargo test",
                "current_repository_fingerprint": "b".repeat(64),
                "verification": "Incomplete",
                "approvals_must_be_reestablished": true,
                "containment_must_be_reestablished": true,
                "checkpoints": [checkpoint()],
                "selected_preview": null
            });
            fs::write(
                directory.join(name),
                serde_json::to_vec_pretty(&record).expect("serialize recovery record"),
            )
            .expect("write recovery record");
        }

        let views = discover(repo.path());
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].session_id, "session-a");
        assert_eq!(views[1].session_id, "session-b");
        assert!(
            views
                .iter()
                .all(|view| view.approvals_must_be_reestablished)
        );
    }

    #[test]
    fn action_context_reloads_authoritative_view_and_fails_closed_without_preview() {
        let repo = tempdir().expect("temporary repository");
        write_record(repo.path(), None);
        let request = RecoveryActionRequest {
            session_id: "session-a".to_owned(),
            operation: RecoveryOperation::RestoreCheckpoint,
            checkpoint_id: Some("checkpoint-1".to_owned()),
            confirmed_destructive_effects: true,
        };
        let (view, evidence) = action_context(repo.path(), &request).expect("action context");
        assert_eq!(view.session_id, "session-a");
        assert!(!evidence.repository_preconditions_verified);
        assert!(!evidence.unresolved_risks.is_empty());
    }

    #[test]
    fn missing_or_stale_session_is_rejected() {
        let repo = tempdir().expect("temporary repository");
        let request = RecoveryActionRequest {
            session_id: "missing".to_owned(),
            operation: RecoveryOperation::Inspect,
            checkpoint_id: None,
            confirmed_destructive_effects: false,
        };
        assert!(action_context(repo.path(), &request).is_err());
    }

    #[test]
    fn corrupt_records_are_visible_and_fail_closed() {
        let repo = tempdir().expect("temporary repository");
        let directory = repo.path().join(RECOVERY_DIRECTORY);
        fs::create_dir_all(&directory).expect("create recovery directory");
        fs::write(directory.join("broken.json"), b"{not json")
            .expect("write corrupt recovery record");

        let views = discover(repo.path());
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].session_id, "broken");
        assert_eq!(views[0].health, RecoveryHealth::Corrupt);
        assert!(views[0].containment_must_be_reestablished);
    }

    #[test]
    fn missing_recovery_directory_is_not_an_error() {
        let repo = tempdir().expect("temporary repository");
        assert!(discover(repo.path()).is_empty());
    }
}
