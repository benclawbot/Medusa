//! Persistent local daemon, cross-platform IPC, process ownership, crash recovery, lifecycle supervision, and remote frontend gateways.

mod cancellation;
mod control_plane;
mod lifecycle;
mod paths;
mod process;
mod protocol;
mod scheduler;
mod server;
pub mod telegram;
mod transport;

pub use control_plane::{
    ControlPlaneError, RuntimeBinding, SupervisionControlPlane, SupervisionEvent,
};
pub use lifecycle::{DaemonLaunch, DaemonLifecycle, DaemonLifecycleState, DaemonSupervisor};
pub use paths::DaemonPaths;
pub use protocol::{
    DAEMON_PROTOCOL_VERSION, JobRecord, JobState, Request, RequestEnvelope, Response,
    ResponseEnvelope,
};
pub use scheduler::DaemonLimits;
pub use server::{DaemonClient, ServerHandle, serve, serve_with_limits, spawn, spawn_with_limits};
