use crate::dto::DesktopRecoveryActionRequest;

#[tauri::command]
pub fn runtime_recovery_action(
    runtime_id: String,
    request: DesktopRecoveryActionRequest,
    registry: tauri::State<'_, RuntimeRegistry>,
) -> Result<(), String> {
    let DesktopRecoveryActionRequest {
        recovery,
        operation,
        checkpoint_id,
        confirmed_destructive_effects,
        repository_fingerprint_before,
        checkpoint_integrity_verified,
        repository_preconditions_verified,
        conflicting_uncommitted_paths,
        unresolved_risks,
    } = request;
    let _untrusted_frontend_evidence = (
        repository_fingerprint_before,
        checkpoint_integrity_verified,
        repository_preconditions_verified,
        conflicting_uncommitted_paths,
        unresolved_risks,
    );
    let session_id = recovery
        .get("session_id")
        .or_else(|| recovery.get("sessionId"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "recovery request is missing a session id".to_owned())?
        .to_owned();
    registry.with_entry(&runtime_id, |entry| {
        let acknowledgement = entry.dispatch_for_session(
            session_id,
            FrontendCommand::RecoveryAction {
                operation,
                checkpoint_id,
                confirmed_destructive_effects,
            },
        )?;
        if matches!(
            acknowledgement.result,
            FrontendControlResult::CommandAccepted { .. }
        ) {
            Ok(())
        } else {
            Err("daemon returned an unexpected recovery result".to_owned())
        }
    })
}
