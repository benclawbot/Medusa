#!/usr/bin/env python3
"""Certify the #689 continuous-verification performance slice from real Rust execution."""

from __future__ import annotations

import argparse
import json
import math
import statistics
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BASELINE = ROOT / "benchmarks/performance/baselines/continuous-verification-v1.json"
MARKER = "MEDUSA_CONTINUOUS_VERIFICATION_PERF="


def percentile_95(values: list[int]) -> float:
    ordered = sorted(values)
    if not ordered:
        raise RuntimeError("performance probe emitted no samples")
    index = max(0, min(len(ordered) - 1, math.ceil(len(ordered) * 0.95) - 1))
    return float(ordered[index])


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--platform", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()

    baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
    if baseline.get("schema_version") != 1:
        raise RuntimeError("unsupported continuous verification baseline schema")
    if args.platform not in baseline["required_platforms"]:
        raise RuntimeError(f"unexpected certification platform: {args.platform}")

    command = [
        "cargo",
        "test",
        "-p",
        "medusa-agent",
        "--test",
        "verification_pipeline_performance",
        "--",
        "--nocapture",
    ]
    completed = subprocess.run(
        command,
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=900,
        check=False,
    )
    if completed.returncode != 0:
        sys.stderr.write(completed.stdout)
        return completed.returncode

    payload = None
    for line in completed.stdout.splitlines():
        if MARKER in line:
            payload = json.loads(line.split(MARKER, 1)[1])
    if payload is None:
        sys.stderr.write(completed.stdout)
        raise RuntimeError("continuous verification performance marker was not emitted")

    fixture_count = int(payload["fixture_count"])
    values_per_fixture = int(payload["values_per_fixture"])
    total_values = fixture_count * values_per_fixture
    cold = [int(value) for value in payload["cold_ns"]]
    warm = [int(value) for value in payload["warm_ns"]]

    if fixture_count < int(baseline["minimum_fixture_count"]):
        raise RuntimeError("performance fixture count is below acceptance threshold")
    if values_per_fixture < int(baseline["minimum_values_per_fixture"]):
        raise RuntimeError("semantic payload per fixture is below acceptance threshold")
    if total_values < int(baseline["minimum_total_values"]):
        raise RuntimeError("total semantic workload is below acceptance threshold")
    if len(cold) < int(baseline["minimum_samples"]) or len(warm) < int(
        baseline["minimum_samples"]
    ):
        raise RuntimeError("performance sample count is below acceptance threshold")

    cold_median = float(statistics.median(cold))
    warm_median = float(statistics.median(warm))
    cold_p95 = percentile_95(cold)
    warm_p95 = percentile_95(warm)
    median_speedup = 1.0 - (warm_median / cold_median)
    p95_speedup = 1.0 - (warm_p95 / cold_p95)
    exact_rerun_ratio = 0.0 if payload["exact_reuse_verified"] else 1.0

    minimum_speedup = float(baseline["minimum_warm_speedup_ratio"])
    if median_speedup < minimum_speedup:
        raise RuntimeError(
            f"warm median speedup {median_speedup:.6f} is below {minimum_speedup:.6f}; "
            f"cold_median_ns={int(cold_median)} warm_median_ns={int(warm_median)}"
        )
    if p95_speedup < minimum_speedup:
        raise RuntimeError(
            f"warm p95 speedup {p95_speedup:.6f} is below {minimum_speedup:.6f}; "
            f"cold_p95_ns={int(cold_p95)} warm_p95_ns={int(warm_p95)}"
        )
    if exact_rerun_ratio > float(baseline["maximum_exact_rerun_ratio"]):
        raise RuntimeError("exact-check rerun ratio exceeds acceptance threshold")
    if float(payload["verification_coverage"]) < float(
        baseline["required_verification_coverage"]
    ):
        raise RuntimeError("verification coverage regressed")
    if baseline["require_stale_input_rerun"] and not payload["stale_input_rerun_verified"]:
        raise RuntimeError("stale input did not force authoritative rerun")

    report = {
        "schema_version": 1,
        "program_issue": 689,
        "scenario": baseline["scenario"],
        "platform": args.platform,
        "repository_revision": subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
        ).strip(),
        "verified_success": True,
        "verification_coverage": float(payload["verification_coverage"]),
        "fixture_count": fixture_count,
        "values_per_fixture": values_per_fixture,
        "total_values": total_values,
        "sample_count": len(cold),
        "cold_median_ns": int(cold_median),
        "warm_median_ns": int(warm_median),
        "cold_p95_ns": int(cold_p95),
        "warm_p95_ns": int(warm_p95),
        "warm_median_speedup_ratio": median_speedup,
        "warm_p95_speedup_ratio": p95_speedup,
        "exact_check_rerun_ratio": exact_rerun_ratio,
        "exact_reuse_verified": bool(payload["exact_reuse_verified"]),
        "stale_input_rerun_verified": bool(payload["stale_input_rerun_verified"]),
        "command": command,
    }
    Path(args.output).write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
