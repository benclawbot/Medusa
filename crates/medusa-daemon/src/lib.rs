//! Persistent local daemon, cross-platform IPC, process ownership, crash recovery, lifecycle supervision, and remote frontend gateways.

mod artifact_store;
mod cancellation;
mod control_plane;
pub mod frontend_control;
mod lifecycle;
pub mod live_session;
pub mod observability;
mod paths;
#[path = "process_bounded.rs"]
mod process;
mod protocol;
mod scheduler;
mod server;
pub mod telegram;
mod transport;

pub use artifact_store::FrontendArtifactExport;
pub use control_plane::{
    ControlPlaneError, RuntimeBinding, SupervisionControlPlane, SupervisionEvent,
};
pub use frontend_control::{
    FrontendCommandAcknowledgement, FrontendControlError, FrontendControlPlane,
    FrontendControlResult, FrontendTransientEvent,
};
pub use lifecycle::{DaemonLaunch, DaemonLifecycle, DaemonLifecycleState, DaemonSupervisor};
pub use live_session::{
    LiveSessionAttachmentView, LiveSessionBroker, LiveSessionBrokerError, LiveSessionReplayView,
    LiveSessionSummary,
};
pub use medusa_process_containment::{ConfinedDir, ConfinedReadError};
pub use observability::initialize_observability;
pub use paths::DaemonPaths;
pub use protocol::{
    DAEMON_PROTOCOL_VERSION, FrontendArtifactKind, FrontendArtifactUpload,
    FrontendCredentialUpdate, JobRecord, JobState, Request, RequestEnvelope, Response,
    ResponseEnvelope,
};
pub use scheduler::DaemonLimits;
pub use server::{
    DaemonClient, ServerHandle, serve, serve_with_config, serve_with_limits, spawn,
    spawn_with_config, spawn_with_limits,
};
