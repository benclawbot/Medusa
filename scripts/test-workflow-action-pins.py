#!/usr/bin/env python3
"""Regression tests for immutable GitHub Actions references."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
from pathlib import Path

SCRIPT = Path(__file__).with_name("check-workflow-action-pins.py")
SPEC = importlib.util.spec_from_file_location("workflow_action_pins", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
POLICY = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = POLICY
SPEC.loader.exec_module(POLICY)

PIN = "11d5960a326750d5838078e36cf38b85af677262"


def write_workflow(root: Path, text: str) -> None:
    directory = root / ".github" / "workflows"
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "test.yml").write_text(text, encoding="utf-8")


def test_pinned_and_local_references_pass() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        write_workflow(
            root,
            f"steps:\n  - uses: actions/checkout@{PIN}\n  - uses: ./local-action\n  - uses: docker://alpine:3.20\n",
        )
        assert POLICY.find_violations(root) == []


def test_mutable_action_tag_fails() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        write_workflow(root, "steps:\n  - uses: actions/checkout@v4\n")
        violations = POLICY.find_violations(root)
        assert len(violations) == 1
        assert violations[0].reference == "actions/checkout@v4"
        assert violations[0].line == 2


def test_mutable_reusable_workflow_ref_fails() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        write_workflow(
            root,
            "jobs:\n  reused:\n    uses: example/project/.github/workflows/build.yml@main\n",
        )
        violations = POLICY.find_violations(root)
        assert len(violations) == 1
        assert violations[0].reference.endswith("@main")


def test_exact_allowlist_entry_passes() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        reference = "example/action@v1"
        write_workflow(root, f"steps:\n  - uses: {reference}\n")
        allowlist = root / ".github" / "workflow-action-pin-allowlist.txt"
        allowlist.write_text(f"# reviewed exception\n{reference}\n", encoding="utf-8")
        assert POLICY.find_violations(root) == []


def test_repository_workflows_are_pinned() -> None:
    root = Path(__file__).resolve().parents[1]
    violations = POLICY.find_violations(root)
    assert not violations, "\n".join(violation.render() for violation in violations)


def main() -> int:
    tests = [
        test_pinned_and_local_references_pass,
        test_mutable_action_tag_fails,
        test_mutable_reusable_workflow_ref_fails,
        test_exact_allowlist_entry_passes,
        test_repository_workflows_are_pinned,
    ]
    for test in tests:
        test()
    print(f"workflow action pin policy tests passed: {len(tests)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
