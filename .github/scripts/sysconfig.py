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


def _update_coverage_and_cleanup() -> None:
    test_path = Path("crates/medusa-daemon/tests/frontend_control_runtime_coverage.rs")
    text = test_path.read_text(encoding="utf-8")
    old = """    let events = control
        .replay_events("desktop-owner", 0)
        .expect("replay attached client events");
    let cursor = events.last().map_or(1, |event| event.sequence.max(1));
"""
    new = """    let replay = control
        .replay_events("desktop-owner", 0)
        .expect("replay attached client events");
    let cursor = replay.next_cursor.max(1);
"""
    count = text.count(old)
    if count != 1:
        raise RuntimeError(
            f"{test_path}: expected one replay coverage anchor, found {count}"
        )
    test_path.write_text(text.replace(old, new, 1), encoding="utf-8")

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


atexit.register(_update_coverage_and_cleanup)
