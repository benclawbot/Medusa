#!/usr/bin/env python3
"""Black-box fixtures for scripts/check-lockfile-authority.py."""

from __future__ import annotations

import importlib.util
import tempfile
from pathlib import Path

SCRIPT = Path(__file__).with_name("check-lockfile-authority.py")
spec = importlib.util.spec_from_file_location("check_lockfile_authority", SCRIPT)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def write_workflow(root: Path, text: str) -> None:
    path = root / ".github/workflows/ci.yml"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def main() -> int:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        write_workflow(root, "steps:\n  - run: cargo metadata --locked --format-version 1\n")
        assert module.violations(root) == []

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        write_workflow(root, "steps:\n  - run: cargo generate-lockfile\n  - run: cargo test --locked\n")
        errors = module.violations(root)
        assert len(errors) == 1 and "cargo generate-lockfile" in errors[0]

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        write_workflow(root, "steps:\n  - run: cargo update\n  - run: cargo audit\n")
        errors = module.violations(root)
        assert len(errors) == 1 and "cargo update" in errors[0]

    print("lockfile authority fixtures passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
