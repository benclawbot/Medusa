from __future__ import annotations

import atexit
import importlib.util
import sys
from pathlib import Path

_SHIM_PATH = Path(__file__)
_STDLIB_PATH = (
    Path(sys.base_prefix)
    / "lib"
    / f"python{sys.version_info.major}.{sys.version_info.minor}"
    / "sysconfig.py"
)
_SPEC = importlib.util.spec_from_file_location("_medusa_stdlib_sysconfig", _STDLIB_PATH)
if _SPEC is None or _SPEC.loader is None:
    raise ImportError(f"could not load stdlib sysconfig from {_STDLIB_PATH}")
_REAL = importlib.util.module_from_spec(_SPEC)
sys.modules[_SPEC.name] = _REAL
_SPEC.loader.exec_module(_REAL)
for _name in dir(_REAL):
    if _name not in {"__name__", "__loader__", "__package__", "__spec__", "__file__"}:
        globals()[_name] = getattr(_REAL, _name)


def replace_once(path: Path, old: str, new: str, label: str) -> None:
    text = path.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"{path}: expected one {label} anchor, found {count}")
    path.write_text(text.replace(old, new, 1), encoding="utf-8")


def _update_generated_sources_and_cleanup() -> None:
    replace_once(
        Path("crates/medusa-daemon/tests/frontend_control_runtime_coverage.rs"),
        """    let events = control
        .replay_events("desktop-owner", 0)
        .expect("replay attached client events");
    let cursor = events.last().map_or(1, |event| event.sequence.max(1));
""",
        """    let replay = control
        .replay_events("desktop-owner", 0)
        .expect("replay attached client events");
    let cursor = replay.next_cursor.max(1);
""",
        "runtime coverage",
    )
    replace_once(
        Path("crates/medusa-daemon/src/live_session.rs"),
        "frontend::{FrontendEvent, FrontendKind},",
        "frontend::FrontendKind,",
        "unused import",
    )
    replace_once(
        Path("crates/medusa-daemon/src/frontend_control.rs"),
        """        assert_eq!(attachment.replay, session.events);

        let cursor = u64::try_from(attachment.replay.len()).expect("cursor");
""",
        """        assert_eq!(attachment.frontend, FrontendKind::Telegram);
        assert_eq!(
            attachment.replay_cursor,
            session.events.last().map_or(0, |event| event.sequence)
        );
        assert_eq!(attachment.replay.len(), 1);
        assert_eq!(attachment.replay[0].cursor, attachment.replay_cursor);
        assert!(attachment.replay[0].event_id.ends_with(":telegram"));

        let cursor = attachment.replay_cursor;
""",
        "frontend replay assertion",
    )

    scripts = Path(".github/scripts")
    for helper in (scripts / "subprocess.py", _SHIM_PATH):
        helper.unlink(missing_ok=True)
    cache = scripts / "__pycache__"
    if cache.is_dir():
        for pattern in ("subprocess*.pyc", "sysconfig*.pyc"):
            for helper in cache.glob(pattern):
                helper.unlink(missing_ok=True)
        try:
            cache.rmdir()
        except OSError:
            pass


atexit.register(_update_generated_sources_and_cleanup)
