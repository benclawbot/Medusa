mod config;
mod credentials;
mod desktop_command;
mod desktop_update;
mod diffs;
mod dto;
mod engineering;
mod github_actions;
mod github_audit;
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
mod learning;
mod memories;
mod model_registry;
mod mutations;
mod provider_auth;
mod pull_requests;
mod review;
mod runtime {
    include!("runtime.rs");
    include!("desktop_projection.rs");
    include!("runtime_resume.rs");
    include!("runtime_recovery.rs");
}
mod sessions;
#[cfg(test)]
mod test_tempfile;
mod voice;
mod worktree;
#[cfg(test)]
extern crate self as tempfile;
#[cfg(test)]
pub(crate) use test_tempfile::tempdir;

use config::{desktop_provider_catalog, desktop_shared_configuration};
use desktop_update::{desktop_update_from_main, desktop_update_status};
use diffs::runtime_read_diff;
use engineering::runtime_engineering_dashboard;
use github_actions::runtime_retry_github_actions_job;
use github_audit::runtime_persist_github_mutation_audit;
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
use learning::{
    runtime_learning_evaluate, runtime_learning_export, runtime_learning_inspect,
    runtime_learning_privacy, runtime_learning_propose, runtime_learning_redaction_preview,
    runtime_learning_review, runtime_learning_transition,
};
use memories::runtime_list_memories;
use model_registry::desktop_model_registry;
use mutations::{
    runtime_commit_changes, runtime_create_branch, runtime_create_checkpoint, runtime_push_branch,
};
use provider_auth::desktop_browser_oauth;
use pull_requests::runtime_create_draft_pull_request;
use review::{runtime_apply_review_action, runtime_export_review_audit, runtime_read_review};
use runtime::{
    RuntimeRegistry, runtime_cancel, runtime_close, runtime_command, runtime_command_suggestions,
    runtime_configure_model, runtime_poll, runtime_recovery_action, runtime_resume, runtime_start,
    runtime_submit,
};
use sessions::{runtime_list_sessions, runtime_read_session};
use voice::{desktop_establish_realtime_session, desktop_realtime_capability};
use worktree::runtime_read_worktree;

pub fn daemon_config() -> Result<medusa_config::Config, String> {
    config::active_config()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> tauri::Result<()> {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(RuntimeRegistry::default())
        .invoke_handler(tauri::generate_handler![
            desktop_shared_configuration,
            desktop_provider_catalog,
            desktop_model_registry,
            desktop_browser_oauth,
            desktop_realtime_capability,
            desktop_establish_realtime_session,
            runtime_start,
            runtime_resume,
            runtime_close,
            runtime_submit,
            runtime_command,
            runtime_command_suggestions,
            runtime_cancel,
            runtime_poll,
            runtime_configure_model,
            runtime_recovery_action,
            runtime_read_review,
            runtime_apply_review_action,
            runtime_export_review_audit,
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
            runtime_persist_github_mutation_audit,
            runtime_github_actions_job_log,
            runtime_retry_github_actions_job,
            runtime_merge_github_pull_request,
            runtime_list_memories,
            runtime_engineering_dashboard,
            runtime_learning_review,
            runtime_learning_transition,
            runtime_learning_inspect,
            runtime_learning_propose,
            runtime_learning_evaluate,
            runtime_learning_privacy,
            runtime_learning_redaction_preview,
            runtime_learning_export,
            desktop_update_status,
            desktop_update_from_main,
        ])
        .run(tauri::generate_context!())
}
