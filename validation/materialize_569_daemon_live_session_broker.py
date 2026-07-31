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
    "medusa-core.workspace = true\n",
    "medusa-agent = { path = \"../medusa-agent\" }\nmedusa-core.workspace = true\n",
    "daemon agent dependency",
)
cargo = replace_once(
    cargo,
    "medusa-recovery-coordinator = { path = \"../medusa-recovery-coordinator\" }\n",
    "medusa-recovery-coordinator = { path = \"../medusa-recovery-coordinator\" }\nmedusa-runtime = { path = \"../medusa-runtime\" }\n",
    "daemon runtime dependency",
)
cargo = replace_once(
    cargo,
    "[dev-dependencies]\nmedusa-testkit.workspace = true\n",
    "[dev-dependencies]\nmedusa-config = { path = \"../medusa-config\" }\nmedusa-provider = { path = \"../medusa-provider\" }\nmedusa-testkit.workspace = true\n",
    "daemon broker test dependencies",
)
cargo_path.write_text(cargo)

lib_path = Path("crates/medusa-daemon/src/lib.rs")
lib = lib_path.read_text()
lib = replace_once(
    lib,
    "mod lifecycle;\n",
    "mod lifecycle;\npub mod live_session;\n",
    "daemon broker module",
)
lib = replace_once(
    lib,
    "pub use lifecycle::{DaemonLaunch, DaemonLifecycle, DaemonLifecycleState, DaemonSupervisor};\n",
    "pub use lifecycle::{DaemonLaunch, DaemonLifecycle, DaemonLifecycleState, DaemonSupervisor};\npub use live_session::{\n    LiveSessionAttachmentView, LiveSessionBroker, LiveSessionBrokerError, LiveSessionSummary,\n};\n",
    "daemon broker exports",
)
lib_path.write_text(lib)

session_path = Path("crates/medusa-runtime/src/attachment/session.rs")
session = session_path.read_text()
target = '''    /// Starts the production controller only when this client is the current owner.
'''
method = '''    /// Reloads durable continuity metadata after another client changes ownership or cursor state.
    pub fn refresh_continuity(&mut self) -> Result<(), RuntimeError> {
        let continuity = continuity_store(&self.repo, &self.session.id.to_string())
            .load()
            .map_err(RuntimeError::agent)?;
        validate_continuity_identity(&continuity, &self.session.id.to_string())?;
        let mode = continuity
            .attachments
            .iter()
            .find(|attachment| attachment.client_id == self.client_id)
            .map(|attachment| attachment.mode)
            .ok_or_else(|| RuntimeError::agent("client is no longer attached"))?;
        self.mode = mode;
        self.continuity = continuity;
        Ok(())
    }

'''
if method not in session:
    if session.count(target) != 1:
        raise SystemExit("continuity refresh insertion target changed")
    session = session.replace(target, method + target, 1)
session_path.write_text(session)
