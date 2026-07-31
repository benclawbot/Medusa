from pathlib import Path


def replace_once(source: str, old: str, new: str, label: str) -> str:
    if new in source:
        return source
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one target, found {count}")
    return source.replace(old, new, 1)


cargo_path = Path("crates/medusa-daemon/Cargo.toml")
cargo = cargo_path.read_text()
cargo = replace_once(
    cargo,
    'medusa-runtime = { path = "../medusa-runtime" }\n',
    'medusa-runtime = { path = "../medusa-runtime" }\nmedusa-session-continuity = { path = "../medusa-session-continuity" }\n',
    "continuity dependency",
)
cargo_path.write_text(cargo)

lib_path = Path("crates/medusa-daemon/src/lib.rs")
lib = lib_path.read_text()
lib = replace_once(
    lib,
    "mod lifecycle;\npub mod live_session;\n",
    "pub mod frontend_control;\nmod lifecycle;\npub mod live_session;\n",
    "frontend control module",
)
lib = replace_once(
    lib,
    '''pub use lifecycle::{DaemonLaunch, DaemonLifecycle, DaemonLifecycleState, DaemonSupervisor};
''',
    '''pub use frontend_control::{
    FrontendCommandAcknowledgement, FrontendControlError, FrontendControlPlane,
    FrontendControlResult,
};
pub use lifecycle::{DaemonLaunch, DaemonLifecycle, DaemonLifecycleState, DaemonSupervisor};
''',
    "frontend control exports",
)
lib_path.write_text(lib)

live_path = Path("crates/medusa-daemon/src/live_session.rs")
live = live_path.read_text()
live = replace_once(
    live,
    '''use medusa_protocol::EventEnvelope;
''',
    '''use medusa_protocol::EventEnvelope;
use medusa_session_continuity::{ContinuityError, ContinuityStore};
''',
    "continuity store imports",
)
method_target = '''    /// Attaches or refreshes one frontend client without allowing an implicit session switch.
'''
method = '''    /// Attaches using the latest durable continuity revision.
    ///
    /// The daemon serializes calls to this method. A concurrent external writer still produces a
    /// normal revision conflict rather than being overwritten.
    pub fn attach_current(
        &mut self,
        session_id: &str,
        client_id: String,
        client_kind: ClientKind,
        requested_mode: AttachmentMode,
        cursor: u64,
        occurred_at_unix_ms: i64,
        event_id: String,
    ) -> Result<LiveSessionAttachmentView, LiveSessionBrokerError> {
        let store = ContinuityStore::new(
            self.repo
                .join(".medusa/continuity")
                .join(format!("{session_id}.json")),
        );
        let expected_revision = match store.load() {
            Ok(continuity) => continuity.revision,
            Err(ContinuityError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                0
            }
            Err(error) => return Err(LiveSessionBrokerError::Session(error.to_string())),
        };
        self.attach(RuntimeAttachRequest {
            session_id: session_id.to_owned(),
            client_id,
            client_kind,
            requested_mode,
            expected_revision,
            cursor,
            occurred_at_unix_ms,
            event_id,
        })
    }

'''
if method not in live:
    if live.count(method_target) != 1:
        raise SystemExit("attach-current insertion target changed")
    live = live.replace(method_target, method + method_target, 1)
live_path.write_text(live)

runtime_path = Path("crates/medusa-runtime/src/lib.rs")
runtime = runtime_path.read_text()
runtime = replace_once(
    runtime,
    '''    #[must_use]
    pub fn is_busy(&self) -> bool {
        lock_submission(&self.submission).busy
    }
''',
    '''    #[must_use]
    pub fn is_busy(&self) -> bool {
        lock_submission(&self.submission).busy
    }

    /// Returns the durable session identity after a submission has been accepted.
    #[must_use]
    pub fn active_session_id(&self) -> Option<String> {
        lock_submission(&self.submission).active_session_id.clone()
    }
''',
    "active session identity",
)
runtime_path.write_text(runtime)
