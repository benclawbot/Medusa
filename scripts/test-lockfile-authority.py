#!/usr/bin/env python3
"""Black-box fixtures for committed Cargo.lock authority."""

from __future__ import annotations

import importlib.util
import subprocess
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


def prove_stale_lock_fails() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        (root / "src").mkdir()
        (root / "src/lib.rs").write_text("pub fn value() -> u8 { 1 }\n", encoding="utf-8")
        (root / "Cargo.toml").write_text(
            '[package]\nname = "lock-authority-fixture"\nversion = "0.1.0"\nedition = "2021"\n',
            encoding="utf-8",
        )
        initial = subprocess.run(
            ["cargo", "generate-lockfile"], cwd=root, check=False, capture_output=True, text=True
        )
        assert initial.returncode == 0, initial.stderr
        committed_lock = (root / "Cargo.lock").read_bytes()

        dep = root / "fixture-dep"
        (dep / "src").mkdir(parents=True)
        (dep / "src/lib.rs").write_text("pub fn dep() -> u8 { 2 }\n", encoding="utf-8")
        (dep / "Cargo.toml").write_text(
            '[package]\nname = "fixture-dep"\nversion = "0.1.0"\nedition = "2021"\n',
            encoding="utf-8",
        )
        (root / "Cargo.toml").write_text(
            '[package]\nname = "lock-authority-fixture"\nversion = "0.1.0"\nedition = "2021"\n'
            '\n[dependencies]\nfixture-dep = { path = "fixture-dep" }\n',
            encoding="utf-8",
        )

        stale = subprocess.run(
            ["cargo", "metadata", "--locked", "--format-version", "1"],
            cwd=root,
            check=False,
            capture_output=True,
            text=True,
        )
        assert stale.returncode != 0, "stale Cargo.lock unexpectedly passed --locked metadata"
        assert (root / "Cargo.lock").read_bytes() == committed_lock, "locked validation mutated Cargo.lock"


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

    prove_stale_lock_fails()
    print("lockfile authority fixtures passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
