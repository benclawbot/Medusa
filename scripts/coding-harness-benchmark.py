#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import json
import os
import statistics
import subprocess
import time
from pathlib import Path
from typing import Any


def load(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def canonical(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")


def sha256(value: Any) -> str:
    return hashlib.sha256(canonical(value)).hexdigest()


def validate_suite(suite: dict[str, Any]) -> None:
    if suite.get("schema_version") != 1 or suite.get("suite_version") != "1.0.0":
        raise ValueError("unsupported coding harness suite version")
    categories = {item.get("category") for item in suite.get("scenarios", [])}
    required = {
        "localized_bug_fix", "repository_navigation", "cross_module_api", "regression_test",
        "multi_diagnostic_repair", "architecture_policy", "dependency_configuration",
        "long_horizon", "compaction_resume", "repository_drift", "hypothesis_recovery",
        "verification_breadth", "roadblock_recovery", "negative_control",
    }
    missing = sorted(required - categories)
    if missing:
        raise ValueError(f"missing benchmark categories: {missing}")
    for flag in ("forced_long_horizon", "forced_compaction", "forced_roadblock"):
        if not any(item.get(flag) for item in suite["scenarios"]):
            raise ValueError(f"missing required forced scenario: {flag}")
    variants = {item["id"]: tuple(item.get("features", [])) for item in suite["variants"]}
    expected = {
        "baseline": (),
        "mandatory-verification": ("873",),
        "trajectory-continuity": ("874",),
        "evidence-ranked-context": ("875",),
        "structured-repair-ledger": ("876",),
        "roadblock-recovery": ("877",),
        "current-production": ("873", "874", "875", "876", "877"),
    }
    if variants != expected:
        raise ValueError("frozen harness feature matrix changed")


def run_acceptance(output: Path) -> dict[str, Any]:
    completed = subprocess.run(
        ["cargo", "product-acceptance", "--output", str(output)], check=False
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"product acceptance failed with exit status {completed.returncode}"
        )
    summary = output / "summary.json"
    if not summary.exists():
        raise RuntimeError(f"missing authoritative product acceptance summary: {summary}")
    return load(summary)


def number(receipt: dict[str, Any], name: str) -> int | None:
    value = receipt.get("metrics", {}).get(name)
    return int(value) if isinstance(value, (int, float)) else None


def sum_metric(receipts: list[dict[str, Any]], name: str) -> int | None:
    values = [number(receipt, name) for receipt in receipts]
    if not values or any(value is None for value in values):
        return None
    return sum(value for value in values if value is not None)


def sum_case_metric(cases: list[dict[str, Any]], name: str) -> int | None:
    values = [case.get(name) for case in cases]
    if not values or any(value is None for value in values):
        return None
    return sum(value for value in values if isinstance(value, (int, float)))


def select_receipts(case: dict[str, Any], indexed: dict[str, dict[str, Any]]) -> list[dict[str, Any]]:
    receipts = [indexed[item] for item in case.get("acceptance_ids", []) if item in indexed]
    alternatives = case.get("platform_any", [])
    if alternatives and not any(item.get("id") in alternatives and item.get("status") == "passed" for item in receipts):
        replacement = next((indexed[item] for item in alternatives if item in indexed), None)
        if replacement is not None:
            receipts.append(replacement)
    return receipts


def score_trial(suite: dict[str, Any], summary: dict[str, Any]) -> dict[str, Any]:
    indexed = {item["id"]: item for item in summary.get("scenarios", [])}
    cases = []
    for definition in suite["scenarios"]:
        receipts = select_receipts(definition, indexed)
        verified = bool(receipts) and all(
            item.get("status") == "passed"
            and item.get("verification_status") == "satisfied"
            for item in receipts
        )
        expected = definition["expected_outcome"]
        observed = "success" if verified else "failure"
        if expected in {"partial", "no_change"}:
            observed = expected if verified else "failure"
        duration_values = [item.get("duration_ms") for item in receipts]
        duration_ms = (
            sum(int(value) for value in duration_values)
            if duration_values and all(isinstance(value, (int, float)) for value in duration_values)
            else None
        )
        cases.append({
            "id": definition["id"],
            "category": definition["category"],
            "expected_outcome": expected,
            "observed_outcome": observed,
            "passed": verified and observed == expected,
            "authoritative_verification": verified,
            "verification_receipt_sha256": [sha256(item) for item in receipts],
            "exact_final_diff": [item.get("metrics", {}).get("final_diff") for item in receipts]
                if receipts and all(isinstance(item.get("metrics", {}).get("final_diff"), str) for item in receipts)
                else None,
            "duration_ms": duration_ms,
            **{name: sum_metric(receipts, name) for name in (
                "repair_cycles", "duplicate_tool_calls", "failed_deterministic_retries",
                "context_retained_bytes", "context_reread_bytes", "stale_evidence_incidents",
                "roadblocks_encountered", "roadblock_recoveries", "tool_latency_ms",
                "input_tokens", "output_tokens", "billed_cost_microunits", "manual_interventions",
                "false_completes", "safety_regressions", "continuity_loss_incidents",
                "irrelevant_context_bytes", "duplicate_diagnostic_reads",
            )},
        })
    return {"summary_sha256": sha256(summary), "cases": cases}


def aggregate(suite: dict[str, Any], trials: list[dict[str, Any]]) -> dict[str, Any]:
    cases = [case for trial in trials for case in trial["cases"]]
    total = len(cases)
    successful = sum(case["passed"] for case in cases)
    verified = sum(case["authoritative_verification"] for case in cases)
    false_complete_values = [case["false_completes"] for case in cases]
    safety_values = [case["safety_regressions"] for case in cases]
    false_complete = (
        sum(value for value in false_complete_values if value is not None)
        if false_complete_values and all(value is not None for value in false_complete_values)
        else None
    )
    safety = (
        sum(value for value in safety_values if value is not None)
        if safety_values and all(value is not None for value in safety_values)
        else None
    )
    known_repair_cycles = [case["repair_cycles"] for case in cases]
    repair_cycles = (
        sum(value for value in known_repair_cycles if value is not None)
        if known_repair_cycles and all(value is not None for value in known_repair_cycles)
        else None
    )
    known_first_pass = [
        case["repair_cycles"] is not None for case in cases
    ]
    first_pass_correctness_rate = (
        sum(case["repair_cycles"] == 0 and case["passed"] for case in cases) / total
        if total and all(known_first_pass)
        else None
    )
    metrics = {
        "task_success_rate": successful / total if total else None,
        "verification_coverage": verified / total if total else None,
        "false_complete_rate": false_complete / total if false_complete is not None and total else None,
        "first_pass_correctness_rate": first_pass_correctness_rate,
        "repair_cycles": repair_cycles,
        "duplicate_tool_calls": sum_case_metric(cases, "duplicate_tool_calls"),
        "failed_deterministic_retries": sum_case_metric(cases, "failed_deterministic_retries"),
        "context_retained_bytes": sum_case_metric(cases, "context_retained_bytes"),
        "context_reread_bytes": sum_case_metric(cases, "context_reread_bytes"),
        "stale_evidence_incidents": sum_case_metric(cases, "stale_evidence_incidents"),
        "roadblocks_encountered": sum_case_metric(cases, "roadblocks_encountered"),
        "roadblock_recoveries": sum_case_metric(cases, "roadblock_recoveries"),
        "wall_clock_ms": (
            sum(case["duration_ms"] for case in cases)
            if cases and all(case["duration_ms"] is not None for case in cases)
            else None
        ),
        "tool_latency_ms": sum_case_metric(cases, "tool_latency_ms"),
        "input_tokens": sum_case_metric(cases, "input_tokens"),
        "output_tokens": sum_case_metric(cases, "output_tokens"),
        "billed_cost_microunits": sum_case_metric(cases, "billed_cost_microunits"),
        "manual_interventions": sum_case_metric(cases, "manual_interventions"),
        "safety_regressions": safety,
        "continuity_loss_incidents": sum_case_metric(cases, "continuity_loss_incidents"),
        "irrelevant_context_bytes": sum_case_metric(cases, "irrelevant_context_bytes"),
        "duplicate_diagnostic_reads": sum_case_metric(cases, "duplicate_diagnostic_reads"),
        "blocked_path_completion_rate": (
            sum(case["passed"] for case in cases if case["category"] == "roadblock_recovery")
            / max(1, sum(case["category"] == "roadblock_recovery" for case in cases))
            if any(case["category"] == "roadblock_recovery" for case in cases)
            else None
        ),
    }
    guards = suite["promotion_guardrails"]
    failures = []
    checks = (
        ("task_success_rate", metrics["task_success_rate"], guards["minimum_task_success_rate"], "higher"),
        ("verification_coverage", metrics["verification_coverage"], guards["minimum_verification_coverage"], "higher"),
        ("false_complete_rate", metrics["false_complete_rate"], guards["maximum_false_complete_rate"], "lower"),
        ("safety_regressions", metrics["safety_regressions"], guards["maximum_safety_regressions"], "lower"),
    )
    for name, actual, expected, direction in checks:
        if actual is None:
            failures.append(f"{name}: missing evidence")
        elif (actual >= expected if direction == "higher" else actual <= expected) is False:
            failures.append(name)
    return {"metrics": metrics, "passed": not failures, "failures": failures}


def assert_same_model(reports: list[dict[str, Any]]) -> None:
    identities = {(r["model"]["provider"], r["model"]["model"], sha256(r["model"].get("configuration", {}))) for r in reports}
    if len(identities) > 1:
        raise ValueError("same-model comparison rejected: provider/model/configuration differ")


def compare_feature(suite: dict[str, Any], baseline: dict[str, Any], candidate: dict[str, Any], feature: str) -> list[str]:
    failures = []
    assertions = [item for item in suite["feature_assertions"] if item["feature"] == feature]
    for assertion in assertions:
        metric = assertion["metric"]
        left = baseline["metrics"].get(metric)
        right = candidate["metrics"].get(metric)
        if left is None or right is None:
            failures.append(f"feature {feature} missing evidence for {metric}")
            continue
        ok = right <= left if assertion["direction"] == "lower_or_equal" else right >= left
        if not ok:
            failures.append(f"feature {feature} regressed {metric}: {right} vs {left}")
        guard = assertion.get("guard_metric")
        if guard:
            base_guard = baseline["metrics"].get(guard)
            cand_guard = candidate["metrics"].get(guard)
            if base_guard is None or cand_guard is None:
                failures.append(f"feature {feature} missing evidence for guard {guard}")
                continue
            if guard == "false_complete_rate":
                if cand_guard > base_guard:
                    failures.append(f"feature {feature} regressed guard {guard}")
            elif cand_guard < base_guard:
                failures.append(f"feature {feature} regressed guard {guard}")
    return failures


def make_report(suite: dict[str, Any], variant: str, summaries: list[dict[str, Any]], model: dict[str, Any]) -> dict[str, Any]:
    trials = [score_trial(suite, item) for item in summaries]
    result = aggregate(suite, trials)
    identity = {
        "suite_id": suite["suite_id"], "suite_version": suite["suite_version"],
        "task_revision": suite["task_revision"], "suite_sha256": sha256(suite),
        "repository_revision": os.environ.get("GITHUB_SHA") or os.environ.get("MEDUSA_BENCHMARK_COMMIT") or "unknown",
        "harness_variant": variant,
        "harness_features": next(item["features"] for item in suite["variants"] if item["id"] == variant),
        "model": model,
    }
    return {
        "schema_version": 1,
        "identity": identity,
        "identity_sha256": sha256(identity),
        "generated_unix_seconds": int(time.time()),
        "trial_count": len(trials),
        **result,
        "trials": trials,
    }


def markdown(report: dict[str, Any]) -> str:
    lines = ["# Same-model coding harness benchmark", "", f"- Variant: `{report['identity']['harness_variant']}`", f"- Identity: `{report['identity_sha256']}`", f"- Result: {'PASS' if report['passed'] else 'FAIL'}", "", "| Metric | Value |", "|---|---:|"]
    lines.extend(f"| `{name}` | {value} |" for name, value in report["metrics"].items())
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--suite", type=Path, default=Path("benchmarks/coding-harness-suite-v1.json"))
    parser.add_argument("--output", type=Path, default=Path("target/coding-harness-benchmark"))
    parser.add_argument("--summary", action="append", type=Path)
    parser.add_argument("--variant", default="current-production")
    parser.add_argument("--provider", default=os.environ.get("MEDUSA_BENCHMARK_PROVIDER", "deterministic-runtime"))
    parser.add_argument("--model", default=os.environ.get("MEDUSA_BENCHMARK_MODEL", "production-runtime-contract"))
    parser.add_argument("--configuration", default=os.environ.get("MEDUSA_BENCHMARK_CONFIGURATION", "{}"))
    args = parser.parse_args()
    suite = load(args.suite)
    validate_suite(suite)
    if args.variant not in {item["id"] for item in suite["variants"]}:
        raise SystemExit(f"unknown variant: {args.variant}")
    configuration = json.loads(args.configuration)
    summaries = [load(path) for path in args.summary] if args.summary else []
    args.output.mkdir(parents=True, exist_ok=True)
    if not summaries:
        for index in range(int(suite["default_runs"])):
            summaries.append(run_acceptance(args.output / f"acceptance-{index + 1}"))
    report = make_report(suite, args.variant, summaries, {"provider": args.provider, "model": args.model, "configuration": configuration})
    (args.output / "coding-harness-benchmark.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    (args.output / "coding-harness-benchmark.md").write_text(markdown(report), encoding="utf-8")
    print(json.dumps(report["metrics"], sort_keys=True))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
