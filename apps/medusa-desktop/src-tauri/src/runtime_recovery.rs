use medusa_runtime::{
    RecoveryActionRequest, RecoveryOperation, RecoveryPreflightEvidence, RecoveryView,
};

use crate::dto::DesktopRecoveryActionRequest;

#[tauri::command]
pub fn runtime_recovery_action(
    runtime_id: String,
    request: DesktopRecoveryActionRequest,
    registry: tauri::State<'_, RuntimeRegistry>,
) -> Result<(), String> {
    let view: RecoveryView = serde_json::from_value(request.recovery)
        .map_err(|error| format!("invalid recovery view: {error}"))?;
    let operation = parse_recovery_operation(&request.operation)?;
    let action = RecoveryActionRequest {
        session_id: view.session_id.clone(),
        operation,
        checkpoint_id: request.checkpoint_id,
        confirmed_destructive_effects: request.confirmed_destructive_effects,
    };
    let preflight = RecoveryPreflightEvidence {
        repository_fingerprint_before: request.repository_fingerprint_before,
        checkpoint_integrity_verified: request.checkpoint_integrity_verified,
        repository_preconditions_verified: request.repository_preconditions_verified,
        conflicting_uncommitted_paths: request.conflicting_uncommitted_paths,
        unresolved_risks: request.unresolved_risks,
    };
    registry.with_entry(&runtime_id, |entry| {
        entry
            .controller
            .execute_recovery(view, action, preflight)
            .map_err(|error| error.to_string())
    })
}

fn parse_recovery_operation(value: &str) -> Result<RecoveryOperation, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "inspect" => Ok(RecoveryOperation::Inspect),
        "resume" => Ok(RecoveryOperation::Resume),
        "restorecheckpoint" | "restore_checkpoint" | "restore-checkpoint" => {
            Ok(RecoveryOperation::RestoreCheckpoint)
        }
        "retryverification" | "retry_verification" | "retry-verification" => {
            Ok(RecoveryOperation::RetryVerification)
        }
        "abandon" => Ok(RecoveryOperation::Abandon),
        _ => Err(format!("unknown recovery operation: {value}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_desktop_and_rust_style_operation_names() {
        assert_eq!(
            parse_recovery_operation("restoreCheckpoint").unwrap(),
            RecoveryOperation::RestoreCheckpoint
        );
        assert_eq!(
            parse_recovery_operation("retry_verification").unwrap(),
            RecoveryOperation::RetryVerification
        );
    }
}
