#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any


def load(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def config_hash(model: dict[str, Any]) -> str:
    raw = json.dumps(model.get("configuration", {}), sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()


def model_key(report: dict[str, Any]) -> tuple[str, str, str]:
    model = report["identity"]["model"]
    return model["provider"], model["model"], config_hash(model)


def validate_comparison(baseline: dict[str, Any], candidate: dict[str, Any]) -> None:
    if model_key(baseline) != model_key(candidate):
        raise ValueError("comparison rejected: provider/model/configuration are not identical")
    for field in ("suite_id", "suite_version", "task_revision", "suite_sha256"):
        if baseline["identity"][field] != candidate["identity"][field]:
            raise ValueError(f"comparison rejected: {field} differs")


def metric(report: dict[str, Any], name: str) -> int | float:
    value = report.get("metrics", {}).get(name)
    if not isinstance(value, (int, float)):
        raise ValueError(f"comparison rejected: missing evidence for metric {name}")
    return value


def compare(suite: dict[str, Any], baseline: dict[str, Any], candidate: dict[str, Any]) -> dict[str, Any]:
    validate_comparison(baseline, candidate)
    features = set(candidate["identity"].get("harness_features", [])) - set(
        baseline["identity"].get("harness_features", [])
    )
    failures: list[str] = []
    for assertion in suite["feature_assertions"]:
        if assertion["feature"] not in features:
            continue
        name = assertion["metric"]
        left = metric(baseline, name)
        right = metric(candidate, name)
        ok = right <= left if assertion["direction"] == "lower_or_equal" else right >= left
        if not ok:
            failures.append(f"{assertion['feature']} regressed {name}: {right} vs {left}")
        guard = assertion.get("guard_metric")
        if guard:
            before = metric(baseline, guard)
            after = metric(candidate, guard)
            guard_ok = after <= before if guard == "false_complete_rate" else after >= before
            if not guard_ok:
                failures.append(f"{assertion['feature']} regressed guard {guard}: {after} vs {before}")
    for name, direction in (
        ("task_success_rate", "higher"),
        ("verification_coverage", "higher"),
        ("false_complete_rate", "lower"),
        ("safety_regressions", "lower"),
    ):
        before = metric(baseline, name)
        after = metric(candidate, name)
        ok = after >= before if direction == "higher" else after <= before
        if not ok:
            failures.append(f"promotion guard regressed {name}: {after} vs {before}")
    return {
        "schema_version": 1,
        "baseline_identity": baseline["identity_sha256"],
        "candidate_identity": candidate["identity_sha256"],
        "features_added": sorted(features),
        "passed": not failures,
        "failures": failures,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("--suite", type=Path, default=Path("benchmarks/coding-harness-suite-v1.json"))
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    result = compare(load(args.suite), load(args.baseline), load(args.candidate))
    payload = json.dumps(result, indent=2) + "\n"
    if args.output:
        args.output.write_text(payload, encoding="utf-8")
    print(payload, end="")
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
