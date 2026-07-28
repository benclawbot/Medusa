use medusa_runtime::{RecoveryActionRequest, RecoveryOperation, recovery_action_context};

use crate::dto::DesktopRecoveryActionRequest;

#[tauri::command]
pub fn runtime_recovery_action(
    runtime_id: String,
    request: DesktopRecoveryActionRequest,
    registry: tauri::State<'_, RuntimeRegistry>,
) -> Result<(), String> {
    let session_id = request
        .recovery
        .get("session_id")
        .or_else(|| request.recovery.get("sessionId"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "recovery request is missing a session id".to_owned())?
        .to_owned();
    let operation = parse_recovery_operation(&request.operation)?;
    let action = RecoveryActionRequest {
        session_id,
        operation,
        checkpoint_id: request.checkpoint_id,
        confirmed_destructive_effects: request.confirmed_destructive_effects,
    };
    registry.with_entry(&runtime_id, |entry| {
        let (view, preflight) = recovery_action_context(&entry.repo, &action)?;
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
mod recovery_action_tests {
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
