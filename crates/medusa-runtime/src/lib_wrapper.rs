extern crate medusa_git_workers as git_workers;
extern crate self as medusa_workers;

mod workspace_worker_manager;
mod openai_oauth;

// Preserve the existing runtime mutation API while routing WorkerManager through the
// workspace-aware adapter. Git-backed workspaces delegate to medusa-workers unchanged;
// ordinary directories use the content-addressed snapshot backend. These types are public so
// embedders can use the same workspace-aware mutation authority as the built-in user surfaces.
pub use crate::git_workers::{IntegrationReceipt, Worker, WorkerState};
pub use crate::openai_oauth::{discover_openai_oauth_models, ensure_openai_oauth_connected};
pub use crate::workspace_worker_manager::{
    WorkspaceMutationBackend, WorkspaceWorkerManager as WorkerManager,
};

include!("lib.rs");

#[rustfmt::skip]
mod parent_reviewer;
// Conflict-aware mutation scheduling and deterministic aggregate staging.
mod parallel_mutation;
mod parallel_mutation_batch;

pub mod openai_realtime_session;
pub mod openai_realtime_websocket;
pub mod workspace;
