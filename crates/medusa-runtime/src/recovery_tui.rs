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
    RecoveryPreflightEvidence, RecoveryView, RecoveryViewInput, VerificationState,
};

const RECOVERY_DIRECTORY: &str = ".medusa/recovery";
struct RuntimeRecoveryExecutor {
    repository_fingerprint: String,
    checkpoint_fingerprints: BTreeMap<String, String>,
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

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}
