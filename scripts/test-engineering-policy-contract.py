#!/usr/bin/env python3
"""Regression fixtures for repository-specific engineering-policy obligations."""
from __future__ import annotations

import importlib.util
import io
import json
import pathlib
import sys
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts/engineering-policy.py"
SPEC = importlib.util.spec_from_file_location("engineering_policy", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {MODULE_PATH}")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def resolved(path: str) -> dict[str, object]:
    policy = MODULE.load_policy(ROOT / ".github/engineering-policy.json", ROOT)
    return MODULE.resolve(policy, [path])


def rule_ids(report: dict[str, object]) -> set[str]:
    return {rule["id"] for rule in report["triggered_rules"]}


def checks(report: dict[str, object]) -> set[str]:
    return set(report["required_checks"])


def main() -> int:
    active_run = [
        {"name": "Workspace quality", "status": "completed", "conclusion": "success"},
        {"name": "Repository policy and evidence", "status": "in_progress", "conclusion": None},
    ]
    assert MODULE.current_run_jobs_succeeded(active_run, "Repository policy and evidence") is True
    assert MODULE.current_run_jobs_succeeded(
        [{"name": "Workspace quality", "status": "in_progress"}, *active_run[1:]],
        "Repository policy and evidence",
    ) is None
    try:
        MODULE.current_run_jobs_succeeded(active_run[:1], "Repository policy and evidence")
    except ValueError as exc:
        assert "exactly one" in str(exc)
    else:
        raise AssertionError("current workflow jobs must include the policy job itself")
    try:
        MODULE.current_run_jobs_succeeded([], "Repository policy and evidence")
    except ValueError as exc:
        assert "exactly one" in str(exc)
    else:
        raise AssertionError("an empty current workflow run must fail closed")
    try:
        MODULE.current_run_jobs_succeeded(
            [{"name": "Workspace quality", "status": "completed", "conclusion": "failure"}, *active_run[1:]],
            "Repository policy and evidence",
        )
    except ValueError as exc:
        assert "Workspace quality=failure" in str(exc)
    else:
        raise AssertionError("failed job in the current workflow run must fail policy enforcement")

    pages = [
        {"total_count": 101, "jobs": [{"name": f"job-{index}"} for index in range(100)]},
        {"total_count": 101, "jobs": [{"name": "last-job"}]},
    ]
    requested_urls: list[str] = []

    def fake_urlopen(request: object, timeout: int) -> io.BytesIO:
        requested_urls.append(request.full_url)  # type: ignore[attr-defined]
        return io.BytesIO(json.dumps(pages.pop(0)).encode())

    with mock.patch.object(MODULE.urllib.request, "urlopen", side_effect=fake_urlopen):
        paged_jobs = MODULE.github_workflow_jobs("owner/repo", "123", "token")
    assert len(paged_jobs) == 101
    assert "page=1" in requested_urls[0] and "page=2" in requested_urls[1]

    docs = resolved("docs/guide.md")
    assert "generated-documentation-inventory" in rule_ids(docs)
    assert "documentation-inventory" in checks(docs)
    assert not docs["protected_change"]

    provider = resolved("docs/provider-support.json")
    assert "provider-support-source-of-truth" in rule_ids(provider)
    assert "provider-support-sync" in checks(provider)

    claims = resolved("docs/CAPABILITY-CLAIMS.json")
    assert "capability-claim-synchronization" in rule_ids(claims)
    assert "capability-claims-sync" in checks(claims)

    authority = resolved("docs/architecture/baseline.json")
    assert "canonical-truth-authorities" in rule_ids(authority)
    assert {"canonical-truth-authority", "evidence-authority", "architecture-policy"} <= checks(authority)
    assert authority["protected_change"]

    runtime = resolved("crates/medusa-runtime/src/lib.rs")
    assert "canonical-truth-authorities" in rule_ids(runtime)
    assert "capability-claim-synchronization" in rule_ids(runtime)
    assert runtime["protected_change"]

    first = resolved("docs/provider-support.json")
    second = resolved("docs/provider-support.json")
    assert first == second

    print("engineering policy contract fixtures passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
