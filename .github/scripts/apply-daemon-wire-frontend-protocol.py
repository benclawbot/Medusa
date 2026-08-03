from __future__ import annotations

from pathlib import Path
import subprocess

EXPECTED_BLOBS = {
    "crates/medusa-daemon/src/protocol.rs": "0f9361c6fcd6002e1eba49d824a2264ab75a4bd8",
    "crates/medusa-daemon/src/server.rs": "acf8e1fab7b9f3d23aa844033be928b1979ec745",
    "crates/medusa-daemon/src/server/tests.rs": "521a8681d1fd983922ce4b850672a8e3e387e145",
    "crates/medusa-daemon/src/lib.rs": "db7efe5bc8436a56824c61e4cb08e2d9d25d549b",
    "crates/medusa-cli/src/main.rs": "bbc7acd2de3ed062b6e6fdc30100542f001617e0",
    "docs/architecture/INDEX.md": "39fd400c6ed7d8f0d5832c2efe55a7aecaca4b51",
    "docs/architecture/decisions/0007-canonical-frontend-projection.md": "ce35626f9b29ac7ed6daee9e067228ee7dd8ebfd",
}


def require_blob(path: str, expected: str) -> None:
    actual = subprocess.check_output(["git", "hash-object", path], text=True).strip()
    if actual != expected:
        raise SystemExit(f"{path}: expected blob {expected}, found {actual}")


