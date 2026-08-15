#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path

SCRIPT = Path(__file__).with_name("coding-harness-benchmark.py")
SPEC = importlib.util.spec_from_file_location("coding_harness_benchmark", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def receipt(identifier: str) -> dict:
    return {
        "id": identifier,
        "status": "passed",
        "duration_ms": 10,
        "verification_status": "satisfied",
        "metrics": {
            "final_diff": "fixture-diff",
            "context_retained_bytes": 100,
            "context_reread_bytes": 10,
            "input_tokens": 20,
            "output_tokens": 10,
        },
    }


def summary() -> dict:
    identifiers = (
        "production-orchestration", "verification-rollback", "headless-entrypoint",
        "architecture-policy", "escalation", "upgrade-rollback-evidence",
        "checkpoint-restore", "interruption-resume", "interruption-replay",
    )
    return {"schema_version": 1, "scenarios": [receipt(item) for item in identifiers]}


def make(suite: dict, source: dict, variant: str = "current-production") -> dict:
    return MODULE.make_report(
        suite, variant, [source, source],
        {"provider": "fixture", "model": "same-model", "configuration": {"temperature": 0}},
    )


def main() -> int:
    suite = json.loads(Path("benchmarks/coding-harness-suite-v1.json").read_text(encoding="utf-8"))
    MODULE.validate_suite(suite)
    assert any(item.get("forced_compaction") for item in suite["scenarios"])
    assert any(item.get("forced_roadblock") for item in suite["scenarios"])
    assert any(item.get("forced_long_horizon") for item in suite["scenarios"])

    os.environ["MEDUSA_BENCHMARK_COMMIT"] = "fixture-commit"
    first = make(suite, summary())
    second = make(suite, summary())
    assert first["passed"]
    assert first["identity_sha256"] == second["identity_sha256"]
    assert first["trials"][0]["summary_sha256"] == second["trials"][0]["summary_sha256"]
    assert first["identity"]["repository_revision"] != "unknown"
    assert first["identity"]["suite_sha256"] == MODULE.sha256(suite)
    assert first["metrics"]["verification_coverage"] == 1.0

    broken = summary()
    broken["scenarios"][0]["verification_status"] = "missing"
    broken_report = make(suite, broken)
    assert not broken_report["passed"]
    assert "verification_coverage" in broken_report["failures"]

    false_complete = summary()
    false_complete["scenarios"][0]["metrics"]["false_completes"] = 1
    false_report = make(suite, false_complete)
    assert not false_report["passed"]
    assert "false_complete_rate" in false_report["failures"]

    baseline_source = summary()
    for item in baseline_source["scenarios"]:
        item["metrics"]["irrelevant_context_bytes"] = 20
    baseline = make(suite, baseline_source, "baseline")
    candidate_source = summary()
    for item in candidate_source["scenarios"]:
        item["metrics"]["irrelevant_context_bytes"] = 1
    candidate = make(suite, candidate_source, "evidence-ranked-context")
    assert MODULE.compare_feature(suite, baseline, candidate, "875") == []

    regression_source = summary()
    regression_source["scenarios"][0]["status"] = "failed"
    regression = make(suite, regression_source, "evidence-ranked-context")
    assert MODULE.compare_feature(suite, baseline, regression, "875")

    print("coding-harness-benchmark-fixtures-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
