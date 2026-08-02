#!/usr/bin/env python3
"""Adversarial tests for the unsafe-Rust boundary checker."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("check-unsafe-boundary.py")
SPEC = importlib.util.spec_from_file_location("check_unsafe_boundary", SCRIPT)
assert SPEC and SPEC.loader
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)


class Fixture:
    def __init__(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.write(
            "Cargo.toml",
            """
[workspace]
members = ["crates/safe", "crates/medusa-process-containment"]

[workspace.lints.rust]
unsafe_code = "forbid"
""".lstrip(),
        )
        self.write(
            "crates/safe/Cargo.toml",
            """
[package]
name = "safe"
version = "0.0.0"
edition = "2024"

[lints]
workspace = true
""".lstrip(),
        )
        self.write("crates/safe/src/lib.rs", 'pub fn label() -> &\'static str { "unsafe { ignored" }\n')
        self.write(
            "crates/medusa-process-containment/Cargo.toml",
            """
[package]
name = "medusa-process-containment"
version = "0.0.0"
edition = "2024"

[lints.rust]
unsafe_code = "deny"
""".lstrip(),
        )
        self.write(
            "crates/medusa-process-containment/src/lib.rs",
            """
mod safe_module;
#[cfg(windows)]
// SAFETY: fixture FFI.
#[allow(unsafe_code)]
mod windows;
""".lstrip(),
        )
        self.write(
            "crates/medusa-process-containment/src/safe_module.rs",
            "pub fn answer() -> u32 { 42 }\n",
        )
        self.write(
            "crates/medusa-process-containment/src/windows.rs",
            """
pub fn call() {
    // SAFETY: fixture pointer is non-null.
    unsafe { core::ptr::read_volatile(&0) };
}
""".lstrip(),
        )
        self.save_policy()

    def close(self) -> None:
        self.temp.cleanup()

    def write(self, relative: str, content: str) -> None:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def save_policy(self) -> None:
        policy = {
            "schema_version": 1,
            "containment_boundary": {
                "crate": "crates/medusa-process-containment",
                "files": [
                    {
                        "path": "crates/medusa-process-containment/src/lib.rs",
                        "module": "crate-root",
                        "classification": "safe",
                    },
                    {
                        "path": "crates/medusa-process-containment/src/safe_module.rs",
                        "module": "safe_module",
                        "classification": "safe",
                    },
                    {
                        "path": "crates/medusa-process-containment/src/windows.rs",
                        "module": "windows",
                        "classification": "unsafe-ffi",
                        "reason": "fixture",
                    },
                ],
            },
        }
        self.write(
            "docs/architecture/unsafe-rust-policy.json",
            json.dumps(policy, indent=2) + "\n",
        )


class UnsafeBoundaryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()

    def tearDown(self) -> None:
        self.fixture.close()

    def validate(self) -> None:
        CHECKER.validate(self.fixture.root)

    def test_valid_fixture_passes(self) -> None:
        self.validate()

    def test_unsafe_outside_allowlist_fails(self) -> None:
        self.fixture.write(
            "crates/safe/src/lib.rs",
            "pub unsafe fn escape() {}\n",
        )
        with self.assertRaisesRegex(
            CHECKER.UnsafeBoundaryError, "outside reviewed allowlist"
        ):
            self.validate()

    def test_new_containment_file_requires_policy_update(self) -> None:
        self.fixture.write(
            "crates/medusa-process-containment/src/new_safe.rs",
            "pub fn new_safe() {}\n",
        )
        with self.assertRaisesRegex(
            CHECKER.UnsafeBoundaryError, "source inventory drift"
        ):
            self.validate()

    def test_moved_allowlisted_file_fails(self) -> None:
        source = self.fixture.root / "crates/medusa-process-containment/src/windows.rs"
        target = source.with_name("ffi.rs")
        source.rename(target)
        with self.assertRaisesRegex(
            CHECKER.UnsafeBoundaryError, "policy path does not exist"
        ):
            self.validate()

    def test_missing_module_exception_fails(self) -> None:
        self.fixture.write(
            "crates/medusa-process-containment/src/lib.rs",
            "mod safe_module;\n#[cfg(windows)]\nmod windows;\n",
        )
        with self.assertRaisesRegex(
            CHECKER.UnsafeBoundaryError, "lacks a local"
        ):
            self.validate()

    def test_safe_module_cannot_receive_exception(self) -> None:
        self.fixture.write(
            "crates/medusa-process-containment/src/lib.rs",
            """
#[allow(unsafe_code)]
mod safe_module;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;
""".lstrip(),
        )
        with self.assertRaisesRegex(
            CHECKER.UnsafeBoundaryError, "safe module has"
        ):
            self.validate()

    def test_crate_wide_exception_fails(self) -> None:
        self.fixture.write(
            "crates/medusa-process-containment/src/lib.rs",
            """
#![allow(unsafe_code)]
mod safe_module;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;
""".lstrip(),
        )
        with self.assertRaisesRegex(
            CHECKER.UnsafeBoundaryError, "exactly one local"
        ):
            self.validate()

    def test_workspace_crate_cannot_drop_lint_inheritance(self) -> None:
        self.fixture.write(
            "crates/safe/Cargo.toml",
            """
[package]
name = "safe"
version = "0.0.0"
edition = "2024"
""".lstrip(),
        )
        with self.assertRaisesRegex(
            CHECKER.UnsafeBoundaryError, "must inherit workspace lints"
        ):
            self.validate()

    def test_containment_crate_must_use_deny(self) -> None:
        self.fixture.write(
            "crates/medusa-process-containment/Cargo.toml",
            """
[package]
name = "medusa-process-containment"
version = "0.0.0"
edition = "2024"

[lints.rust]
unsafe_code = "allow"
""".lstrip(),
        )
        with self.assertRaisesRegex(
            CHECKER.UnsafeBoundaryError, 'must set .* "deny"'
        ):
            self.validate()

    def test_comments_and_strings_do_not_count_as_unsafe(self) -> None:
        self.fixture.write(
            "crates/safe/src/lib.rs",
            """
// unsafe fn not_code() {}
pub const TEXT: &str = "unsafe { still not code";
/* unsafe impl Fake {} */
pub fn safe() {}
""".lstrip(),
        )
        self.validate()


if __name__ == "__main__":
    unittest.main()
