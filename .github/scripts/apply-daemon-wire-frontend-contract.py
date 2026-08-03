from __future__ import annotations

from pathlib import Path
import subprocess

EXPECTED_BLOBS = {
    "crates/medusa-daemon/src/protocol.rs": "0f9361c6fcd6002e1eba49d824a2264ab75a4bd8",
    "crates/medusa-daemon/src/server.rs": "acf8e1fab7b9f3d23aa844033be928b1979ec745",
    "crates/medusa-daemon/src/server/tests.rs": "521a8681d1fd983922ce4b850672a8e3e387e145",
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
    "use medusa_protocol::frontend::FrontendCommandEnvelope;\n"
    "use serde::{Deserialize, Serialize};\n"
    "use time::OffsetDateTime;\n\n"
    "use crate::frontend_control::FrontendCommandAcknowledgement;\n",
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
    "    List,\n    Shutdown,",
    "    List,\n"
    "    Frontend {\n"
    "        command: Box<FrontendCommandEnvelope>,\n"
    "    },\n"
    "    Shutdown,",
    str(protocol_path),
)
protocol = replace_once(
    protocol,
    "#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]\n"
    "pub struct ResponseEnvelope {",
    "#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]\n"
    "pub struct ResponseEnvelope {",
    str(protocol_path),
)
protocol = replace_once(
    protocol,
    "#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]\n"
    "#[serde(tag = \"type\", rename_all = \"snake_case\")]\n"
    "pub enum Response {",
    "#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]\n"
    "#[serde(tag = \"type\", rename_all = \"snake_case\")]\n"
    "pub enum Response {",
    str(protocol_path),
)
protocol = replace_once(
    protocol,
    "    Jobs { jobs: Vec<JobRecord> },\n    Ack,",
    "    Jobs { jobs: Vec<JobRecord> },\n"
    "    Frontend {\n"
    "        acknowledgement: Box<FrontendCommandAcknowledgement>,\n"
    "    },\n"
    "    Ack,",
    str(protocol_path),
)
protocol_path.write_text(protocol, encoding="utf-8")

