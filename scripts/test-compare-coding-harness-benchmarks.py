#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
from pathlib import Path

SCRIPT = Path(__file__).with_name("compare-coding-harness-benchmarks.py")
SPEC = importlib.util.spec_from_file_location("compare_coding_harness", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def report(model: str, features: list[str], success: float = 1.0, irrelevant: int = 10) -> dict:
    return {
        "identity_sha256": f"id-{model}-{'-'.join(features)}",
        "identity": {
            "suite_id": "medusa-same-model-coding-harness",
            "suite_version": "1.0.0",
            "task_revision": "coding-harness-corpus-v1",
            "suite_sha256": "suite",
            "harness_features": features,
            "model": {"provider": "fixture", "model": model, "configuration": {"temperature": 0}},
        },
        "metrics": {
            "task_success_rate": success,
            "verification_coverage": success,
            "false_complete_rate": 0.0,
            "safety_regressions": 0,
            "irrelevant_context_bytes": irrelevant,
            "continuity_loss_incidents": 0,
            "duplicate_diagnostic_reads": 0,
            "blocked_path_completion_rate": success,
        },
    }


def main() -> int:
    suite = MODULE.load(Path("benchmarks/coding-harness-suite-v1.json"))
    baseline = report("same-model", [], irrelevant=20)
    candidate = report("same-model", ["875"], irrelevant=5)
    assert MODULE.compare(suite, baseline, candidate)["passed"]

    regressed = report("same-model", ["875"], success=0.5, irrelevant=5)
    result = MODULE.compare(suite, baseline, regressed)
    assert not result["passed"]
    assert any("task_success_rate" in item for item in result["failures"])

    mismatched = report("different-model", ["875"], irrelevant=5)
    try:
        MODULE.compare(suite, baseline, mismatched)
    except ValueError:
        pass
    else:
        raise AssertionError("different model identity was accepted")

    print("coding-harness-comparison-fixtures-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
