mod config;
mod credentials;
mod desktop_command;
mod desktop_update;
mod diffs;
mod dto;
mod memories;
mod model_registry;
mod mutations;
mod permissions;
mod preview;
mod provider_auth;
mod review;
mod runtime {
    include!("runtime.rs");
    include!("desktop_projection.rs");
    include!("runtime_resume.rs");
    include!("runtime_recovery.rs");
    include!("runtime_wakeup.rs");
}
mod sessions;
#[cfg(test)]
mod test_tempfile;
mod worktree;
#[cfg(test)]
extern crate self as tempfile;
#[cfg(test)]
pub(crate) use test_tempfile::tempdir;

use config::{desktop_provider_catalog, desktop_shared_configuration};
use desktop_update::{
    desktop_update_from_main, desktop_update_renderer_ready, desktop_update_status,
};
use diffs::runtime_read_diff;
use memories::runtime_list_memories;
use model_registry::desktop_model_registry;
use mutations::{
    runtime_commit_changes, runtime_create_branch, runtime_create_checkpoint, runtime_push_branch,
};
use permissions::{desktop_permission_mode, desktop_permission_modes, desktop_set_permission_mode};
use preview::{PreviewRegistry, preview_runtime_close, preview_runtime_find_web_artifact};
use provider_auth::{desktop_browser_oauth, desktop_ensure_browser_oauth};
use review::{runtime_apply_review_action, runtime_export_review_audit, runtime_read_review};
use runtime::{
    RuntimeRegistry, runtime_begin_wakeups, runtime_cancel, runtime_command,
    runtime_command_suggestions, runtime_configure_model, runtime_open_web_artifact, runtime_poll,
    runtime_recovery_action, runtime_resume, runtime_start, runtime_submit,
};
use sessions::{
    runtime_list_sessions, runtime_list_sessions_page, runtime_read_session,
    runtime_read_session_page,
};
use tauri::Manager;
use worktree::runtime_read_worktree;

pub fn daemon_config() -> Result<medusa_config::Config, String> {
    config::active_config()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> tauri::Result<()> {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(RuntimeRegistry::default())
        .manage(PreviewRegistry::default())
        .register_uri_scheme_protocol("asset", |ctx, request| {
            let previews = ctx.app_handle().state::<PreviewRegistry>();
            preview::handle_protocol_request(
                ctx.app_handle(),
                ctx.webview_label(),
                request.uri().path(),
                previews,
            )
        })
        .invoke_handler(tauri::generate_handler![
            desktop_shared_configuration,
            desktop_provider_catalog,
            desktop_model_registry,
            desktop_permission_modes,
            desktop_permission_mode,
            desktop_set_permission_mode,
            desktop_browser_oauth,
            desktop_ensure_browser_oauth,
            runtime_start,
            runtime_resume,
            runtime_begin_wakeups,
            preview_runtime_close,
            runtime_submit,
            runtime_command,
            runtime_command_suggestions,
            runtime_cancel,
            runtime_poll,
            preview_runtime_find_web_artifact,
            runtime_open_web_artifact,
            runtime_configure_model,
            runtime_recovery_action,
            runtime_read_review,
            runtime_apply_review_action,
            runtime_export_review_audit,
            runtime_list_sessions,
            runtime_list_sessions_page,
            runtime_read_session,
            runtime_read_session_page,
            runtime_read_diff,
            runtime_read_worktree,
            runtime_create_branch,
            runtime_create_checkpoint,
            runtime_commit_changes,
            runtime_push_branch,
            runtime_list_memories,
            desktop_update_status,
            desktop_update_renderer_ready,
            desktop_update_from_main,
        ])
        .build(tauri::generate_context!())?
        .run(|app_handle, event| {
            if matches!(
                event,
                tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
            ) {
                if let Some(registry) = app_handle.try_state::<RuntimeRegistry>() {
                    registry.shutdown_all();
                }
            }
        });
    Ok(())
}