server_path = Path("crates/medusa-daemon/src/server.rs")
server = server_path.read_text(encoding="utf-8")
server = replace_once(
    server,
    "use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};\n",
    "use medusa_config::Config;\n"
    "use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};\n"
    "use medusa_protocol::frontend::FrontendCommandEnvelope;\n",
    str(server_path),
)
server = replace_once(
    server,
    "use crate::{\n"
    "    cancellation::{append_detail, cancel_all_jobs, cancel_job, mark_job_interrupted},\n",
    "use crate::{\n"
    "    cancellation::{append_detail, cancel_all_jobs, cancel_job, mark_job_interrupted},\n"
    "    frontend_control::{FrontendCommandAcknowledgement, FrontendControlPlane},\n",
    str(server_path),
)
server = replace_once(
    server,
    "        Ok(response.response)\n"
    "    }\n"
    "}\n\n"
    "/// Starts a daemon loop with production limits and blocks until shutdown.",
    "        Ok(response.response)\n"
    "    }\n\n"
    "    /// Sends one versioned frontend command over the daemon socket.\n"
    "    pub fn frontend_command(\n"
    "        &self,\n"
    "        command: FrontendCommandEnvelope,\n"
    "    ) -> MedusaResult<FrontendCommandAcknowledgement> {\n"
    "        let command_id = command.command_id.clone();\n"
    "        match self.request(Request::Frontend {\n"
    "            command: Box::new(command),\n"
    "        })? {\n"
    "            Response::Frontend { acknowledgement } => Ok(*acknowledgement),\n"
    "            Response::Error { code, message } => Err(MedusaError::new(\n"
    "                ErrorCode::InvalidConfiguration,\n"
    "                ErrorCategory::Validation,\n"
    "                format!(\n"
    "                    \"daemon frontend command {command_id} was rejected ({code}): {message}\"\n"
    "                ),\n"
    "            )),\n"
    "            response => Err(MedusaError::new(\n"
    "                ErrorCode::IncompatibleProtocol,\n"
    "                ErrorCategory::Validation,\n"
    "                format!(\n"
    "                    \"daemon returned an unexpected response to frontend command {command_id}: {response:?}\"\n"
    "                ),\n"
    "            )),\n"
    "        }\n"
    "    }\n"
    "}\n\n"
    "/// Starts a daemon loop with production limits and blocks until shutdown.",
    str(server_path),
)
server = replace_once(
    server,
    "    let jobs = Arc::new(Mutex::new(jobs));\n"
    "    let processes = Arc::new(ProcessRegistry::default());\n"
    "    let listener = LocalListener::bind(&paths.socket).map_err(transport_error)?;",
    "    let jobs = Arc::new(Mutex::new(jobs));\n"
    "    let processes = Arc::new(ProcessRegistry::default());\n"
    "    let frontend = load_frontend_control(&paths)?;\n"
    "    let listener = LocalListener::bind(&paths.socket).map_err(transport_error)?;",
    str(server_path),
)
server = replace_once(
    server,
    "        processes,\n"
    "        Arc::new(AtomicU8::new(SHUTDOWN_NONE)),\n"
    "        scheduler,\n",
    "        processes,\n"
    "        frontend,\n"
    "        Arc::new(AtomicU8::new(SHUTDOWN_NONE)),\n"
    "        scheduler,\n",
    str(server_path),
)
server = replace_once(
    server,
    "            let jobs = Arc::new(Mutex::new(jobs));\n"
    "            let processes = Arc::new(ProcessRegistry::default());\n"
    "            let listener = LocalListener::bind(&paths.socket).map_err(transport_error)?;",
    "            let jobs = Arc::new(Mutex::new(jobs));\n"
    "            let processes = Arc::new(ProcessRegistry::default());\n"
    "            let frontend = load_frontend_control(&paths)?;\n"
    "            let listener = LocalListener::bind(&paths.socket).map_err(transport_error)?;",
    str(server_path),
)
server = replace_once(
    server,
    "            run_loop(listener, paths, jobs, processes, server_shutdown, scheduler)\n",
    "            run_loop(\n"
    "                listener,\n"
    "                paths,\n"
    "                jobs,\n"
    "                processes,\n"
    "                frontend,\n"
    "                server_shutdown,\n"
    "                scheduler,\n"
    "            )\n",
    str(server_path),
)
server = replace_once(
    server,
    "fn start_scheduler(\n",
    "fn load_frontend_control(\n"
    "    paths: &DaemonPaths,\n"
    ") -> MedusaResult<Arc<Mutex<FrontendControlPlane>>> {\n"
    "    let project = paths.repo.join(\".medusa/config.toml\");\n"
    "    let project = project.exists().then_some(project);\n"
    "    let config = Config::load_layers(\n"
    "        None,\n"
    "        project.as_deref(),\n"
    "        &BTreeMap::new(),\n"
    "        &BTreeMap::new(),\n"
    "    )?;\n"
    "    Ok(Arc::new(Mutex::new(FrontendControlPlane::new(\n"
    "        paths.repo.clone(),\n"
    "        config,\n"
    "    ))))\n"
    "}\n\n"
    "fn start_scheduler(\n",
    str(server_path),
)
server = replace_once(
    server,
    "    processes: Arc<ProcessRegistry>,\n"
    "    shutdown: Arc<AtomicU8>,\n",
    "    processes: Arc<ProcessRegistry>,\n"
    "    frontend: Arc<Mutex<FrontendControlPlane>>,\n"
    "    shutdown: Arc<AtomicU8>,\n",
    str(server_path),
)
server = replace_once(
    server,
    "                        handle_connection(stream, &paths, &jobs, &processes, &shutdown, &scheduler);\n",
    "                        handle_connection(\n"
    "                            stream,\n"
    "                            &paths,\n"
    "                            &jobs,\n"
    "                            &processes,\n"
    "                            &frontend,\n"
    "                            &shutdown,\n"
    "                            &scheduler,\n"
    "                        );\n",
    str(server_path),
)
server = replace_once(
    server,
    "    processes: &Arc<ProcessRegistry>,\n"
    "    shutdown: &Arc<AtomicU8>,\n",
    "    processes: &Arc<ProcessRegistry>,\n"
    "    frontend: &Arc<Mutex<FrontendControlPlane>>,\n"
    "    shutdown: &Arc<AtomicU8>,\n",
    str(server_path),
)
server = replace_once(
    server,
    "            processes,\n"
    "            shutdown,\n"
    "            scheduler,\n",
    "            processes,\n"
    "            frontend,\n"
    "            shutdown,\n"
    "            scheduler,\n",
    str(server_path),
)
server = replace_once(
    server,
    "    processes: &Arc<ProcessRegistry>,\n"
    "    shutdown: &Arc<AtomicU8>,\n"
    "    scheduler: &JobScheduler,\n"
    ") -> MedusaResult<Response> {",
    "    processes: &Arc<ProcessRegistry>,\n"
    "    frontend: &Arc<Mutex<FrontendControlPlane>>,\n"
    "    shutdown: &Arc<AtomicU8>,\n"
    "    scheduler: &JobScheduler,\n"
    ") -> MedusaResult<Response> {",
    str(server_path),
)
server = replace_once(
    server,
    "        Request::List => {\n"
    "            let locked = lock_jobs(jobs)?;\n"
    "            Ok(Response::Jobs {\n"
    "                jobs: locked.values().cloned().collect(),\n"
    "            })\n"
    "        }\n"
    "        Request::Shutdown => {",
    "        Request::List => {\n"
    "            let locked = lock_jobs(jobs)?;\n"
    "            Ok(Response::Jobs {\n"
    "                jobs: locked.values().cloned().collect(),\n"
    "            })\n"
    "        }\n"
    "        Request::Frontend { command } => {\n"
    "            let mut control = lock_frontend_control(frontend)?;\n"
    "            match control.dispatch(*command) {\n"
    "                Ok(acknowledgement) => Ok(Response::Frontend {\n"
    "                    acknowledgement: Box::new(acknowledgement),\n"
    "                }),\n"
    "                Err(error) => Ok(Response::Error {\n"
    "                    code: \"frontend_command_rejected\".to_owned(),\n"
    "                    message: error.to_string(),\n"
    "                }),\n"
    "            }\n"
    "        }\n"
    "        Request::Shutdown => {",
    str(server_path),
)
server = replace_once(
    server,
    "fn transport_error(error: impl std::fmt::Display) -> MedusaError {\n",
    "fn lock_frontend_control(\n"
    "    frontend: &Arc<Mutex<FrontendControlPlane>>,\n"
    ") -> MedusaResult<std::sync::MutexGuard<'_, FrontendControlPlane>> {\n"
    "    frontend.lock().map_err(|_| {\n"
    "        MedusaError::new(\n"
    "            ErrorCode::InternalInvariant,\n"
    "            ErrorCategory::Internal,\n"
    "            \"daemon frontend control lock was poisoned\",\n"
    "        )\n"
    "    })\n"
    "}\n\n"
    "fn transport_error(error: impl std::fmt::Display) -> MedusaError {\n",
    str(server_path),
)
server_path.write_text(server, encoding="utf-8")

