use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use medusa_review_model::ReviewActionRequest;
use medusa_runtime::review::{
    ReviewWorkspace, apply_review_action, export_review_audit, read_review_workspace,
};

// Review actions update persisted review state only; they never commit, push, or merge.
#[tauri::command]
pub fn runtime_read_review(repo: String) -> Result<ReviewWorkspace, String> {
    read_review_workspace(&PathBuf::from(repo)).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn runtime_apply_review_action(
    repo: String,
    operation: String,
    path: Option<String>,
    hunk_id: Option<String>,
    snapshot_id: String,
    file_fingerprint: Option<String>,
    hunk_fingerprint: Option<String>,
) -> Result<ReviewWorkspace, String> {
    let request = match operation.as_str() {
        "accept-file" => ReviewActionRequest::AcceptFile {
            path: path.ok_or("accept-file requires a path")?,
            expected_snapshot_id: snapshot_id,
        },
        "revert-file" => ReviewActionRequest::RevertFile {
            path: path.ok_or("revert-file requires a path")?,
            expected_snapshot_id: snapshot_id,
            expected_file_fingerprint: file_fingerprint
                .ok_or("revert-file requires a file fingerprint")?,
        },
        "revert-hunk" => ReviewActionRequest::RevertHunk {
            path: path.ok_or("revert-hunk requires a path")?,
            hunk_id: hunk_id.ok_or("revert-hunk requires a hunk id")?,
            expected_snapshot_id: snapshot_id,
            expected_file_fingerprint: file_fingerprint
                .ok_or("revert-hunk requires a file fingerprint")?,
            expected_hunk_fingerprint: hunk_fingerprint
                .ok_or("revert-hunk requires a hunk fingerprint")?,
        },
        "accept-task" => ReviewActionRequest::AcceptTask {
            expected_snapshot_id: snapshot_id,
        },
        _ => return Err(format!("unknown review operation: {operation}")),
    };
    apply_review_action(&PathBuf::from(repo), request, "desktop").map_err(|error| error.to_string())
}

#[tauri::command]
pub fn runtime_export_review_audit(repo: String) -> Result<serde_json::Value, String> {
    let generated_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_millis();
    let generated_at_unix_ms = i64::try_from(generated_at_unix_ms)
        .map_err(|_| "audit export timestamp exceeded i64".to_owned())?;
    let audit = export_review_audit(&PathBuf::from(repo), generated_at_unix_ms)
        .map_err(|error| error.to_string())?;
    serde_json::to_value(audit).map_err(|error| error.to_string())
}
