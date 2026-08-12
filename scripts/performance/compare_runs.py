#!/usr/bin/env python3
"""Compare two Medusa benchmark result files.

The comparator intentionally uses only the Python standard library so it can run
in normal CI without additional dependencies.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

REQUIRED = {
    "schema_version",
    "scenario",
    "repository_revision",
    "platform",
    "verified_success",
    "verification_coverage",
    "end_to_end_ms",
    "runtime_overhead_ms",
    "model_requests",
    "redundant_tool_call_ratio",
}


def load(path: str) -> dict[str, Any]:
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    missing = sorted(REQUIRED - data.keys())
    if missing:
        raise ValueError(f"{path}: missing required fields: {', '.join(missing)}")
    if data["schema_version"] != 1:
        raise ValueError(f"{path}: unsupported schema_version {data['schema_version']}")
    return data


def compare(base: dict[str, Any], candidate: dict[str, Any]) -> list[str]:
    failures: list[str] = []
    for key in ("scenario", "platform"):
        if base[key] != candidate[key]:
            failures.append(f"incomparable {key}: {base[key]!r} != {candidate[key]!r}")

    if base["verified_success"] and not candidate["verified_success"]:
        failures.append("verified success regressed")
    if candidate["verification_coverage"] < base["verification_coverage"]:
        failures.append("verification coverage regressed")
    if candidate["end_to_end_ms"] > base["end_to_end_ms"] * 1.10:
        failures.append("end-to-end latency regressed by more than 10%")
    if candidate["runtime_overhead_ms"] > max(50, base["runtime_overhead_ms"] * 1.10):
        failures.append("runtime overhead exceeds the 50 ms budget or regressed by more than 10%")
    if candidate["model_requests"] > base["model_requests"]:
        failures.append("model request count regressed")
    if candidate["redundant_tool_call_ratio"] > 0.03:
        failures.append("redundant tool-call ratio exceeds 3%")
    return failures


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print("usage: compare_runs.py BASELINE.json CANDIDATE.json", file=sys.stderr)
        return 2
    try:
        failures = compare(load(argv[1]), load(argv[2]))
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(error, file=sys.stderr)
        return 2
    if failures:
        for failure in failures:
            print(f"REGRESSION: {failure}")
        return 1
    print("PASS: candidate preserves verified correctness and performance budgets")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
