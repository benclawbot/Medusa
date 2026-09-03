use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    RuntimeError, checkpoint_payload,
    recovery_model::{PersistedRecoveryRecord, common_outcome},
};
use medusa_recovery_coordinator::{
    AuthorizedRecoveryAction, RecoveryActionExecutor, RecoveryActionRequest, RecoveryActionService,
    RecoveryExecutionOutcome, RecoveryExecutionReceipt, RecoveryOperation,
    RecoveryPreflightEvidence, RecoveryPreview, RecoveryView, RecoveryViewInput, VerificationState,
};

const RECOVERY_DIRECTORY: &str = ".medusa/recovery";
struct RuntimeRecoveryExecutor {
    repository_fingerprint: String,
    checkpoint_fingerprints: BTreeMap<String, String>,
    expected_preview: Option<RecoveryPreview>,
}

impl RecoveryActionExecutor for RuntimeRecoveryExecutor {
    type Error = RuntimeError;

    fn execute(
        &mut self,
        repository: &Path,
        action: &AuthorizedRecoveryAction,
    ) -> Result<RecoveryExecutionOutcome, Self::Error> {
        let outcome = if let Some(outcome) =
            common_outcome(action.operation, &self.repository_fingerprint)
        {
            outcome
        } else {
            let checkpoint_id = action
                .checkpoint_id
                .as_deref()
                .ok_or_else(|| RuntimeError::InvalidCommand("missing checkpoint id".to_owned()))?;
            let payload = checkpoint_payload::load(repository, &action.session_id, checkpoint_id)?;
            let live_fingerprint =
                checkpoint_payload::current_repository_fingerprint(repository, &payload)?;
            if live_fingerprint != self.repository_fingerprint {
                return Err(RuntimeError::InvalidCommand(
                    "repository changed after recovery authorization; regenerate the recovery preview"
                        .to_owned(),
                ));
            }
            let expected_preview = self.expected_preview.as_ref().ok_or_else(|| {
                RuntimeError::InvalidCommand(
                    "authorized restore is missing its recovery preview".to_owned(),
                )
            })?;
            let live_preview = checkpoint_payload::preview(repository, &payload)?;
            if &live_preview != expected_preview {
                return Err(RuntimeError::InvalidCommand(
                    "repository recovery preview changed before restore; regenerate the recovery preview"
                        .to_owned(),
                ));
            }
            checkpoint_payload::restore(repository, &payload)?;
            let fingerprint = self
                .checkpoint_fingerprints
                .get(checkpoint_id)
                .cloned()
                .ok_or_else(|| {
                    RuntimeError::InvalidCommand("checkpoint metadata is unavailable".to_owned())
                })?;
            RecoveryExecutionOutcome::succeeded(fingerprint, VerificationState::Incomplete)
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
    validate_restore_evidence(repo, view, request, &preflight)?;
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
        expected_preview: view.selected_preview.clone(),
    };
    RecoveryActionService::new(executor)
        .execute_and_audit(repo, view, request, preflight, now_unix_ms())
        .map_err(|error| error.to_string())
}

fn validate_restore_evidence(
    repo: &Path,
    view: &RecoveryView,
    request: &RecoveryActionRequest,
    preflight: &RecoveryPreflightEvidence,
) -> Result<(), String> {
    if request.operation != RecoveryOperation::RestoreCheckpoint {
        return Ok(());
    }
    let checkpoint_id = request
        .checkpoint_id
        .as_deref()
        .ok_or_else(|| "restore requires a selected checkpoint".to_owned())?;
    let checkpoint = view
        .checkpoints
        .iter()
        .find(|checkpoint| checkpoint.id == checkpoint_id)
        .ok_or_else(|| "selected recovery checkpoint is no longer available".to_owned())?;
    if !checkpoint.integrity_verified || !preflight.checkpoint_integrity_verified {
        return Err("selected recovery checkpoint failed integrity verification".to_owned());
    }
    let expected_preview = view
        .selected_preview
        .as_ref()
        .filter(|preview| preview.checkpoint_id == checkpoint_id)
        .ok_or_else(|| "restore requires a current authoritative preview".to_owned())?;
    let payload = checkpoint_payload::load(repo, &request.session_id, checkpoint_id)
        .map_err(|error| error.to_string())?;
    if payload.repository_fingerprint != checkpoint.repository_fingerprint {
        return Err("checkpoint payload no longer matches recovery metadata".to_owned());
    }

    let live_fingerprint = checkpoint_payload::current_repository_fingerprint(repo, &payload)
        .map_err(|error| error.to_string())?;
    if live_fingerprint != view.current_repository_fingerprint
        || live_fingerprint != preflight.repository_fingerprint_before
    {
        return Err(
            "repository changed since the recovery preview; regenerate the recovery preview"
                .to_owned(),
        );
    }
    let live_preview =
        checkpoint_payload::preview(repo, &payload).map_err(|error| error.to_string())?;
    if &live_preview != expected_preview {
        return Err(
            "repository recovery preview is stale; regenerate it before restoring".to_owned(),
        );
    }

    let conflicting_uncommitted_paths = live_preview
        .files
        .iter()
        .filter(|file| file.would_overwrite_uncommitted_work)
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let unresolved_risks = live_preview.unresolved_risks.clone();
    let repository_preconditions_verified = live_preview.repository_matches_checkpoint_base
        && conflicting_uncommitted_paths.is_empty()
        && unresolved_risks.is_empty();
    if preflight.conflicting_uncommitted_paths != conflicting_uncommitted_paths
        || preflight.unresolved_risks != unresolved_risks
        || preflight.repository_preconditions_verified != repository_preconditions_verified
    {
        return Err(
            "recovery preflight evidence no longer matches the live repository".to_owned(),
        );
    }
    Ok(())
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

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use medusa_agent::{AgentEngine, record_session_event};
    use medusa_config::Config;
    use medusa_core::MedusaResult;
    use medusa_protocol::{Actor, EventPayload};
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};
    use medusa_recovery_coordinator::{CheckpointPresentation, RecoveryViewInput};
    use tempfile::tempdir;

