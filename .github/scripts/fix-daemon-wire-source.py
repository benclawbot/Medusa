from pathlib import Path


def replace_once(path: Path, old: str, new: str) -> None:
    text = path.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one source correction anchor, found {count}")
    path.write_text(text.replace(old, new, 1), encoding="utf-8")


replace_once(
    Path("crates/medusa-daemon/src/server.rs"),
    "        FrontendCommandAcknowledgement, FrontendControlPlane, FrontendControlResult,\n",
    "        FrontendCommandAcknowledgement, FrontendControlPlane,\n",
)
replace_once(
    Path("crates/medusa-cli/src/main.rs"),
    "use medusa_daemon::{DaemonClient, DaemonPaths, Request, serve_with_config};\n",
    "use medusa_daemon::{DaemonClient, DaemonPaths, Request, serve, serve_with_config};\n",
)