def replace_once(text: str, old: str, new: str, path: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one replacement anchor, found {count}")
    return text.replace(old, new, 1)


for path, expected in EXPECTED_BLOBS.items():
    require_blob(path, expected)

protocol_path = Path("crates/medusa-daemon/src/protocol.rs")
protocol = protocol_path.read_text(encoding="utf-8")
protocol = replace_once(
    protocol,
    "use serde::{Deserialize, Serialize};\nuse time::OffsetDateTime;\n",
    "use medusa_protocol::frontend::FrontendCommandEnvelope;\nuse serde::{Deserialize, Serialize};\nuse time::OffsetDateTime;\n\nuse crate::frontend_control::FrontendCommandAcknowledgement;\n",
    str(protocol_path),
)
protocol = replace_once(
    protocol,
    "pub const DAEMON_PROTOCOL_VERSION: u16 = 1;",
    "pub const DAEMON_PROTOCOL_VERSION: u16 = 2;",
    str(protocol_path),
)
protocol = replace_once(
    protocol,
    "    List,\n    Shutdown,\n",
    "    List,\n    Frontend {\n        envelope: FrontendCommandEnvelope,\n    },\n    Shutdown,\n",
    str(protocol_path),
)
protocol = replace_once(
    protocol,
    "#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]\npub struct ResponseEnvelope",
    "#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]\npub struct ResponseEnvelope",
    str(protocol_path),
)
protocol = replace_once(
    protocol,
    "#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]\n#[serde(tag = \"type\", rename_all = \"snake_case\")]\npub enum Response",
    "#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]\n#[serde(tag = \"type\", rename_all = \"snake_case\")]\npub enum Response",
    str(protocol_path),
)
protocol = replace_once(
    protocol,
    "    Jobs { jobs: Vec<JobRecord> },\n    Ack,\n",
    "    Jobs { jobs: Vec<JobRecord> },\n    Frontend {\n        acknowledgement: FrontendCommandAcknowledgement,\n    },\n    Ack,\n",
    str(protocol_path),
)
protocol_path.write_text(protocol, encoding="utf-8")

server_path = Path("crates/medusa-daemon/src/server.rs")
server = server_path.read_text(encoding="utf-8")
server = replace_once(
    server,
    "use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};\nuse time::OffsetDateTime;\n",
    "use medusa_config::Config;\nuse medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};\nuse medusa_protocol::frontend::FrontendCommandEnvelope;\nuse time::OffsetDateTime;\n",
    str(server_path),
)
server = replace_once(
    server,
    "use crate::{\n    cancellation::{append_detail, cancel_all_jobs, cancel_job, mark_job_interrupted},\n",
    "use crate::{\n    cancellation::{append_detail, cancel_all_jobs, cancel_job, mark_job_interrupted},\n    frontend_control::{\n        FrontendCommandAcknowledgement, FrontendControlPlane, FrontendControlResult,\n    },\n",
    str(server_path),
)
server = replace_once(
    server,
    "        Ok(response.response)\n    }\n}\n\n/// Starts a daemon loop with production limits and blocks until shutdown.",
    "        Ok(response.response)\n    }\n\n    /// Sends one versioned frontend command through the repository-scoped daemon authority.\n    pub fn frontend(\n        &self,\n        envelope: FrontendCommandEnvelope,\n    ) -> MedusaResult<FrontendCommandAcknowledgement> {\n        match self.request(Request::Frontend { envelope })? {\n            Response::Frontend { acknowledgement } => Ok(acknowledgement),\n            Response::Error { code, message } => Err(MedusaError::new(\n                ErrorCode::DependencyUnavailable,\n                ErrorCategory::Environment,\n                format!(\"daemon frontend request failed ({code}): {message}\"),\n            )),\n            response => Err(MedusaError::new(\n                ErrorCode::InternalInvariant,\n                ErrorCategory::Internal,\n                format!(\"daemon returned an unexpected frontend response: {response:?}\"),\n            )),\n        }\n    }\n}\n\n/// Starts a daemon loop with production limits and blocks until shutdown.",
    str(server_path),
)
old_serve = '''pub fn serve(paths: DaemonPaths) -> MedusaResult<()> {
    serve_with_limits(paths, DaemonLimits::default())
}

/// Starts a daemon loop with explicit worker and queue limits.
pub fn serve_with_limits(paths: DaemonPaths, limits: DaemonLimits) -> MedusaResult<()> {
    fs::create_dir_all(&paths.directory)?;
    let _ownership = Ownership::acquire(&paths)?;
    let (jobs, recovered) = load_and_recover(&paths)?;
    if recovered {
        persist_jobs(&paths, &jobs)?;
    }
    let jobs = Arc::new(Mutex::new(jobs));
    let processes = Arc::new(ProcessRegistry::default());
    let listener = LocalListener::bind(&paths.socket).map_err(transport_error)?;
    let scheduler = match start_scheduler(&paths, &jobs, &processes, limits) {
        Ok(scheduler) => scheduler,
        Err(error) => {
            listener.cleanup();
            return Err(error);
        }
    };
    run_loop(
        listener,
        paths,
        jobs,
        processes,
        Arc::new(AtomicU8::new(SHUTDOWN_NONE)),
        scheduler,
    )
}
'''
new_serve = '''pub fn serve(paths: DaemonPaths) -> MedusaResult<()> {
    serve_with_config(paths, Config::default())
}

/// Starts a daemon loop with production limits and an explicit resolved configuration.
pub fn serve_with_config(paths: DaemonPaths, config: Config) -> MedusaResult<()> {
    serve_with_limits_and_config(paths, DaemonLimits::default(), config)
}

/// Starts a daemon loop with explicit worker and queue limits.
pub fn serve_with_limits(paths: DaemonPaths, limits: DaemonLimits) -> MedusaResult<()> {
    serve_with_limits_and_config(paths, limits, Config::default())
}

fn serve_with_limits_and_config(
    paths: DaemonPaths,
    limits: DaemonLimits,
    config: Config,
) -> MedusaResult<()> {
    fs::create_dir_all(&paths.directory)?;
    let _ownership = Ownership::acquire(&paths)?;
    let (jobs, recovered) = load_and_recover(&paths)?;
    if recovered {
        persist_jobs(&paths, &jobs)?;
    }
    let jobs = Arc::new(Mutex::new(jobs));
    let processes = Arc::new(ProcessRegistry::default());
    let frontend = Arc::new(Mutex::new(FrontendControlPlane::new(
        paths.repo.clone(),
        config,
    )));
    let listener = LocalListener::bind(&paths.socket).map_err(transport_error)?;
    let scheduler = match start_scheduler(&paths, &jobs, &processes, limits) {
        Ok(scheduler) => scheduler,
        Err(error) => {
            listener.cleanup();
            return Err(error);
        }
    };
    run_loop(
        listener,
        paths,
        jobs,
        processes,
        frontend,
        Arc::new(AtomicU8::new(SHUTDOWN_NONE)),
        scheduler,
    )
}
'''
server = replace_once(server, old_serve, new_serve, str(server_path))
old_spawn = '''pub fn spawn(
    paths: DaemonPaths,
) -> MedusaResult<(ServerHandle, thread::JoinHandle<MedusaResult<()>>)> {
    spawn_with_limits(paths, DaemonLimits::default())
}

/// Starts the server in a dedicated thread with explicit worker and queue limits.
pub fn spawn_with_limits(
    paths: DaemonPaths,
    limits: DaemonLimits,
) -> MedusaResult<(ServerHandle, thread::JoinHandle<MedusaResult<()>>)> {
'''
new_spawn = '''pub fn spawn(
    paths: DaemonPaths,
) -> MedusaResult<(ServerHandle, thread::JoinHandle<MedusaResult<()>>)> {
    spawn_with_config(paths, Config::default())
}

/// Starts the server in a dedicated thread with an explicit resolved configuration.
pub fn spawn_with_config(
    paths: DaemonPaths,
    config: Config,
) -> MedusaResult<(ServerHandle, thread::JoinHandle<MedusaResult<()>>)> {
    spawn_with_limits_and_config(paths, DaemonLimits::default(), config)
}

/// Starts the server in a dedicated thread with explicit worker and queue limits.
pub fn spawn_with_limits(
    paths: DaemonPaths,
    limits: DaemonLimits,
) -> MedusaResult<(ServerHandle, thread::JoinHandle<MedusaResult<()>>)> {
    spawn_with_limits_and_config(paths, limits, Config::default())
}

fn spawn_with_limits_and_config(
    paths: DaemonPaths,
    limits: DaemonLimits,
    config: Config,
) -> MedusaResult<(ServerHandle, thread::JoinHandle<MedusaResult<()>>)> {
'''
server = replace_once(server, old_spawn, new_spawn, str(server_path))
server = replace_once(
    server,
    "            let jobs = Arc::new(Mutex::new(jobs));\n            let processes = Arc::new(ProcessRegistry::default());\n            let listener = LocalListener::bind(&paths.socket).map_err(transport_error)?;\n",
    "            let jobs = Arc::new(Mutex::new(jobs));\n            let processes = Arc::new(ProcessRegistry::default());\n            let frontend = Arc::new(Mutex::new(FrontendControlPlane::new(\n                paths.repo.clone(),\n                config,\n            )));\n            let listener = LocalListener::bind(&paths.socket).map_err(transport_error)?;\n",
    str(server_path),
)
server = replace_once(
    server,
    "            run_loop(listener, paths, jobs, processes, server_shutdown, scheduler)\n",
    "            run_loop(\n                listener,\n                paths,\n                jobs,\n                processes,\n                frontend,\n                server_shutdown,\n                scheduler,\n            )\n",
    str(server_path),
)
server = replace_once(
    server,
    "    processes: Arc<ProcessRegistry>,\n    shutdown: Arc<AtomicU8>,\n",
    "    processes: Arc<ProcessRegistry>,\n    frontend: Arc<Mutex<FrontendControlPlane>>,\n    shutdown: Arc<AtomicU8>,\n",
    str(server_path),
)
server = replace_once(
    server,
    "                        handle_connection(stream, &paths, &jobs, &processes, &shutdown, &scheduler);\n",
    "                        handle_connection(\n                            stream,\n                            &paths,\n                            &jobs,\n                            &processes,\n                            &frontend,\n                            &shutdown,\n                            &scheduler,\n                        );\n",
    str(server_path),
)
server = replace_once(
    server,
    "    processes: &Arc<ProcessRegistry>,\n    shutdown: &Arc<AtomicU8>,\n",
    "    processes: &Arc<ProcessRegistry>,\n    frontend: &Arc<Mutex<FrontendControlPlane>>,\n    shutdown: &Arc<AtomicU8>,\n",
    str(server_path),
)
server = replace_once(
    server,
    "            processes,\n            shutdown,\n            scheduler,\n",
    "            processes,\n            frontend,\n            shutdown,\n            scheduler,\n",
    str(server_path),
)
server = replace_once(
    server,
    "    processes: &Arc<ProcessRegistry>,\n    shutdown: &Arc<AtomicU8>,\n    scheduler: &JobScheduler,\n) -> MedusaResult<Response> {\n",
    "    processes: &Arc<ProcessRegistry>,\n    frontend: &Arc<Mutex<FrontendControlPlane>>,\n    shutdown: &Arc<AtomicU8>,\n    scheduler: &JobScheduler,\n) -> MedusaResult<Response> {\n",
    str(server_path),
)
server = replace_once(
    server,
    "        Request::List => {\n            let locked = lock_jobs(jobs)?;\n            Ok(Response::Jobs {\n                jobs: locked.values().cloned().collect(),\n            })\n        }\n        Request::Shutdown => {\n",
    "        Request::List => {\n            let locked = lock_jobs(jobs)?;\n            Ok(Response::Jobs {\n                jobs: locked.values().cloned().collect(),\n            })\n        }\n        Request::Frontend { envelope } => {\n            let mut control = lock_frontend(frontend)?;\n            Ok(match control.dispatch(envelope) {\n                Ok(acknowledgement) => Response::Frontend { acknowledgement },\n                Err(error) => Response::Error {\n                    code: \"frontend_control\".to_owned(),\n                    message: error.to_string(),\n                },\n            })\n        }\n        Request::Shutdown => {\n",
    str(server_path),
)
server = replace_once(
    server,
    "pub(crate) fn lock_jobs(\n",
    "fn lock_frontend(\n    frontend: &Arc<Mutex<FrontendControlPlane>>,\n) -> MedusaResult<std::sync::MutexGuard<'_, FrontendControlPlane>> {\n    frontend.lock().map_err(|_| {\n        MedusaError::new(\n            ErrorCode::InternalInvariant,\n            ErrorCategory::Internal,\n            \"daemon frontend control lock was poisoned\",\n        )\n    })\n}\n\npub(crate) fn lock_jobs(\n",
    str(server_path),
)
server_path.write_text(server, encoding="utf-8")

tests_path = Path("crates/medusa-daemon/src/server/tests.rs")
tests = tests_path.read_text(encoding="utf-8")
tests = replace_once(
    tests,
    "use std::time::Instant;\n\nuse super::*;\n",
    "use std::time::Instant;\n\nuse medusa_protocol::frontend::{\n    FRONTEND_PROTOCOL_VERSION, FrontendCommand, FrontendCommandEnvelope, FrontendKind,\n};\n\nuse super::*;\n",
    str(tests_path),
)
tests = replace_once(
    tests,
    "#[test]\nfn client_reconnects_while_job_continues() {\n",
    '''#[test]
fn canonical_frontend_command_round_trips_over_daemon_wire() {
    let directory = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::for_repo(directory.path());
    let (handle, server) = spawn(paths.clone()).expect("spawn daemon");
    wait_for_endpoint(&paths.socket);
    let client = DaemonClient::new(&paths.socket);
    let envelope = FrontendCommandEnvelope {
        protocol_version: FRONTEND_PROTOCOL_VERSION,
        command_id: "desktop-list-1".to_owned(),
        idempotency_key: "desktop:list:1".to_owned(),
        frontend: FrontendKind::Desktop,
        client_id: "desktop-client".to_owned(),
        session_id: None,
        turn_id: None,
        timestamp: OffsetDateTime::now_utc(),
        command: FrontendCommand::ListSessions,
    };

    let first = client.frontend(envelope.clone()).expect("frontend request");
    let FrontendControlResult::Sessions { sessions } = &first.result else {
        panic!("expected sessions response")
    };
    assert!(sessions.is_empty());
    let duplicate = client.frontend(envelope).expect("idempotent replay");
    assert_eq!(first, duplicate);

    handle.shutdown();
    server.join().expect("join daemon").expect("daemon result");
}

#[test]
fn client_reconnects_while_job_continues() {
''',
    str(tests_path),
)
tests_path.write_text(tests, encoding="utf-8")

lib_path = Path("crates/medusa-daemon/src/lib.rs")
lib = lib_path.read_text(encoding="utf-8")
lib = replace_once(
    lib,
    "pub use server::{DaemonClient, ServerHandle, serve, serve_with_limits, spawn, spawn_with_limits};\n",
    "pub use server::{\n    DaemonClient, ServerHandle, serve, serve_with_config, serve_with_limits, spawn,\n    spawn_with_config, spawn_with_limits,\n};\n",
    str(lib_path),
)
lib_path.write_text(lib, encoding="utf-8")

cli_path = Path("crates/medusa-cli/src/main.rs")
cli = cli_path.read_text(encoding="utf-8")
cli = replace_once(
    cli,
    "use medusa_daemon::{DaemonClient, DaemonPaths, Request, serve};\n",
    "use medusa_daemon::{DaemonClient, DaemonPaths, Request, serve_with_config};\n",
    str(cli_path),
)
cli = replace_once(
    cli,
    "    if matches!(command, CommandKind::DaemonServe) {\n        return serve(DaemonPaths::for_repo(&repo));\n    }\n",
    "    if matches!(command, CommandKind::DaemonServe) {\n        let overrides = cli\n            .overrides\n            .iter()\n            .cloned()\n            .collect::<BTreeMap<_, _>>();\n        let config = Config::load_layers(None, None, &BTreeMap::new(), &overrides)?;\n        return serve_with_config(DaemonPaths::for_repo(&repo), config);\n    }\n",
    str(cli_path),
)
cli_path.write_text(cli, encoding="utf-8")

index_path = Path("docs/architecture/INDEX.md")
index = index_path.read_text(encoding="utf-8")
index = replace_once(
    index,
    "| Daemon service | `medusa __daemon-serve` | `crates/medusa-daemon` | daemon-owned runtime and continuity; canonical journal → frontend-scoped replay batches |",
    "| Daemon service | `medusa __daemon-serve` | `crates/medusa-daemon` | daemon protocol v2 routes shared frontend commands; canonical journal → frontend-scoped replay batches |",
    str(index_path),
)
index = replace_once(
    index,
    "Daemon attachment and replay now return the same frontend-scoped envelopes plus an explicit next canonical cursor that advances through non-presentable events. The TUI temporarily retains local settings, startup recovery, turn-counter, and reset hints; daemon wire integration, desktop, and remote voice surfaces remain follow-up slices.",
    "Daemon protocol v2 now routes the shared frontend command envelope through one daemon-owned control plane and returns typed acknowledgements plus frontend-scoped replay batches with a next canonical cursor that advances through non-presentable events. The TUI temporarily retains local settings, startup recovery, turn-counter, and reset hints; desktop and remote voice surfaces remain follow-up slices.",
    str(index_path),
)
index_path.write_text(index, encoding="utf-8")

adr_path = Path("docs/architecture/decisions/0007-canonical-frontend-projection.md")
adr = adr_path.read_text(encoding="utf-8")
adr = replace_once(
    adr,
    "The TUI keeps process-local settings, startup recovery, turn-counter, and reset hints only as bounded compatibility inputs while daemon wire integration is completed.",
    "Daemon protocol v2 exposes the shared frontend command envelope and typed acknowledgement through the repository-scoped local IPC server. The TUI keeps process-local settings, startup recovery, turn-counter, and reset hints only as bounded compatibility inputs while desktop migration is completed.",
    str(adr_path),
)
adr_path.write_text(adr, encoding="utf-8")
