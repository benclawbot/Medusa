#!/usr/bin/env python3
"""Explain and validate Medusa's change-triggered engineering policy.

The policy is intentionally advisory for local planning and authoritative when
invoked by CI.  Policy/evaluator changes are evaluated against the base policy
so a patch cannot weaken the rules used to approve itself.
"""
from __future__ import annotations

import argparse
import fnmatch
import json
import pathlib
import subprocess
import sys
import tempfile
from typing import Any

SCHEMA_VERSION = 1
REQUIRED_RULE_FIELDS = {"id", "description", "include", "required_checks", "protected"}


def load_policy(path: pathlib.Path) -> dict[str, Any]:
    try:
        policy = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ValueError(f"cannot load engineering policy {path}: {exc}") from exc
    validate_policy(policy)
    return policy


def validate_policy(policy: dict[str, Any]) -> None:
    if policy.get("schema_version") != SCHEMA_VERSION:
        raise ValueError(
            f"unsupported engineering policy schema_version={policy.get('schema_version')!r}; "
            f"expected {SCHEMA_VERSION}"
        )
    if not isinstance(policy.get("policy_id"), str) or not policy["policy_id"]:
        raise ValueError("policy_id must be a non-empty string")
    protected_paths = policy.get("protected_policy_paths")
    if not isinstance(protected_paths, list) or not protected_paths or not all(isinstance(x, str) and x for x in protected_paths):
        raise ValueError("protected_policy_paths must be a non-empty string list")
    rules = policy.get("rules")
    if not isinstance(rules, list) or not rules:
        raise ValueError("rules must be a non-empty list")
    seen: set[str] = set()
    for index, rule in enumerate(rules):
        if not isinstance(rule, dict):
            raise ValueError(f"rules[{index}] must be an object")
        missing = REQUIRED_RULE_FIELDS - set(rule)
        if missing:
            raise ValueError(f"rules[{index}] missing fields: {', '.join(sorted(missing))}")
        rule_id = rule["id"]
        if not isinstance(rule_id, str) or not rule_id:
            raise ValueError(f"rules[{index}].id must be non-empty")
        if rule_id in seen:
            raise ValueError(f"duplicate rule id: {rule_id}")
        seen.add(rule_id)
        for field in ("include", "exclude", "required_checks"):
            value = rule.get(field, [])
            if not isinstance(value, list) or not all(isinstance(x, str) and x for x in value):
                raise ValueError(f"rule {rule_id}: {field} must be a string list")
        if not rule["include"]:
            raise ValueError(f"rule {rule_id}: include must not be empty")
        if not rule["required_checks"]:
            raise ValueError(f"rule {rule_id}: required_checks must not be empty")
        if not isinstance(rule["protected"], bool):
            raise ValueError(f"rule {rule_id}: protected must be boolean")
        if "evaluate_against_base" in rule and not isinstance(rule["evaluate_against_base"], bool):
            raise ValueError(f"rule {rule_id}: evaluate_against_base must be boolean")


def normalize_path(value: str) -> str:
    path = value.replace("\\", "/")
    while path.startswith("./"):
        path = path[2:]
    if not path or path.startswith("/") or ".." in pathlib.PurePosixPath(path).parts:
        raise ValueError(f"invalid repository-relative path: {value!r}")
    return path


def matches(pattern: str, path: str) -> bool:
    # fnmatch is deliberately used over pathlib matching: it gives identical
    # behavior across Linux/macOS/Windows for normalized repository paths.
    return fnmatch.fnmatchcase(path, pattern)


def rule_matches(rule: dict[str, Any], path: str) -> bool:
    if not any(matches(pattern, path) for pattern in rule["include"]):
        return False
    return not any(matches(pattern, path) for pattern in rule.get("exclude", []))


def resolve(policy: dict[str, Any], paths: list[str]) -> dict[str, Any]:
    normalized = sorted(set(normalize_path(path) for path in paths))
    triggered: list[dict[str, Any]] = []
    checks: set[str] = set()
    protected = False
    for rule in policy["rules"]:
        matched_paths = [path for path in normalized if rule_matches(rule, path)]
        if not matched_paths:
            continue
        item = {
            "id": rule["id"],
            "description": rule["description"],
            "matched_paths": matched_paths,
            "required_checks": sorted(set(rule["required_checks"])),
            "protected": rule["protected"],
            "evaluate_against_base": bool(rule.get("evaluate_against_base", False)),
        }
        triggered.append(item)
        checks.update(item["required_checks"])
        protected = protected or item["protected"]
    return {
        "schema_version": SCHEMA_VERSION,
        "policy_id": policy["policy_id"],
        "paths": normalized,
        "triggered_rules": triggered,
        "required_checks": sorted(checks),
        "protected_change": protected,
    }


