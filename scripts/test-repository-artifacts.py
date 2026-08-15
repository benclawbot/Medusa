#!/usr/bin/env python3
"""Fixture tests for repository artifact hygiene policy."""

from __future__ import annotations

import importlib.util
from pathlib import Path


SCRIPT = Path(__file__).with_name("check-repository-artifacts.py")
spec = importlib.util.spec_from_file_location("repository_artifacts", SCRIPT)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def assert_rejected(path: str, expected: str) -> None:
    found = module.violations([path])
    assert len(found) == 1, (path, found)
    assert expected in found[0], found[0]


def assert_admitted(path: str) -> None:
    found = module.violations([path])
    assert found == [], (path, found)


def main() -> int:
    assert_rejected("cargo-test.log", "root log")
    assert_rejected("dependency-test.log", "root log")
    assert_rejected(".github/issue-447-trigger.txt", "trigger marker")
    assert_rejected(".github/release-trigger.md", "trigger marker")

    # Intentional fixtures and durable machine-readable evidence remain valid.
    assert_admitted("tests/fixtures/provider-output.log")
    assert_admitted("crates/medusa-agent/tests/fixtures/trace.log")
    assert_admitted("benchmarks/results/baseline.json")
    assert_admitted("benchmark-evidence.json")
    assert_admitted("docs/trigger-semantics.md")
    assert_admitted(".github/workflows/trigger-release.yml")

    print("repository artifact hygiene fixtures passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
