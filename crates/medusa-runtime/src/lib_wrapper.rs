extern crate medusa_git_workers as git_workers;
extern crate self as medusa_workers;

mod openai_oauth;
mod workspace_worker_manager;

// Preserve the existing runtime mutation API while routing WorkerManager through the
// workspace-aware adapter. Git-backed workspaces delegate to medusa-workers unchanged;
// ordinary directories use the content-addressed snapshot backend. These types are public so
// embedders can use the same workspace-aware mutation authority as the built-in user surfaces.
pub use crate::git_workers::{IntegrationReceipt, Worker, WorkerState};
pub use crate::openai_oauth::{
    OpenAiOAuthLogin, discover_openai_oauth_models, ensure_openai_oauth_connected,
    start_openai_oauth_login,
};
pub use crate::workspace_worker_manager::{
    WorkspaceMutationBackend, WorkspaceWorkerManager as WorkerManager,
};

include!("lib.rs");

pub mod behavioral_outcome;
pub mod behavioral_health {
    //! Runtime-facing re-export of the canonical cross-surface behavioral health contract.
    pub use medusa_improvement::behavioral_health::*;
}
pub mod runtime_config;
pub mod service_provider;

#[rustfmt::skip]
mod parent_reviewer;
// Conflict-aware mutation scheduling and deterministic aggregate staging.
mod parallel_mutation;
mod parallel_mutation_batch;

pub mod openai_realtime_session;
pub mod openai_realtime_websocket;
pub mod workspace;
