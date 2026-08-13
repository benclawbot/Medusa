extern crate medusa_git_workers as git_workers;

mod workspace_worker_manager;

// Preserve the existing runtime mutation API while routing WorkerManager through the
// workspace-aware adapter. Git-backed workspaces delegate to medusa-workers unchanged;
// ordinary directories use the content-addressed snapshot backend.
mod medusa_workers {
    pub use crate::git_workers::{IntegrationReceipt, Worker, WorkerState};
    pub use crate::workspace_worker_manager::WorkspaceWorkerManager as WorkerManager;
}

include!("lib.rs");

#[rustfmt::skip]
mod parent_reviewer;
// Conflict-aware mutation scheduling and deterministic aggregate staging.
mod parallel_mutation;
mod parallel_mutation_batch;

pub mod openai_realtime_session;
pub mod openai_realtime_websocket;
pub mod workspace;
