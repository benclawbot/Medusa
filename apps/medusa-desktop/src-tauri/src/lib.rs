mod credentials;
mod diffs;
mod dto;
mod github_auth;
mod github_checks;
mod github_repository;
mod memories;
mod mutations;
mod pull_requests;
mod runtime {
    include!("runtime.rs");
    include!("runtime_resume.rs");
}
mod sessions;
#[cfg(test)]
mod test_tempfile;
mod worktree;
#[cfg(test)]
extern crate self as tempfile;
#[cfg(test)]
pub(crate) use test_tempfile::tempdir;

use diffs::runtime_read_diff;
use github_auth::runtime_github_auth_status;
use github_checks::runtime_github_commit_checks;
use github_repository::runtime_github_repository_access;
use memories::runtime_list_memories;
use mutations::{
    runtime_commit_changes, runtime_create_branch, runtime_create_checkpoint, runtime_push_branch,
};
use pull_requests::runtime_create_draft_pull_request;
use runtime::{
    RuntimeRegistry, runtime_cancel, runtime_close, runtime_command, runtime_command_suggestions,
    runtime_configure_model, runtime_poll, runtime_resume, runtime_start, runtime_submit,
};
use sessions::{runtime_list_sessions, runtime_read_session};
use worktree::runtime_read_worktree;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> tauri::Result<()> {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(RuntimeRegistry::default())
        .invoke_handler(tauri::generate_handler![
            runtime_start,
            runtime_resume,
            runtime_close,
            runtime_submit,
            runtime_command,
            runtime_command_suggestions,
            runtime_cancel,
            runtime_poll,
            runtime_configure_model,
            runtime_list_sessions,
            runtime_read_session,
            runtime_read_diff,
            runtime_read_worktree,
            runtime_create_branch,
            runtime_create_checkpoint,
            runtime_commit_changes,
            runtime_push_branch,
            runtime_create_draft_pull_request,
            runtime_github_auth_status,
            runtime_github_repository_access,
            runtime_github_commit_checks,
            runtime_list_memories,
        ])
        .run(tauri::generate_context!())
}
