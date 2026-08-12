#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import json
from pathlib import Path

SCRIPT = Path(__file__).with_name("orchestration-benchmark.py")
SPEC = importlib.util.spec_from_file_location("orchestration_benchmark", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def summary(status: str = "passed") -> dict:
    ids = {
        "production-orchestration", "headless-entrypoint", "checkpoint-restore",
        "verification-rollback", "architecture-policy", "upgrade-rollback-evidence",
        "interruption-replay", "interruption-resume",
    }
    return {
        "schema_version": 1,
        "scenarios": [{
            "id": item,
            "status": status,
            "duration_ms": 10,
            "verification_status": "satisfied",
            "metrics": {
                "attempts": 1,
                "total_tool_calls": 2,
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_hits": 1,
            },
        } for item in sorted(ids)],
    }


def main() -> int:
    suite = json.loads(Path("benchmarks/orchestration-suite.json").read_text(encoding="utf-8"))
    report = MODULE.score(suite, [summary(), summary()])
    assert report["passed"]
    assert report["metrics"]["task_success_rate"] == 1.0
    assert report["metrics"]["verification_coverage"] == 1.0
    assert report["metrics"]["total_tool_calls"] > 0

    failed = summary()
    failed["scenarios"][0]["status"] = "failed"
    report = MODULE.score(suite, [summary(), failed])
    assert not report["passed"]
    assert any(item["metric"] == "task_success_rate" for item in report["failures"])

    unsafe = summary()
    unsafe["scenarios"][0]["metrics"]["safety_regressions"] = 1
    report = MODULE.score(suite, [summary(), unsafe])
    assert not report["passed"]
    assert report["metrics"]["safety_regressions"] == 1

    assert "No arbitrary improvement percentage" in MODULE.markdown(report)
    print("orchestration-benchmark-fixtures-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