def changed_paths(root: pathlib.Path, base: str, head: str) -> list[str]:
    result = subprocess.run(
        ["git", "diff", "--name-only", "--diff-filter=ACDMRTUXB", f"{base}...{head}"],
        cwd=root,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode:
        raise ValueError(result.stderr.strip() or "git diff failed")
    return [line for line in result.stdout.splitlines() if line.strip()]


def policy_paths_touched(policy: dict[str, Any], paths: list[str]) -> bool:
    protected = set(policy["protected_policy_paths"])
    return any(path in protected for path in paths)


def explain(policy: dict[str, Any], paths: list[str], base_policy: dict[str, Any] | None) -> dict[str, Any]:
    normalized = [normalize_path(path) for path in paths]
    current = resolve(policy, normalized)
    current["evaluation_policy"] = "head"
    if policy_paths_touched(policy, normalized):
        if base_policy is None:
            raise ValueError("engineering policy/evaluator changed but no base policy was supplied; fail closed")
        base = resolve(base_policy, normalized)
        base["evaluation_policy"] = "base"
        base["head_policy_id"] = policy["policy_id"]
        base["policy_change_protected"] = True
        return base
    return current


def render_human(report: dict[str, Any]) -> str:
    lines = [
        f"engineering policy: {report['policy_id']} ({report.get('evaluation_policy', 'head')})",
        f"protected change: {'yes' if report['protected_change'] else 'no'}",
    ]
    if not report["triggered_rules"]:
        lines.append("triggered rules: none")
    else:
        lines.append("triggered rules:")
        for rule in report["triggered_rules"]:
            lines.append(f"- {rule['id']}: {rule['description']}")
            lines.append(f"  paths: {', '.join(rule['matched_paths'])}")
            lines.append(f"  checks: {', '.join(rule['required_checks'])}")
    lines.append("required checks: " + (", ".join(report["required_checks"]) or "none"))
    return "\n".join(lines)


def self_test() -> int:
    policy = {
        "schema_version": 1,
        "policy_id": "fixture-v1",
        "protected_policy_paths": ["policy.json", "checker.py"],
        "rules": [
            {
                "id": "docs",
                "description": "docs",
                "include": ["docs/**"],
                "exclude": ["docs/architecture/**"],
                "required_checks": ["documentation"],
                "protected": False,
            },
            {
                "id": "containment",
                "description": "containment",
                "include": ["crates/containment/**"],
                "required_checks": ["linux", "macos", "windows"],
                "protected": True,
            },
            {
                "id": "policy",
                "description": "policy",
                "include": ["policy.json", "checker.py"],
                "required_checks": ["base-policy"],
                "protected": True,
                "evaluate_against_base": True,
            },
        ],
    }
    validate_policy(policy)
    docs = resolve(policy, ["docs/guide.md"])
    assert docs["required_checks"] == ["documentation"]
    assert not docs["protected_change"]
    assert not resolve(policy, ["docs/architecture/authority.md"])["triggered_rules"]
    containment = resolve(policy, ["crates/containment/src/lib.rs"])
    assert containment["required_checks"] == ["linux", "macos", "windows"]
    assert containment["protected_change"]
    assert resolve(policy, ["crates/containment/src/lib.rs", "crates/containment/src/lib.rs"]) == containment
    try:
        explain(policy, ["policy.json"], None)
    except ValueError as exc:
        assert "fail closed" in str(exc)
    else:
        raise AssertionError("policy mutation without base policy must fail closed")
    base = dict(policy)
    base["policy_id"] = "fixture-base"
    report = explain(policy, ["policy.json"], base)
    assert report["evaluation_policy"] == "base"
    assert report["policy_id"] == "fixture-base"
    assert report["required_checks"] == ["base-policy"]
    with tempfile.TemporaryDirectory() as temp:
        path = pathlib.Path(temp) / "policy.json"
        path.write_text(json.dumps(policy), encoding="utf-8")
        assert load_policy(path)["policy_id"] == "fixture-v1"
    print("engineering policy self-test passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    validate_parser = sub.add_parser("validate")
    validate_parser.add_argument("--policy", type=pathlib.Path, required=True)

    explain_parser = sub.add_parser("explain")
    explain_parser.add_argument("--policy", type=pathlib.Path, required=True)
    explain_parser.add_argument("--base-policy", type=pathlib.Path)
    explain_parser.add_argument("--path", dest="paths", action="append", default=[])
    explain_parser.add_argument("--root", type=pathlib.Path, default=pathlib.Path("."))
    explain_parser.add_argument("--base")
    explain_parser.add_argument("--head", default="HEAD")
    explain_parser.add_argument("--json", action="store_true")

    sub.add_parser("self-test")
    args = parser.parse_args()

    try:
        if args.command == "self-test":
            return self_test()
        policy = load_policy(args.policy)
        if args.command == "validate":
            print(f"engineering policy valid: {policy['policy_id']}")
            return 0
        paths = list(args.paths)
        if args.base:
            paths.extend(changed_paths(args.root.resolve(), args.base, args.head))
        if not paths:
            raise ValueError("explain requires at least one --path or --base")
        base_policy = load_policy(args.base_policy) if args.base_policy else None
        report = explain(policy, paths, base_policy)
        print(json.dumps(report, indent=2, sort_keys=True) if args.json else render_human(report))
        return 0
    except ValueError as exc:
        print(f"engineering policy error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
