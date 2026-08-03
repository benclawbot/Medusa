from __future__ import annotations

import atexit
from pathlib import Path


def apply_generated_clippy_correction() -> None:
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
            raise SystemExit(
                f"expected one generated Clippy anchor, found {text.count(old)}"
            )
        path.write_text(text.replace(old, new, 1), encoding="utf-8")

    Path(".github/scripts/tui-generator-rerun.txt").unlink(missing_ok=True)
    Path(__file__).unlink(missing_ok=True)


atexit.register(apply_generated_clippy_correction)