tests_path = Path("crates/medusa-daemon/src/server/tests.rs")
tests = tests_path.read_text(encoding="utf-8")
tests = replace_once(
    tests,
    "use std::time::Instant;\n\nuse super::*;\n",
    "use std::time::Instant;\n\n"
    "use medusa_protocol::frontend::{\n"
    "    FRONTEND_PROTOCOL_VERSION, FrontendCommand, FrontendCommandEnvelope, FrontendKind,\n"
    "};\n"
    "use time::macros::datetime;\n\n"
    "use super::*;\n",
    str(tests_path),
)
tests = replace_once(
    tests,
    "fn submit_job(client: &DaemonClient, command: (String, Vec<String>)) -> JobRecord {\n",
    "fn frontend_envelope(\n"
    "    command_id: &str,\n"
    "    idempotency_key: &str,\n"
    "    command: FrontendCommand,\n"
    ") -> FrontendCommandEnvelope {\n"
    "    FrontendCommandEnvelope {\n"
    "        protocol_version: FRONTEND_PROTOCOL_VERSION,\n"
    "        command_id: command_id.to_owned(),\n"
    "        idempotency_key: idempotency_key.to_owned(),\n"
    "        frontend: FrontendKind::Desktop,\n"
    "        client_id: \"desktop-wire-test\".to_owned(),\n"
    "        session_id: None,\n"
    "        turn_id: None,\n"
    "        timestamp: datetime!(2026-08-03 17:00 UTC),\n"
    "        command,\n"
    "    }\n"
    "}\n\n"
    "fn submit_job(client: &DaemonClient, command: (String, Vec<String>)) -> JobRecord {\n",
    str(tests_path),
)
tests = replace_once(
    tests,
    "#[test]\nfn daemon_paths_remain_repository_scoped() {",
    "#[test]\n"
    "fn frontend_commands_round_trip_across_reconnecting_socket_clients() {\n"
    "    let directory = tempfile::tempdir().expect(\"tempdir\");\n"
    "    let paths = DaemonPaths::for_repo(directory.path());\n"
    "    let (handle, server) = spawn(paths.clone()).expect(\"spawn daemon\");\n"
    "    wait_for_endpoint(&paths.socket);\n\n"
    "    let command = frontend_envelope(\n"
    "        \"desktop-list-1\",\n"
    "        \"desktop-wire:list\",\n"
    "        FrontendCommand::ListSessions,\n"
    "    );\n"
    "    let first = DaemonClient::new(&paths.socket)\n"
    "        .frontend_command(command.clone())\n"
    "        .expect(\"list sessions over daemon wire\");\n"
    "    let FrontendControlResult::Sessions { sessions } = &first.result else {\n"
    "        panic!(\"expected session list acknowledgement\")\n"
    "    };\n"
    "    assert!(sessions.is_empty());\n"
    "    drop(first);\n\n"
    "    let second_client = DaemonClient::new(&paths.socket);\n"
    "    let replayed = second_client\n"
    "        .frontend_command(command)\n"
    "        .expect(\"replay idempotent acknowledgement after reconnect\");\n"
    "    let FrontendControlResult::Sessions { sessions } = replayed.result else {\n"
    "        panic!(\"expected replayed session list acknowledgement\")\n"
    "    };\n"
    "    assert!(sessions.is_empty());\n\n"
    "    let conflict = frontend_envelope(\n"
    "        \"desktop-create-1\",\n"
    "        \"desktop-wire:list\",\n"
    "        FrontendCommand::CreateSession {\n"
    "            repository_profile: \"default\".to_owned(),\n"
    "            objective: Some(\"must not execute\".to_owned()),\n"
    "        },\n"
    "    );\n"
    "    let Response::Error { code, message } = second_client\n"
    "        .request(Request::Frontend {\n"
    "            command: Box::new(conflict),\n"
    "        })\n"
    "        .expect(\"receive idempotency conflict\")\n"
    "    else {\n"
    "        panic!(\"expected frontend command rejection\")\n"
    "    };\n"
    "    assert_eq!(code, \"frontend_command_rejected\");\n"
    "    assert!(message.contains(\"idempotency\"));\n\n"
    "    handle.shutdown();\n"
    "    server.join().expect(\"join daemon\").expect(\"daemon result\");\n"
    "}\n\n"
    "#[test]\nfn daemon_paths_remain_repository_scoped() {",
    str(tests_path),
)
tests_path.write_text(tests, encoding="utf-8")

