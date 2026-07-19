//! Persistent local daemon, cross-platform IPC, process ownership, crash recovery, and lifecycle supervision.

mod lifecycle;
mod paths;
mod protocol;
mod scheduler;
mod server;
mod transport;

pub use lifecycle::{DaemonLaunch, DaemonLifecycle, DaemonLifecycleState, DaemonSupervisor};
pub use paths::DaemonPaths;
pub use protocol::{
    DAEMON_PROTOCOL_VERSION, JobRecord, JobState, Request, RequestEnvelope, Response,
    ResponseEnvelope,
};
pub use scheduler::DaemonLimits;
pub use server::{DaemonClient, ServerHandle, serve, serve_with_limits, spawn, spawn_with_limits};
