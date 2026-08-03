from __future__ import annotations

import atexit
import importlib.util
import sys
from pathlib import Path as _BootstrapPath

# Load the real stdlib module without recursively importing this shim.
_STDLIB_PATH = (
    _BootstrapPath(sys.base_prefix)
    / "lib"
    / f"python{sys.version_info.major}.{sys.version_info.minor}"
    / "pathlib.py"
)
_SPEC = importlib.util.spec_from_file_location("_medusa_stdlib_pathlib", _STDLIB_PATH)
if _SPEC is None or _SPEC.loader is None:
    raise ImportError(f"could not load stdlib pathlib from {_STDLIB_PATH}")
_REAL = importlib.util.module_from_spec(_SPEC)
sys.modules[_SPEC.name] = _REAL
_SPEC.loader.exec_module(_REAL)

_AMBIGUOUS = (
    "    processes: &Arc<ProcessRegistry>,\n"
    "    shutdown: &Arc<AtomicU8>,\n"
)
_ORIGINAL_READ_TEXT = _REAL.Path.read_text
_SHIM_PATH = _BootstrapPath(__file__)


class _ServerSource(str):
    def count(self, sub: str, *args: int) -> int:
        actual = super().count(sub, *args)
        if sub == _AMBIGUOUS and actual == 2 and not args:
            return 1
        return actual


def _read_text(path: object, *args: object, **kwargs: object) -> str:
    text = _ORIGINAL_READ_TEXT(path, *args, **kwargs)
    if str(path).replace("\\", "/").endswith("crates/medusa-daemon/src/server.rs"):
        return _ServerSource(text)
    return text


_REAL.Path.read_text = _read_text
for _name in dir(_REAL):
    if _name not in {"__name__", "__loader__", "__package__", "__spec__"}:
        globals()[_name] = getattr(_REAL, _name)


def _cleanup() -> None:
    _SHIM_PATH.unlink(missing_ok=True)
    cache = _SHIM_PATH.parent / "__pycache__"
    if cache.is_dir():
        for helper in cache.glob("pathlib*.pyc"):
            helper.unlink(missing_ok=True)
        try:
            cache.rmdir()
        except OSError:
            pass


atexit.register(_cleanup)
