mod credentials;
mod diffs;
mod dto;
mod github_actions;
mod github_auth;
mod github_checks;
mod github_issue_mutations;
mod github_issues;
mod github_logs;
#[rustfmt::skip]
mod github_merge;
mod github_private_repository;
mod github_pull_request_mutations;
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
use github_actions::runtime_retry_github_actions_job;
use github_auth::runtime_github_auth_status;
use github_checks::runtime_github_commit_checks;
use github_issue_mutations::{runtime_create_github_issue, runtime_update_github_issue};
use github_issues::runtime_github_issues;
use github_logs::runtime_github_actions_job_log;
use github_merge::runtime_merge_github_pull_request;
use github_private_repository::{runtime_clone_github_repository, runtime_fetch_github_repository};
use github_pull_request_mutations::{
    runtime_review_github_pull_request, runtime_update_github_pull_request,
};
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
            runtime_clone_github_repository,
            runtime_fetch_github_repository,
            runtime_github_commit_checks,
            runtime_github_issues,
            runtime_create_github_issue,
            runtime_update_github_issue,
            runtime_update_github_pull_request,
            runtime_review_github_pull_request,
            runtime_github_actions_job_log,
            runtime_retry_github_actions_job,
            runtime_merge_github_pull_request,
            runtime_list_memories,
        ])
        .run(tauri::generate_context!())
}
