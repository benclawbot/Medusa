#!/usr/bin/env python3
"""Run the fast PR product-acceptance contract with one shared Cargo target.

The smoke contract exercises production tests directly. It intentionally does not
replace the authoritative cross-platform `cargo product-acceptance` evidence run,
which remains required on main and for manual release validation.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

SCENARIOS: tuple[dict[str, Any], ...] = (
    {
        "id": "production-orchestration",
        "guarantee": "Production orchestration passes its authoritative integration suite.",
        "args": ["test", "-p", "medusa-runtime", "--locked"],
        "marker": None,
    },
    {
        "id": "headless-entrypoint",
        "guarantee": "The shipped CLI retains the supported headless run entrypoint.",
        "args": [
            "test",
            "-p",
            "medusa-cli",
            "headless_run_remains_available",
            "--locked",
            "--",
            "--nocapture",
        ],
        "marker": "headless_run_remains_available",
    },
    {
        "id": "checkpoint-restore",
        "guarantee": "Execution checkpoints persist and restore deterministically.",
        "args": ["test", "-p", "medusa-execution-checkpoint", "--locked"],
        "marker": None,
    },
    {
        "id": "verification-rollback",
        "guarantee": "Failed or rejected integration can roll repository changes back.",
        "args": ["test", "-p", "medusa-workers", "--locked"],
        "marker": None,
    },
    {
        "id": "filesystem-network-process-boundary",
        "guarantee": "The production Linux sandbox enforces repository, network, and process boundaries.",
        "args": [
            "test",
            "-p",
            "medusa-agent",
            "linux_product_boundary_exercises_allowed_write_external_denial_and_network_denial",
            "--locked",
            "--",
            "--nocapture",
        ],
        "marker": "linux_product_boundary_exercises_allowed_write_external_denial_and_network_denial",
    },
    {
        "id": "interruption-resume",
        "guarantee": "Interrupted repository repair resumes with exact durable evidence.",
        "args": [
            "test",
            "-p",
            "medusa-agent",
            "fixture_bug_fix_survives_restart_with_exact_evidence",
            "--locked",
            "--",
            "--nocapture",
        ],
        "marker": "fixture_bug_fix_survives_restart_with_exact_evidence",
    },
)


def run(output_dir: Path) -> int:
    output_dir.mkdir(parents=True, exist_ok=True)
    target_dir = output_dir / "target"
    target_dir.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment["CARGO_TARGET_DIR"] = str(target_dir.resolve())
    environment["MEDUSA_PRODUCT_ACCEPTANCE"] = "1"

    results: list[dict[str, Any]] = []
    for scenario in SCENARIOS:
        command = ["cargo", *scenario["args"]]
        print(f"==> {scenario['id']}: {scenario['guarantee']}", flush=True)
        started = time.monotonic()
        completed = subprocess.run(
            command,
            check=False,
            capture_output=True,
            text=True,
            env=environment,
        )
        duration_ms = round((time.monotonic() - started) * 1000)
        combined = f"--- stdout ---\n{completed.stdout}\n--- stderr ---\n{completed.stderr}"
        log_path = output_dir / f"{scenario['id']}.log"
        log_path.write_text(combined, encoding="utf-8")
        marker = scenario["marker"]
        marker_present = marker is None or marker in combined
        passed = completed.returncode == 0 and marker_present
        detail = None
        if completed.returncode != 0:
            detail = f"cargo exited with status {completed.returncode}"
        elif not marker_present:
            detail = "required test marker was absent; the filter may have matched zero tests"
        print(f"    {'passed' if passed else 'failed'} ({duration_ms} ms)", flush=True)
        results.append(
            {
                "id": scenario["id"],
                "guarantee": scenario["guarantee"],
                "command": command,
                "status": "passed" if passed else "failed",
                "duration_ms": duration_ms,
                "log": str(log_path),
                "detail": detail,
            }
        )

    passed = sum(result["status"] == "passed" for result in results)
    summary = {
        "schema_version": 1,
        "mode": "pr-smoke",
        "platform": sys.platform,
        "shared_target_dir": str(target_dir),
        "passed": passed,
        "failed": len(results) - passed,
        "total": len(results),
        "scenarios": results,
    }
    summary_path = output_dir / "summary.json"
    summary_path.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(f"summary: {summary_path}")
    return 0 if summary["failed"] == 0 else 1


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=Path("product-acceptance-smoke-artifacts"))
    args = parser.parse_args()
    return run(args.output)


if __name__ == "__main__":
    raise SystemExit(main())
