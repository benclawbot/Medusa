#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

DEFAULT_ACCEPTANCE_TIMEOUT_SECONDS = 450


def load(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def acceptance_command(output: Path) -> list[str]:
    configured = os.environ.get("MEDUSA_PRODUCT_ACCEPTANCE_BIN")
    if configured:
        return [configured, "--output", str(output)]
    candidate = Path("target/debug/medusa-product-acceptance")
    if candidate.exists():
        return [str(candidate), "--output", str(output)]
    return ["cargo", "product-acceptance", "--output", str(output)]


def run_acceptance(output: Path, run_number: int, total_runs: int) -> dict[str, Any]:
    timeout_seconds = int(
        os.environ.get(
            "MEDUSA_ACCEPTANCE_TIMEOUT_SECONDS", str(DEFAULT_ACCEPTANCE_TIMEOUT_SECONDS)
        )
    )
    command = acceptance_command(output)
    print(
        f"[reliability] acceptance run {run_number}/{total_runs} "
        f"(timeout={timeout_seconds}s): {' '.join(command)}",
        flush=True,
    )
    started = time.monotonic()
    try:
        completed = subprocess.run(command, check=False, timeout=timeout_seconds)
    except subprocess.TimeoutExpired as error:
        raise RuntimeError(
            f"product acceptance run {run_number}/{total_runs} timed out after "
            f"{timeout_seconds}s"
        ) from error
    elapsed = time.monotonic() - started
    print(
        f"[reliability] acceptance run {run_number}/{total_runs} "
        f"finished with exit={completed.returncode} in {elapsed:.1f}s",
        flush=True,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"product acceptance run {run_number}/{total_runs} failed with exit status "
            f"{completed.returncode}"
        )
    summary = output / "summary.json"
    if not summary.exists():
        raise RuntimeError(f"missing product acceptance summary: {summary}")
    return load(summary)


def score(suite: dict[str, Any], runs: list[dict[str, Any]]) -> dict[str, Any]:
    indexed = [
        {scenario["id"]: scenario for scenario in run.get("scenarios", [])}
        for run in runs
    ]
    results = []
    for case in suite["scenarios"]:
        per_run = []
        for run in indexed:
            candidates = [run[item] for item in case["acceptance_ids"] if item in run]
            passed = bool(candidates) and all(item["status"] == "passed" for item in candidates)
            duration_values = [item.get("duration_ms") for item in candidates]
            duration_ms = (
                sum(int(value) for value in duration_values)
                if duration_values and all(isinstance(value, (int, float)) for value in duration_values)
                else None
            )
            per_run.append({"passed": passed, "duration_ms": duration_ms})
        results.append({"id": case["id"], "metric": case["metric"], "runs": per_run})

    verified = [item for item in results if item["metric"] == "verified_completion"]
    resumes = [item for item in results if item["metric"] == "successful_resume"]
    rollbacks = [item for item in results if item["metric"] == "successful_rollback"]
    containment = [item for item in results if item["metric"] == "containment_enforcement"]

    def rate(items: list[dict[str, Any]]) -> float | None:
        attempts = sum(len(item["runs"]) for item in items)
        passes = sum(sum(1 for run in item["runs"] if run["passed"]) for item in items)
        return passes / attempts if attempts else None

    fingerprints = []
    for run in runs:
        stable = [(s["id"], s["status"]) for s in run.get("scenarios", [])]
        fingerprints.append(hashlib.sha256(json.dumps(stable, sort_keys=True).encode()).hexdigest())

    measured_false_completion = [
        run.get("metrics", {}).get("false_completion_rate") for run in runs
    ]
    measured_manual_interventions = [
        run.get("metrics", {}).get("manual_interventions") for run in runs
    ]
    false_completion_rate = (
        sum(value for value in measured_false_completion) / len(measured_false_completion)
        if measured_false_completion
        and all(isinstance(value, (int, float)) for value in measured_false_completion)
        else None
    )
    manual_interventions = (
        sum(measured_manual_interventions)
        if measured_manual_interventions
        and all(isinstance(value, (int, float)) for value in measured_manual_interventions)
        else None
    )
    containment_violations = (
        sum(1 for item in containment for run in item["runs"] if not run["passed"])
        if containment and runs
        else None
    )
    verified_duration = [run["duration_ms"] for item in verified for run in item["runs"]]
    metrics = {
        "verified_completion_rate": rate(verified),
        "false_completion_rate": false_completion_rate,
        "successful_resume_rate": rate(resumes),
        "successful_rollback_rate": rate(rollbacks),
        "containment_violations": containment_violations,
        "manual_interventions": manual_interventions,
        "time_to_verified_completion_ms": (
            sum(verified_duration)
            if verified_duration and all(value is not None for value in verified_duration)
            else None
        ),
        "repeated_run_determinism": (
            1.0 if len(set(fingerprints)) <= 1 else 0.0
        ) if fingerprints else None,
    }
    thresholds = suite["thresholds"]
    failures = []
    for name, expected in thresholds.items():
        actual = metrics[name]
        if actual is None:
            failures.append({"metric": name, "actual": None, "threshold": expected, "reason": "missing evidence"})
            continue
        if name in {"false_completion_rate", "containment_violations", "manual_interventions"}:
            ok = actual <= expected
        else:
            ok = actual >= expected
        if not ok:
            failures.append({"metric": name, "actual": actual, "threshold": expected})

    return {
        "schema_version": 1,
        "suite_id": suite["suite_id"],
        "mode": suite["mode"],
        "commit": os.environ.get("GITHUB_SHA") or os.environ.get("MEDUSA_BENCHMARK_COMMIT") or "unknown",
        "generated_unix_seconds": int(time.time()),
        "run_count": len(runs),
        "metrics": metrics,
        "thresholds": thresholds,
        "passed": not failures,
        "failures": failures,
        "scenarios": results,
    }


def markdown(report: dict[str, Any]) -> str:
    lines = [
        "# Reliability and recovery benchmark results",
        "",
        f"- Commit: `{report['commit']}`",
        f"- Runs: {report['run_count']}",
        f"- Result: {'PASS' if report['passed'] else 'FAIL'}",
        "",
        "| Metric | Result | Threshold |",
        "|---|---:|---:|",
    ]
    for name, value in report["metrics"].items():
        threshold = report["thresholds"].get(name, "reported")
        lines.append(f"| `{name}` | {value} | {threshold} |")
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--suite", type=Path, default=Path("benchmarks/reliability-suite.json"))
    parser.add_argument("--output", type=Path, default=Path("target/reliability-benchmark"))
    parser.add_argument("--summary", action="append", type=Path)
    args = parser.parse_args()
    suite = load(args.suite)
    args.output.mkdir(parents=True, exist_ok=True)
    runs = [load(path) for path in args.summary] if args.summary else []
    if not runs:
        total_runs = int(suite["runs"])
        for index in range(total_runs):
            runs.append(run_acceptance(args.output / f"run-{index + 1}", index + 1, total_runs))
    report = score(suite, runs)
    (args.output / "reliability-benchmark.json").write_text(
        json.dumps(report, indent=2) + "\n", encoding="utf-8"
    )
    (args.output / "reliability-benchmark.md").write_text(markdown(report), encoding="utf-8")
    print(json.dumps(report["metrics"], sort_keys=True))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
