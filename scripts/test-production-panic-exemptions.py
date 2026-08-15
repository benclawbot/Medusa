#!/usr/bin/env python3
"""Fixture tests for production panic-lint exemption policy."""

from __future__ import annotations

import importlib.util
from pathlib import Path


SCRIPT = Path(__file__).with_name("check-production-panic-exemptions.py")
spec = importlib.util.spec_from_file_location("production_panic_exemptions", SCRIPT)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def main() -> int:
    assert module.violations(
        {"crates/example/src/lib.rs": "#[allow(clippy::expect_used)]\npub mod risky;\n"}
    )
    assert module.violations(
        {
            "apps/example/src-tauri/src/main.rs": (
                "#[allow(\n    clippy::unwrap_used,\n    clippy::panic\n)]\nfn main() {}\n"
            )
        }
    )
    assert module.violations(
        {"src/main.rs": "#![allow(clippy::expect_used)]\nfn main() {}\n"}
    )

    # Test-only expect use is permitted; the policy targets exemptions, not the call itself.
    assert module.violations(
        {
            "crates/example/src/lib.rs": (
                "#[cfg(test)]\nmod tests {\n"
                "    #[test]\n"
                "    fn fixture() { let _ = Some(1).expect(\"fixture\"); }\n"
                "}\n"
            )
        }
    ) == []
    assert module.violations(
        {"crates/example/tests/integration.rs": "#[allow(clippy::expect_used)]\nfn fixture() {}\n"}
    ) == []
    assert module.violations(
        {"crates/example/src/lib.rs": "#[allow(clippy::too_many_arguments)]\nfn ok() {}\n"}
    ) == []

    print("production panic exemption fixtures passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
