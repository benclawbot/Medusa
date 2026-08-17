#!/usr/bin/env python3
"""Deterministic tests for changed-rust-packages.py."""

from __future__ import annotations

import importlib.util
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "changed-rust-packages.py"

spec = importlib.util.spec_from_file_location("changed_rust_packages", SCRIPT)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def metadata() -> dict:
    return {
        "packages": [
            {"name": "alpha", "manifest_path": str(ROOT / "crates" / "alpha" / "Cargo.toml")},
            {"name": "beta", "manifest_path": str(ROOT / "crates" / "beta" / "Cargo.toml")},
            {
                "name": "desktop-shell",
                "manifest_path": str(ROOT / "apps" / "desktop" / "src-tauri" / "Cargo.toml"),
            },
        ]
    }


def assert_selected(changed: list[str], expected: list[str]) -> None:
    actual = module.select_packages(metadata(), changed)
    assert actual == expected, (changed, actual, expected)


def main() -> int:
    assert_selected(["crates/alpha/src/lib.rs"], ["alpha"])
    assert_selected(["crates/beta/build.rs", "docs/README.md"], ["beta"])
    assert_selected(["apps/desktop/src-tauri/src/main.rs"], ["desktop-shell"])
    assert_selected(["docs/README.md", "scripts/tool.py"], [])
    assert_selected(["Cargo.lock"], ["alpha", "beta", "desktop-shell"])
    assert_selected([".cargo/config.toml"], ["alpha", "beta", "desktop-shell"])

    with tempfile.TemporaryDirectory() as temporary:
        changed = Path(temporary) / "changed.txt"
        changed.write_text("crates/alpha/src/lib.rs\n", encoding="utf-8")
        loaded = module.load_changed_files(None, changed)
        assert loaded == ["crates/alpha/src/lib.rs"]

    print("changed-rust-packages tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
