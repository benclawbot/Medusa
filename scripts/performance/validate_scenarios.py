#!/usr/bin/env python3
"""Validate the versioned Medusa performance scenario corpus."""

from __future__ import annotations

import json
import sys
from pathlib import Path

REQUIRED_IDS = {
    "repository_lookup",
    "rust_single_file_fix",
    "typescript_multi_file_feature",
    "failing_test_repair",
    "cross_package_refactor",
    "browser_verified_ui_change",
    "security_sensitive_change",
    "ambiguous_scope_fail_closed",
    "provider_failure_recovery",
    "verification_failure_repair",
    "crash_resume",
    "multi_implementer_conflict",
}
VALID_MODES = {"cold", "warm", "injected_provider_latency", "injected_tool_latency"}


def main(argv: list[str]) -> int:
    path = Path(argv[1] if len(argv) > 1 else "benchmarks/performance/scenarios.json")
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        print(error, file=sys.stderr)
        return 2
    if data.get("schema_version") != 1 or not isinstance(data.get("scenarios"), list):
        print("invalid scenario corpus envelope", file=sys.stderr)
        return 1
    seen: set[str] = set()
    for scenario in data["scenarios"]:
        scenario_id = scenario.get("id")
        modes = scenario.get("modes")
        if not isinstance(scenario_id, str) or not scenario_id or scenario_id in seen:
            print(f"invalid or duplicate scenario id: {scenario_id!r}", file=sys.stderr)
            return 1
        seen.add(scenario_id)
        if scenario.get("language") not in {"rust", "typescript", "mixed"}:
            print(f"{scenario_id}: invalid language", file=sys.stderr)
            return 1
        if scenario.get("risk") not in {"low", "medium", "high"}:
            print(f"{scenario_id}: invalid risk", file=sys.stderr)
            return 1
        if not isinstance(modes, list) or not modes or any(mode not in VALID_MODES for mode in modes):
            print(f"{scenario_id}: invalid modes", file=sys.stderr)
            return 1
    missing = sorted(REQUIRED_IDS - seen)
    if missing:
        print(f"missing required scenarios: {', '.join(missing)}", file=sys.stderr)
        return 1
    print(f"PASS: {len(seen)} representative scenarios validated")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
