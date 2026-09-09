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

use medusa_config::Config;
use medusa_core::MedusaResult;

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
    DaemonClient, ServerHandle, serve, serve_with_limits, spawn, spawn_with_config,
    spawn_with_limits,
};

/// Starts the repository daemon and warms the reusable ChatGPT app-server in parallel.
///
/// Daemon readiness must not depend on provider/network readiness, so the warmup is
/// best-effort and runs independently. When ChatGPT OAuth is already authenticated,
/// the runtime consumes the warmed Codex app-server on the first turn instead of
/// paying its cold process/protocol startup cost after prompt submission.
pub fn serve_with_config(paths: DaemonPaths, config: Config) -> MedusaResult<()> {
    if config.model.provider == "openai-oauth" {
        let _ = std::thread::Builder::new()
            .name("medusa-daemon-oauth-prewarm".to_owned())
            .spawn(|| {
                // Discovery checks the existing account without opening an OAuth browser.
                // A successful call keeps the initialized app-server available for the
                // first RuntimeController in this daemon process.
                let _ = medusa_runtime::discover_openai_oauth_models();
            });
    }
    server::serve_with_config(paths, config)
}
