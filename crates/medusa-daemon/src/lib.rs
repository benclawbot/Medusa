//! Persistent local daemon, cross-platform IPC, process ownership, and crash recovery.

mod paths;
mod protocol;
mod scheduler;
mod server;
mod transport;

pub use paths::DaemonPaths;
pub use protocol::{
    DAEMON_PROTOCOL_VERSION, JobRecord, JobState, Request, RequestEnvelope, Response,
    ResponseEnvelope,
};
pub use scheduler::DaemonLimits;
pub use server::{DaemonClient, ServerHandle, serve, serve_with_limits, spawn, spawn_with_limits};
