from pathlib import Path

path = Path(".github/scripts/apply-daemon-wire-frontend-protocol.py")
text = path.read_text(encoding="utf-8")
old = '''server = replace_once(
    server,
    "    processes: &Arc<ProcessRegistry>,\\n    shutdown: &Arc<AtomicU8>,\\n",
    "    processes: &Arc<ProcessRegistry>,\\n    frontend: &Arc<Mutex<FrontendControlPlane>>,\\n    shutdown: &Arc<AtomicU8>,\\n",
    str(server_path),
)
'''
new = '''server = replace_once(
    server,
    "fn handle_connection(\\n    mut stream: LocalStream,\\n    paths: &DaemonPaths,\\n    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,\\n    processes: &Arc<ProcessRegistry>,\\n    shutdown: &Arc<AtomicU8>,\\n",
    "fn handle_connection(\\n    mut stream: LocalStream,\\n    paths: &DaemonPaths,\\n    jobs: &Arc<Mutex<BTreeMap<String, JobRecord>>>,\\n    processes: &Arc<ProcessRegistry>,\\n    frontend: &Arc<Mutex<FrontendControlPlane>>,\\n    shutdown: &Arc<AtomicU8>,\\n",
    str(server_path),
)
'''
count = text.count(old)
if count != 1:
    raise SystemExit(f"expected one ambiguous connection anchor block, found {count}")
path.write_text(text.replace(old, new, 1), encoding="utf-8")
