#!/usr/bin/env python3
"""Aggregate and validate cross-platform live-provider dogfood evidence."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

EXPECTED_PLATFORMS = {"Linux", "macOS", "Windows"}
ALLOWED_FAILURE_CLASSES = {"product", "provider", "environment", "flaky-test"}


def load_summaries(root: Path) -> list[dict[str, Any]]:
    summaries: list[dict[str, Any]] = []
    for path in sorted(root.rglob("summary.json")):
        data = json.loads(path.read_text(encoding="utf-8"))
        if "platform" in data and "provider" in data:
            data["_path"] = str(path)
            summaries.append(data)
    return summaries


def validate(summaries: list[dict[str, Any]]) -> list[str]:
    errors: list[str] = []
    if not summaries:
        return ["no live-provider summaries were found"]
    by_platform = {str(summary.get("platform")): summary for summary in summaries}
    missing = EXPECTED_PLATFORMS - set(by_platform)
    extras = set(by_platform) - EXPECTED_PLATFORMS
    if missing:
        errors.append(f"missing platform evidence: {', '.join(sorted(missing))}")
    if extras:
        errors.append(f"unexpected platform evidence: {', '.join(sorted(extras))}")
    if len(by_platform) != len(summaries):
        errors.append("duplicate platform summaries were found")
    commits = {str(summary.get("commit")) for summary in summaries}
    if len(commits) != 1 or "unknown" in commits or "None" in commits:
        errors.append(f"evidence does not share one exact commit: {sorted(commits)}")
    for summary in summaries:
        platform = summary.get("platform", "unknown")
        if summary.get("schema_version") != 1:
            errors.append(f"{platform}: unsupported schema version")
        if summary.get("result") != "passed":
            errors.append(
                f"{platform}: live dogfood failed ({summary.get('classification')}: {summary.get('detail')})"
            )
        classification = summary.get("classification")
        if classification is not None and classification not in ALLOWED_FAILURE_CLASSES:
            errors.append(f"{platform}: invalid failure classification {classification}")
        if summary.get("passed") != summary.get("total") or summary.get("total") != 3:
            errors.append(f"{platform}: independent assertions are incomplete")
        if summary.get("credential_persisted") is not False:
            errors.append(f"{platform}: credential non-persistence was not proven")
        if summary.get("verification_contract_unchanged") is not True:
            errors.append(f"{platform}: verification contract integrity was not proven")
        bounded = summary.get("bounded")
        if not isinstance(bounded, dict) or not all(
            isinstance(bounded.get(key), int) and bounded[key] > 0
            for key in (
                "timeout_seconds", "max_turns", "parallel_workers", "max_output_tokens",
                "context_window_tokens", "max_retries", "max_cost_microusd",
            )
        ):
            errors.append(f"{platform}: execution budgets are missing or invalid")
        usage = summary.get("usage")
        if not isinstance(usage, dict) or not all(
            isinstance(usage.get(key), int) and usage[key] > 0
            for key in ("model_turns", "total_tokens")
        ) or not isinstance(usage.get("estimated_cost_microusd"), int):
            errors.append(f"{platform}: durable usage and cost evidence is missing")
        elif isinstance(bounded, dict) and usage["estimated_cost_microusd"] > bounded.get("max_cost_microusd", -1):
            errors.append(f"{platform}: estimated cost exceeded the declared budget")
        build = summary.get("build")
        if not isinstance(build, dict) or not isinstance(build.get("binary_sha256"), str) or len(build["binary_sha256"]) != 64:
            errors.append(f"{platform}: installed executable identity is missing")
    return errors


def render_markdown(summaries: list[dict[str, Any]], errors: list[str]) -> str:
    commits = sorted({str(summary.get("commit", "unknown")) for summary in summaries})
    lines = [
        "# Live-provider dogfood report",
        "",
        f"- Commit: `{commits[0] if len(commits) == 1 else ', '.join(commits)}`",
        "- Provider/model: `minimax / MiniMax-M3`",
        "- Scenario: one bounded multi-language repository repair with three independent assertions",
        f"- Overall result: **{'passed' if not errors else 'failed'}**",
        "",
        "| Platform | Result | Assertions | Tokens | Cost (micro-USD) | Elapsed | Failure class |",
        "|---|---:|---:|---:|---:|---:|---|",
    ]
    for summary in sorted(summaries, key=lambda item: str(item.get("platform"))):
        lines.append(
            "| {platform} | {result} | {passed}/{total} | {tokens} | {cost} | {elapsed}s | {classification} |".format(
                platform=summary.get("platform", "unknown"),
                result=summary.get("result", "unknown"),
                passed=summary.get("passed", 0),
                total=summary.get("total", 0),
                tokens=(summary.get("usage") or {}).get("total_tokens", 0),
                cost=(summary.get("usage") or {}).get("estimated_cost_microusd", 0),
                elapsed=summary.get("elapsed_seconds", 0),
                classification=summary.get("classification") or "—",
            )
        )
    lines.extend(["", "## Validation"])
    if errors:
        lines.extend(f"- FAIL: {error}" for error in errors)
    else:
        lines.extend(
            [
                "- All three platforms passed on the same immutable commit.",
                "- The verification contract remained byte-identical.",
                "- The provider credential was environment-only and absent from retained evidence.",
                "- Timeout, turn, worker, context, output-token, retry, and estimated-cost budgets were explicit and bounded.",
                "- Every run used a staged public `medusa` executable from an unrelated working directory and recorded its SHA-256 identity.",
            ]
        )
    lines.extend(
        [
            "",
            "## Known limitations",
            "",
            "- This is one bounded coding scenario, not a provider availability or performance guarantee.",
            "- It exercises MiniMax-M3; other provider routes retain their own compatibility and authentication gates.",
            "- Hosted-runner timing is diagnostic evidence and is not used as a product latency SLO.",
            "",
        ]
    )
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--json", type=Path, required=True)
    parser.add_argument("--markdown", type=Path, required=True)
    args = parser.parse_args()
    summaries = load_summaries(args.input)
    errors = validate(summaries)
    payload = {
        "schema_version": 1,
        "result": "passed" if not errors else "failed",
        "errors": errors,
        "platforms": summaries,
    }
    args.json.parent.mkdir(parents=True, exist_ok=True)
    args.json.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    args.markdown.write_text(render_markdown(summaries, errors), encoding="utf-8")
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