    use super::*;

    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("session creation does not call the provider")
        }
    }

    #[test]
    fn restore_rejects_repository_drift_after_preview() {
        let repository = tempdir().expect("repository");
        fs::create_dir_all(repository.path().join("src")).expect("src");
        fs::write(repository.path().join("src/lib.rs"), "checkpoint").expect("write");
        let engine = AgentEngine::new(UnusedProvider, Config::default());
        let mut session = engine
            .create_session(repository.path(), "Recovery drift test".to_owned())
            .expect("session");
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::FileTransactionCommitted {
                paths: vec!["src/lib.rs".to_owned()],
                rollback_ref: "rollback".to_owned(),
            },
        )
        .expect("file event");
        let checkpoint =
            crate::checkpoint_store::materialize(repository.path(), session.id.as_str())
                .expect("checkpoint");
        let payload = checkpoint_payload::materialize(repository.path(), &checkpoint)
            .expect("payload");
        fs::write(repository.path().join("src/lib.rs"), "changed").expect("change");
        let preview = checkpoint_payload::preview(repository.path(), &payload).expect("preview");
        let current =
            checkpoint_payload::current_repository_fingerprint(repository.path(), &payload)
                .expect("fingerprint");
        let view = RecoveryView::build(RecoveryViewInput {
            session_id: session.id.to_string(),
            last_durable_step: "implement".to_owned(),
            interrupted_operation: Some("verification".to_owned()),
            current_repository_fingerprint: current.clone(),
            verification: VerificationState::Incomplete,
            approvals_must_be_reestablished: false,
            containment_must_be_reestablished: false,
            checkpoints: vec![CheckpointPresentation {
                id: payload.checkpoint_id.clone(),
                sequence: 1,
                created_at_unix_ms: 1,
                task_step: "implement".to_owned(),
                reason: "durable progress".to_owned(),
                repository_fingerprint: payload.repository_fingerprint.clone(),
                verification: VerificationState::Incomplete,
                provenance: "execution-checkpoint/v1".to_owned(),
                integrity_verified: true,
            }],
            selected_preview: Some(preview.clone()),
            source_corrupt: false,
        });
        let request = RecoveryActionRequest {
            session_id: session.id.to_string(),
            operation: RecoveryOperation::RestoreCheckpoint,
            checkpoint_id: Some(payload.checkpoint_id.clone()),
            confirmed_destructive_effects: true,
        };
        let conflicting_uncommitted_paths = preview
            .files
            .iter()
            .filter(|file| file.would_overwrite_uncommitted_work)
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let preflight = RecoveryPreflightEvidence {
            repository_fingerprint_before: current,
            checkpoint_integrity_verified: true,
            repository_preconditions_verified: preview.repository_matches_checkpoint_base
                && conflicting_uncommitted_paths.is_empty()
                && preview.unresolved_risks.is_empty(),
            conflicting_uncommitted_paths,
            unresolved_risks: preview.unresolved_risks.clone(),
        };

        validate_restore_evidence(repository.path(), &view, &request, &preflight)
            .expect("fresh preview");
        fs::write(repository.path().join("src/lib.rs"), "changed again").expect("drift");
        assert!(
            validate_restore_evidence(repository.path(), &view, &request, &preflight).is_err()
        );
    }
}
