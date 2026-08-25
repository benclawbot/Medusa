use std::env;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_update::{MainBranchUpdater, Restart};
use serde::Serialize;
use tauri::Emitter;

const DESKTOP_UPDATE_PROGRESS_EVENT: &str = "desktop-update-progress";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopUpdateStatus {
    current_version: String,
    latest_main_sha: String,
    executable: String,
    ready: bool,
    artifact_published: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopUpdateProgress {
    phase: String,
    completed: u64,
    total: Option<u64>,
    message: String,
}

#[tauri::command]
pub async fn desktop_update_status() -> Result<DesktopUpdateStatus, String> {
    tauri::async_runtime::spawn_blocking(status)
        .await
        .map_err(|error| format!("desktop update status task failed: {error}"))?
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn desktop_update_from_main(
    app: tauri::AppHandle,
    target_sha: String,
) -> Result<(), String> {
    let update_app = app.clone();
    let result =
        tauri::async_runtime::spawn_blocking(move || schedule_update(&update_app, &target_sha))
            .await;

    match result {
        Ok(Ok(())) => {
            app.exit(0);
            Ok(())
        }
        Ok(Err(error)) => {
            emit_progress(&app, "failed", 0, None, &format!("Update failed: {error}"));
            Err(error.to_string())
        }
        Err(error) => {
            let message = format!("desktop update task failed: {error}");
            emit_progress(&app, "failed", 0, None, &message);
            Err(message)
        }
    }
}

fn status() -> MedusaResult<DesktopUpdateStatus> {
    let executable = env::current_exe()?;
    let updater = MainBranchUpdater::public()?;
    let latest_main_sha = updater.latest_main()?.sha;
    let artifact_published = updater.main_desktop_artifact_available(&latest_main_sha)?;
    Ok(DesktopUpdateStatus {
        current_version: env!("CARGO_PKG_VERSION").to_owned(),
        latest_main_sha,
        executable: executable.display().to_string(),
        ready: artifact_published,
        artifact_published,
    })
}

fn schedule_update(app: &tauri::AppHandle, target_sha: &str) -> MedusaResult<()> {
    let target_sha = validate_target_sha(target_sha)?;
    emit_progress(
        app,
        "preparing",
        0,
        None,
        "Preparing the verified desktop update…",
    );
    let updater = MainBranchUpdater::public()?;
    let latest_main_sha = updater.latest_main()?.sha;
    if latest_main_sha != target_sha {
        return Err(MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Transient,
            "main changed while the desktop update was being prepared; check again",
        )
        .with_retryable(true));
    }
    if !updater.main_desktop_artifact_available(target_sha)? {
        return Err(MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Transient,
            "the checked desktop revision is not published yet; check again shortly",
        )
        .with_retryable(true));
    }

    let executable = env::current_exe()?;
    let parent_pid = std::process::id();
    emit_progress(
        app,
        "downloading",
        0,
        None,
        "Downloading the verified desktop executable…",
    );

    let mut last_percent = 0_u64;
    let restart = Restart {
        detached: true,
        ..Restart::default()
    };
    updater.schedule_main_desktop_install(
        &executable,
        &restart,
        parent_pid,
        |completed, total| {
            let should_emit = match total {
                Some(total) if total > 0 => {
                    let percent = completed.saturating_mul(100) / total;
                    if percent > last_percent || completed == total {
                        last_percent = percent;
                        true
                    } else {
                        false
                    }
                }
                _ => true,
            };
            if should_emit {
                emit_progress(
                    app,
                    "downloading",
                    completed,
                    total,
                    "Downloading the verified desktop executable…",
                );
            }
        },
    )?;

    emit_progress(
        app,
        "installing",
        1,
        Some(1),
        "Download verified; preparing the final replacement…",
    );
    emit_progress(
        app,
        "replacing",
        99,
        Some(100),
        "The update is ready. Medusa Desktop will close briefly while the application is replaced, then reopen automatically…",
    );
    Ok(())
}

fn emit_progress(
    app: &tauri::AppHandle,
    phase: &str,
    completed: u64,
    total: Option<u64>,
    message: &str,
) {
    let _ = app.emit(
        DESKTOP_UPDATE_PROGRESS_EVENT,
        DesktopUpdateProgress {
            phase: phase.to_owned(),
            completed,
            total,
            message: message.to_owned(),
        },
    );
}

fn validate_target_sha(target_sha: &str) -> MedusaResult<&str> {
    if target_sha.len() == 40 && target_sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(target_sha);
    }
    Err(MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        "desktop update target must be a full 40-character Git commit SHA",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn update_target_requires_full_commit_sha() {
        assert_eq!(validate_target_sha(TARGET_SHA).unwrap(), TARGET_SHA);
        assert!(validate_target_sha("main").is_err());
        assert!(validate_target_sha("0123456789abcdef0123456789abcdef0123456z").is_err());
        assert!(validate_target_sha("01234567;rm -rf /").is_err());
    }

    #[test]
    fn update_path_uses_published_artifacts_instead_of_a_source_build() {
        let source = include_str!("desktop_update.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production update source");
        for forbidden in ["npm ci", "npm run build", "cargo build", "git fetch"] {
            assert!(
                !source.contains(forbidden),
                "found forbidden client build: {forbidden}"
            );
        }
    }

    #[test]
    fn progress_payload_serializes_for_the_frontend_contract() {
        let payload = DesktopUpdateProgress {
            phase: "downloading".to_owned(),
            completed: 2,
            total: Some(4),
            message: "Downloading".to_owned(),
        };
        let value = serde_json::to_value(payload).expect("progress payload");
        assert_eq!(value["completed"], 2);
        assert_eq!(value["total"], 4);
        assert_eq!(value["message"], "Downloading");
    }
}
