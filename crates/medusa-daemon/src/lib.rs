//! Persistent local daemon, cross-platform IPC, process ownership, and crash recovery.

mod paths;
mod protocol;
mod server;
mod transport;

pub use paths::DaemonPaths;
pub use protocol::{
    DAEMON_PROTOCOL_VERSION, JobRecord, JobState, Request, RequestEnvelope, Response,
    ResponseEnvelope,
};
pub use server::{DaemonClient, ServerHandle, serve, spawn};