index_path = Path("docs/architecture/INDEX.md")
index = index_path.read_text(encoding="utf-8")
index = replace_once(
    index,
    "| Daemon service | `medusa __daemon-serve` | `crates/medusa-daemon` | daemon-owned runtime and continuity; canonical journal → frontend-scoped replay batches |",
    "| Daemon service | `medusa __daemon-serve` | `crates/medusa-daemon` | daemon protocol v2 carries shared frontend commands to one daemon-owned runtime/continuity control plane and returns frontend-scoped replay batches |",
    str(index_path),
)
index = replace_once(
    index,
    "The phase-6 frontend migration is proceeding in production-entrypoint order. Headless CLI and interactive TUI output tail committed session-journal events through `medusa-protocol::frontend`. Daemon attachment and replay now return the same frontend-scoped envelopes plus an explicit next canonical cursor that advances through non-presentable events. The TUI temporarily retains local settings, startup recovery, turn-counter, and reset hints; daemon wire integration, desktop, and remote voice surfaces remain follow-up slices.",
    "The phase-6 frontend migration is proceeding in production-entrypoint order. Headless CLI and interactive TUI output tail committed session-journal events through `medusa-protocol::frontend`. Daemon protocol v2 now carries the shared frontend command envelope over the generic local socket into one reconnect-stable control plane and returns typed acknowledgements with frontend-scoped replay. The TUI temporarily retains local settings, startup recovery, turn-counter, and reset hints; desktop and remaining remote voice surfaces remain follow-up slices.",
    str(index_path),
)
index_path.write_text(index, encoding="utf-8")

adr_path = Path("docs/architecture/decisions/0007-canonical-frontend-projection.md")
adr = adr_path.read_text(encoding="utf-8")
adr = replace_once(
    adr,
    "The headless CLI and interactive TUI consume the canonical stream for durable transcript, plan, question, activity, usage, cancellation, failure, and completion state. Daemon attachments and replay project the same journal range according to each attached frontend kind and expose a next canonical cursor even when every scanned event is non-presentable. Telegram delivery consumes those daemon-projected envelopes directly and acknowledges the batch cursor after hidden events, rather than re-projecting raw journal payloads. The TUI keeps process-local settings, startup recovery, turn-counter, and reset hints only as bounded compatibility inputs while daemon wire integration is completed.",
    "The headless CLI and interactive TUI consume the canonical stream for durable transcript, plan, question, activity, usage, cancellation, failure, and completion state. Daemon attachments and replay project the same journal range according to each attached frontend kind and expose a next canonical cursor even when every scanned event is non-presentable. Daemon protocol v2 carries boxed `FrontendCommandEnvelope`s over the generic local socket and returns typed acknowledgements from one daemon-owned control plane that survives client reconnects. Telegram delivery consumes those daemon-projected envelopes directly and acknowledges the batch cursor after hidden events, rather than re-projecting raw journal payloads. The TUI keeps process-local settings, startup recovery, turn-counter, and reset hints only as bounded compatibility inputs while desktop migration is completed.",
    str(adr_path),
)
adr_path.write_text(adr, encoding="utf-8")
