use std::{env, fs, path::Path};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_update::{
    CURRENT_RELEASE_ID, MainBranchUpdater, Restart, UpdateOutcome, read_update_outcome,
};
use serde::Serialize;
use tauri::Emitter;

const DESKTOP_UPDATE_PROGRESS_EVENT: &str = "desktop-update-progress";

/// Commit update health only after the React renderer has mounted its recovery-capable shell.
/// Tauri's `Ready` event means the native window exists; it does not prove the user interface
/// successfully bootstrapped.
#[tauri::command]
pub fn desktop_update_renderer_ready() -> Result<bool, String> {
    medusa_update::acknowledge_update_health().map_err(|error| error.to_string())
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopUpdateStatus {
    current_version: String,
    current_revision: String,
    latest_main_sha: String,
    executable: String,
    ready: bool,
    artifact_published: bool,
    up_to_date: bool,
    last_outcome: Option<UpdateOutcome>,
    /// Install channel recorded by the installer (`release`, `main`, or
    /// absent). Release installs must not be pushed main builds.
    channel: Option<String>,
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
    let current_revision = option_env!("MEDUSA_BUILD_COMMIT")
        .unwrap_or("unknown")
        .to_owned();
    let installed = revisions_match(&current_revision, &latest_main_sha);
    let last_outcome = executable
        .parent()
        .map(read_update_outcome)
        .transpose()?
        .flatten();
    let channel = installed_channel(executable.parent());
    // Release-channel installs are never offered rolling-main builds here;
    // they update through the verified release path instead.
    let on_release_channel = channel.as_deref() == Some("release");
    Ok(DesktopUpdateStatus {
        current_version: CURRENT_RELEASE_ID.to_owned(),
        current_revision,
        latest_main_sha,
        executable: executable.display().to_string(),
        ready: artifact_published && !installed && !on_release_channel,
        artifact_published,
        up_to_date: installed,
        last_outcome,
        channel,
    })
}

/// Reads the install-channel marker written beside the executable by
/// install.sh / install.ps1. Absent markers mean "unknown", never "main".
fn installed_channel(directory: Option<&Path>) -> Option<String> {
    let marker = fs::read_to_string(directory?.join(".medusa-install-channel")).ok()?;
    let channel = marker.trim().to_owned();
    (!channel.is_empty()).then_some(channel)
}

fn revisions_match(installed: &str, available: &str) -> bool {
    installed != "unknown" && installed.eq_ignore_ascii_case(available)
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
        previous_revision: option_env!("MEDUSA_BUILD_COMMIT").map(str::to_owned),
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
    fn release_channel_is_never_offered_main_builds() {
        assert_eq!(
            installed_channel(None),
            None,
            "absent marker means unknown, not main"
        );
        let directory = tempfile::tempdir().expect("tempdir");
        assert_eq!(installed_channel(Some(directory.path())), None);
        fs::write(
            directory.path().join(".medusa-install-channel"),
            "release\n",
        )
        .expect("marker");
        assert_eq!(
            installed_channel(Some(directory.path())).as_deref(),
            Some("release")
        );
    }

    #[test]
    fn installed_revision_controls_update_availability() {
        assert!(revisions_match(
            "0123456789abcdef0123456789abcdef01234567",
            "0123456789ABCDEF0123456789ABCDEF01234567"
        ));
        assert!(!revisions_match("unknown", TARGET_SHA));
        assert!(!revisions_match(
            "0123456789abcdef0123456789abcdef01234567",
            "fedcba9876543210fedcba9876543210fedcba98"
        ));
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
