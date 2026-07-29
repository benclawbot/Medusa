#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import statistics
import subprocess
import time
from pathlib import Path
from typing import Any


def load(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def run_acceptance(output: Path) -> dict[str, Any]:
    subprocess.run(["cargo", "product-acceptance", "--output", str(output)], check=False)
    summary = output / "summary.json"
    if not summary.exists():
        raise RuntimeError(f"missing product acceptance summary: {summary}")
    return load(summary)


def percentile(values: list[int], percentile_value: float) -> int:
    if not values:
        return 0
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, int(round((len(ordered) - 1) * percentile_value))))
    return ordered[index]


def numeric(scenario: dict[str, Any], name: str) -> int:
    metrics = scenario.get("metrics", {})
    value = metrics.get(name, 0)
    return int(value) if isinstance(value, (int, float)) else 0


def score(suite: dict[str, Any], runs: list[dict[str, Any]]) -> dict[str, Any]:
    indexed = [{item["id"]: item for item in run.get("scenarios", [])} for run in runs]
    trajectories: list[dict[str, Any]] = []
    durations: list[int] = []
    first_passes = 0
    successful = 0
    verification_satisfied = 0
    safety_regressions = 0
    derived_metrics = {"first_pass_success_rate", "median_duration_ms", "p95_duration_ms"}
    totals = {
        name: 0 for name in suite["reported_tradeoffs"] if name not in derived_metrics
    }

    for definition in suite["scenarios"]:
        per_run = []
        for run in indexed:
            selected = [run[item] for item in definition["acceptance_ids"] if item in run]
            passed = bool(selected) and all(item.get("status") == "passed" for item in selected)
            duration_ms = sum(int(item.get("duration_ms", 0)) for item in selected)
            first_pass = passed and all(numeric(item, "attempts") <= 1 for item in selected)
            verified = passed and all(
                item.get("verification_status", "satisfied") == "satisfied" for item in selected
            )
            safety = sum(numeric(item, "safety_regressions") for item in selected)
            durations.append(duration_ms)
            successful += int(passed)
            first_passes += int(first_pass)
            verification_satisfied += int(verified)
            safety_regressions += safety
            for name in totals:
                if name == "critical_path_latency_ms":
                    totals[name] += max([numeric(item, name) for item in selected] or [duration_ms])
                else:
                    totals[name] += sum(numeric(item, name) for item in selected)
            per_run.append({
                "passed": passed,
                "first_pass": first_pass,
                "verified": verified,
                "duration_ms": duration_ms,
                "safety_regressions": safety,
            })
        trajectories.append({"id": definition["id"], "runs": per_run})

    attempts = len(suite["scenarios"]) * len(runs)
    metrics: dict[str, Any] = {
        "task_success_rate": successful / attempts if attempts else 1.0,
        "first_pass_success_rate": first_passes / attempts if attempts else 1.0,
        "verification_coverage": verification_satisfied / attempts if attempts else 1.0,
        "safety_regressions": safety_regressions,
        "median_duration_ms": int(statistics.median(durations)) if durations else 0,
        "p95_duration_ms": percentile(durations, 0.95),
        **totals,
    }
    invariants = suite["release_invariants"]
    failures = []
    for name, expected in invariants.items():
        actual = metrics[name]
        ok = actual <= expected if name == "safety_regressions" else actual >= expected
        if not ok:
            failures.append({"metric": name, "actual": actual, "required": expected})

    return {
        "schema_version": 1,
        "suite_id": suite["suite_id"],
        "commit": os.environ.get("GITHUB_SHA") or os.environ.get("MEDUSA_BENCHMARK_COMMIT") or "unknown",
        "generated_unix_seconds": int(time.time()),
        "run_count": len(runs),
        "metrics": metrics,
        "release_invariants": invariants,
        "passed": not failures,
        "failures": failures,
        "trajectories": trajectories,
    }


def markdown(report: dict[str, Any]) -> str:
    lines = [
        "# End-to-end orchestration benchmark results",
        "",
        f"- Commit: `{report['commit']}`",
        f"- Runs: {report['run_count']}",
        f"- Release invariants: {'PASS' if report['passed'] else 'FAIL'}",
        "",
        "No arbitrary improvement percentage is enforced. Non-safety tradeoffs are reported for comparison against retained baselines.",
        "",
        "| Metric | Result |",
        "|---|---:|",
    ]
    lines.extend(f"| `{name}` | {value} |" for name, value in report["metrics"].items())
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--suite", type=Path, default=Path("benchmarks/orchestration-suite.json"))
    parser.add_argument("--output", type=Path, default=Path("target/orchestration-benchmark"))
    parser.add_argument("--summary", action="append", type=Path)
    args = parser.parse_args()
    suite = load(args.suite)
    args.output.mkdir(parents=True, exist_ok=True)
    runs = [load(path) for path in args.summary] if args.summary else []
    if not runs:
        for index in range(int(suite["runs"])):
            runs.append(run_acceptance(args.output / f"run-{index + 1}"))
    report = score(suite, runs)
    (args.output / "orchestration-benchmark.json").write_text(
        json.dumps(report, indent=2) + "\n", encoding="utf-8"
    )
    (args.output / "orchestration-benchmark.md").write_text(
        markdown(report), encoding="utf-8"
    )
    print(json.dumps(report["metrics"], sort_keys=True))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
