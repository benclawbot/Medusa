#!/usr/bin/env python3
"""Validate and summarize Medusa performance run JSON files using stdlib only."""
from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

REQUIRED = {
    "schema_version",
    "run_id",
    "scenario",
    "mode",
    "repository_revision",
    "platform",
    "machine",
    "provider",
    "outcome",
    "phases",
    "model",
    "tools",
    "verification",
    "artifacts",
}


def percentile(values: list[float], q: float) -> float:
    if not values:
        raise ValueError("no samples")
    ordered = sorted(values)
    rank = max(0, math.ceil(q * len(ordered)) - 1)
    return ordered[rank]


def load(path: str) -> dict:
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    missing = sorted(REQUIRED - data.keys())
    if missing:
        raise ValueError(f"{path}: missing telemetry: {', '.join(missing)}")
    if data["schema_version"] != 1:
        raise ValueError(f"{path}: unsupported schema_version")
    phases = data["phases"]
    start = phases.get("objective_accepted_ns")
    end = phases.get("verified_completion_ns")
    if not isinstance(start, int) or not isinstance(end, int) or end < start:
        raise ValueError(f"{path}: invalid verified-completion timestamps")
    if not data["artifacts"]:
        raise ValueError(f"{path}: missing artifacts")
    return data


def build_report(runs: list[dict]) -> dict:
    grouped: dict[tuple[str, str, str], list[float]] = {}
    for run in runs:
        key = (run["scenario"], run["mode"], run["platform"])
        elapsed_ms = (
            run["phases"]["verified_completion_ns"]
            - run["phases"]["objective_accepted_ns"]
        ) / 1_000_000
        grouped.setdefault(key, []).append(elapsed_ms)

    report = {"schema_version": 1, "groups": []}
    for key, values in sorted(grouped.items()):
        report["groups"].append(
            {
                "scenario": key[0],
                "mode": key[1],
                "platform": key[2],
                "samples": len(values),
                "p50_ms": percentile(values, 0.50),
                "p95_ms": percentile(values, 0.95),
                "p99_ms": percentile(values, 0.99),
            }
        )
    return report


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Validate and summarize Medusa verified-completion performance runs."
    )
    parser.add_argument("runs", nargs="+", metavar="RUN.json")
    parser.add_argument(
        "--output",
        type=Path,
        help="write the JSON report to this path instead of stdout",
    )
    return parser.parse_args(argv[1:])


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        runs = [load(path) for path in args.runs]
        report = build_report(runs)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(error, file=sys.stderr)
        return 2

    rendered = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.output is None:
        sys.stdout.write(rendered)
    else:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
