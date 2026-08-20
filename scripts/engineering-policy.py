#!/usr/bin/env python3
"""Explain, validate, and enforce Medusa's change-triggered engineering policy."""
from __future__ import annotations

import argparse
import fnmatch
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
from typing import Any

SCHEMA_VERSION = 1
REQUIRED_RULE_FIELDS = {"id", "description", "include", "required_checks", "protected"}
AUTHORITY_PATHS = {
    ".github/engineering-policy.json",
    "scripts/engineering-policy.py",
    ".github/workflows/architecture-policy.yml",
}
TERMINAL_BAD_CONCLUSIONS = {"failure", "cancelled", "timed_out", "action_required", "stale", "skipped"}


def string_list(value: Any, label: str, *, nonempty: bool = False) -> list[str]:
    if not isinstance(value, list) or not all(isinstance(x, str) and x for x in value):
        raise ValueError(f"{label} must be a string list")
    if nonempty and not value:
        raise ValueError(f"{label} must not be empty")
    return value


def load_policy(path: pathlib.Path, root: pathlib.Path | None = None) -> dict[str, Any]:
    try:
        policy = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ValueError(f"cannot load engineering policy {path}: {exc}") from exc
    validate_policy(policy, root)
    return policy


def validate_check_ids(checks: list[str], registry: dict[str, Any], label: str) -> None:
    unknown = sorted(set(checks) - set(registry))
    if unknown:
        raise ValueError(f"{label}: unregistered required checks: {', '.join(unknown)}")


def validate_policy(policy: dict[str, Any], root: pathlib.Path | None = None) -> None:
    if policy.get("schema_version") != SCHEMA_VERSION:
        raise ValueError(
            f"unsupported engineering policy schema_version={policy.get('schema_version')!r}; expected {SCHEMA_VERSION}"
        )
    if not isinstance(policy.get("policy_id"), str) or not policy["policy_id"]:
        raise ValueError("policy_id must be a non-empty string")
    string_list(policy.get("protected_policy_paths"), "protected_policy_paths", nonempty=True)

    registry = policy.get("check_registry")
    if not isinstance(registry, dict) or not registry:
        raise ValueError("check_registry must be a non-empty object")
    for check_id, entry in registry.items():
        if not isinstance(check_id, str) or not check_id or not isinstance(entry, dict):
            raise ValueError("check_registry entries must have non-empty string ids and object values")
        kind = entry.get("kind")
        if kind not in {"inline", "command", "workflow"}:
            raise ValueError(f"check {check_id}: unsupported kind {kind!r}")
        if kind == "command":
            command = string_list(entry.get("command"), f"check {check_id}.command", nonempty=True)
            if root is not None and len(command) > 1 and command[1].startswith("scripts/") and not (root / command[1]).is_file():
                raise ValueError(f"check {check_id}: registered command path does not exist: {command[1]}")
        if kind == "workflow" and (not isinstance(entry.get("workflow_name"), str) or not entry["workflow_name"]):
            raise ValueError(f"check {check_id}: workflow_name must be non-empty")

    unsafe = policy.get("unsafe_code_policy")
    if not isinstance(unsafe, dict) or unsafe.get("default") != "forbid":
        raise ValueError("unsafe_code_policy.default must be 'forbid'")
    string_list(unsafe.get("allowed_ffi_paths"), "unsafe_code_policy.allowed_ffi_paths", nonempty=True)
    validate_check_ids([unsafe.get("required_check")], registry, "unsafe_code_policy")

    relation_ids: set[str] = set()
    for index, relation in enumerate(policy.get("source_of_truth_relationships", [])):
        if not isinstance(relation, dict) or not isinstance(relation.get("id"), str) or not relation["id"]:
            raise ValueError(f"source_of_truth_relationships[{index}] requires an id")
        if relation["id"] in relation_ids:
            raise ValueError(f"duplicate source-of-truth id: {relation['id']}")
        relation_ids.add(relation["id"])
        string_list(relation.get("sources"), f"source_of_truth {relation['id']}.sources", nonempty=True)
        string_list(relation.get("generated", []), f"source_of_truth {relation['id']}.generated")
        string_list(relation.get("synchronized", []), f"source_of_truth {relation['id']}.synchronized")
        checks = string_list(relation.get("required_checks"), f"source_of_truth {relation['id']}.required_checks", nonempty=True)
        validate_check_ids(checks, registry, f"source_of_truth {relation['id']}")

    authority_ids: set[str] = set()
    for index, authority in enumerate(policy.get("canonical_truth_stores", [])):
        if not isinstance(authority, dict) or not isinstance(authority.get("id"), str) or not authority["id"]:
            raise ValueError(f"canonical_truth_stores[{index}] requires an id")
        if authority["id"] in authority_ids:
            raise ValueError(f"duplicate canonical truth id: {authority['id']}")
        authority_ids.add(authority["id"])
        string_list(authority.get("authority_paths"), f"canonical_truth {authority['id']}.authority_paths", nonempty=True)
        string_list(authority.get("governed_paths"), f"canonical_truth {authority['id']}.governed_paths", nonempty=True)
        if authority.get("parallel_authority_policy") not in {"reject", "reject_or_protected_review"}:
            raise ValueError(f"canonical_truth {authority['id']}: invalid parallel_authority_policy")
        checks = string_list(authority.get("required_checks"), f"canonical_truth {authority['id']}.required_checks", nonempty=True)
        validate_check_ids(checks, registry, f"canonical_truth {authority['id']}")

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
        string_list(rule.get("include"), f"rule {rule_id}.include", nonempty=True)
        string_list(rule.get("exclude", []), f"rule {rule_id}.exclude")
        checks = string_list(rule.get("required_checks"), f"rule {rule_id}.required_checks", nonempty=True)
        validate_check_ids(checks, registry, f"rule {rule_id}")
        for conditional in rule.get("conditional_checks", []):
            if not isinstance(conditional, dict) or not isinstance(conditional.get("check"), str):
                raise ValueError(f"rule {rule_id}: invalid conditional_checks entry")
            if not isinstance(conditional.get("when"), str) or not conditional["when"]:
                raise ValueError(f"rule {rule_id}: conditional check requires non-empty when")
            validate_check_ids([conditional["check"]], registry, f"rule {rule_id}")
            validate_check_ids(string_list(conditional.get("substitute_evidence", []), f"rule {rule_id}.substitute_evidence"), registry, f"rule {rule_id}")
        for relation_id in string_list(rule.get("source_of_truth", []), f"rule {rule_id}.source_of_truth"):
            if relation_id not in relation_ids:
                raise ValueError(f"rule {rule_id}: unknown source_of_truth id {relation_id}")
        for authority_id in string_list(rule.get("canonical_authorities", []), f"rule {rule_id}.canonical_authorities"):
            if authority_id not in authority_ids:
                raise ValueError(f"rule {rule_id}: unknown canonical authority id {authority_id}")
        if not isinstance(rule["protected"], bool):
            raise ValueError(f"rule {rule_id}: protected must be boolean")
        if "evaluate_against_base" in rule and not isinstance(rule["evaluate_against_base"], bool):
            raise ValueError(f"rule {rule_id}: evaluate_against_base must be boolean")
        if rule.get("promotion", "bounded-autonomous") not in {"bounded-autonomous", "protected-review"}:
            raise ValueError(f"rule {rule_id}: invalid promotion")


def normalize_path(value: str) -> str:
    path = value.replace("\\", "/")
    while path.startswith("./"):
        path = path[2:]
    if not path or path.startswith("/") or ".." in pathlib.PurePosixPath(path).parts:
        raise ValueError(f"invalid repository-relative path: {value!r}")
    return path


def matches(pattern: str, path: str) -> bool:
    return fnmatch.fnmatchcase(path, pattern)


def any_match(patterns: list[str], paths: list[str]) -> list[str]:
    return sorted(path for path in paths if any(matches(pattern, path) for pattern in patterns))


def rule_matches(rule: dict[str, Any], path: str) -> bool:
    if not any(matches(pattern, path) for pattern in rule["include"]):
        return False
    return not any(matches(pattern, path) for pattern in rule.get("exclude", []))


def resolve(policy: dict[str, Any], paths: list[str], task_type: str | None = None, satisfied: set[str] | None = None, conditions: set[str] | None = None) -> dict[str, Any]:
    normalized = sorted(set(normalize_path(path) for path in paths))
    satisfied = satisfied or set()
    conditions = conditions or set()
    triggered: list[dict[str, Any]] = []
    checks: set[str] = set()
    protected = False
    source_ids: set[str] = set()
    authority_ids: set[str] = set()
    platforms: set[str] = set()
    providers: set[str] = set()
    evidence: set[str] = set()
    policy_sources: set[str] = set()
    promotions: set[str] = set()
    conditional_checks: list[dict[str, Any]] = []

    for rule in policy["rules"]:
        matched_paths = [path for path in normalized if rule_matches(rule, path)]
        if not matched_paths:
            continue
        rendered_conditionals: list[dict[str, Any]] = []
        for conditional in rule.get("conditional_checks", []):
            rendered = dict(conditional)
            rendered["active"] = conditional["when"] in conditions
            rendered["required"] = [conditional["check"]] if rendered["active"] else list(conditional.get("substitute_evidence", []))
            checks.update(rendered["required"])
            rendered_conditionals.append(rendered)
        item = {
            "id": rule["id"],
            "description": rule["description"],
            "matched_paths": matched_paths,
            "required_checks": sorted(set(rule["required_checks"])),
            "protected": rule["protected"],
            "evaluate_against_base": bool(rule.get("evaluate_against_base", False)),
            "policy_sources": sorted(set(rule.get("policy_sources", []))),
            "source_of_truth": sorted(set(rule.get("source_of_truth", []))),
            "canonical_authorities": sorted(set(rule.get("canonical_authorities", []))),
            "platforms": sorted(set(rule.get("platforms", []))),
            "providers": sorted(set(rule.get("providers", []))),
            "evidence": sorted(set(rule.get("evidence", []))),
            "promotion": rule.get("promotion", "bounded-autonomous"),
            "conditional_checks": rendered_conditionals,
        }
        triggered.append(item)
        checks.update(item["required_checks"])
        protected = protected or item["protected"]
        source_ids.update(item["source_of_truth"])
        authority_ids.update(item["canonical_authorities"])
        platforms.update(item["platforms"])
        providers.update(item["providers"])
        evidence.update(item["evidence"])
        policy_sources.update(item["policy_sources"])
        promotions.add(item["promotion"])
        conditional_checks.extend(rendered_conditionals)

    source_relations: list[dict[str, Any]] = []
    for relation in policy.get("source_of_truth_relationships", []):
        relation_paths = relation["sources"] + relation.get("generated", []) + relation.get("synchronized", [])
        matched = any_match(relation_paths, normalized)
        if matched or relation["id"] in source_ids:
            rendered = dict(relation)
            rendered["matched_paths"] = matched
            source_relations.append(rendered)
            checks.update(relation["required_checks"])
            source_ids.add(relation["id"])

    canonical: list[dict[str, Any]] = []
    for authority in policy.get("canonical_truth_stores", []):
        matched = any_match(authority["authority_paths"] + authority["governed_paths"], normalized)
        if matched or authority["id"] in authority_ids:
            rendered = dict(authority)
            rendered["matched_paths"] = matched
            canonical.append(rendered)
            checks.update(authority["required_checks"])
            authority_ids.add(authority["id"])
            protected = True
            promotions.add("protected-review")

    required_checks = sorted(checks)
    registry = policy["check_registry"]
    check_sources = {check: registry[check] for check in required_checks}
    return {
        "schema_version": SCHEMA_VERSION,
        "policy_id": policy["policy_id"],
        "task_type": task_type,
        "conditions": sorted(conditions),
        "paths": normalized,
        "triggered_rules": triggered,
        "required_checks": required_checks,
        "missing_evidence": sorted(set(required_checks) - satisfied),
        "check_sources": check_sources,
        "source_of_truth_relationships": source_relations,
        "canonical_truth_stores": canonical,
        "platforms": sorted(platforms),
        "providers": sorted(providers),
        "conditional_checks": conditional_checks,
        "evidence_requirements": sorted(evidence),
        "policy_sources": sorted(policy_sources),
        "promotion": "protected-review" if "protected-review" in promotions else "bounded-autonomous",
        "protected_change": protected,
        "unsafe_code_policy": policy["unsafe_code_policy"],
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


def protected_policy_paths(policy: dict[str, Any], base_policy: dict[str, Any] | None = None) -> set[str]:
    paths = set(AUTHORITY_PATHS)
    paths.update(policy.get("protected_policy_paths", []))
    if base_policy is not None:
        paths.update(base_policy.get("protected_policy_paths", []))
    return paths


def policy_paths_touched(policy: dict[str, Any], paths: list[str], base_policy: dict[str, Any] | None = None) -> bool:
    protected = protected_policy_paths(policy, base_policy)
    return any(path in protected for path in paths)


def explain(policy: dict[str, Any], paths: list[str], base_policy: dict[str, Any] | None, task_type: str | None = None, satisfied: set[str] | None = None, conditions: set[str] | None = None) -> dict[str, Any]:
    normalized = [normalize_path(path) for path in paths]
    current = resolve(policy, normalized, task_type, satisfied, conditions)
    current["evaluation_policy"] = "head"
    if policy_paths_touched(policy, normalized, base_policy):
        if base_policy is None:
            raise ValueError("engineering policy/evaluator changed but no base policy was supplied; fail closed")
        base = resolve(base_policy, normalized, task_type, satisfied, conditions)
        base["evaluation_policy"] = "base"
        base["head_policy_id"] = policy["policy_id"]
        base["policy_change_protected"] = True
        return base
    return current


def render_human(report: dict[str, Any]) -> str:
    lines = [
        f"engineering policy: {report['policy_id']} ({report.get('evaluation_policy', 'head')})",
        f"protected change: {'yes' if report['protected_change'] else 'no'}",
        f"promotion: {report['promotion']}",
    ]
    if report.get("task_type"):
        lines.append(f"task type: {report['task_type']}")
    if report.get("conditions"):
        lines.append("active conditions: " + ", ".join(report["conditions"]))
    if not report["triggered_rules"]:
        lines.append("triggered rules: none")
    else:
        lines.append("triggered rules:")
        for rule in report["triggered_rules"]:
            lines.append(f"- {rule['id']}: {rule['description']}")
            lines.append(f"  paths: {', '.join(rule['matched_paths'])}")
            lines.append(f"  checks: {', '.join(rule['required_checks'])}")
    lines.append("required checks: " + (", ".join(report["required_checks"]) or "none"))
    lines.append("missing evidence: " + (", ".join(report["missing_evidence"]) or "none"))
    if report["source_of_truth_relationships"]:
        lines.append("source-of-truth relationships: " + ", ".join(item["id"] for item in report["source_of_truth_relationships"]))
    if report["canonical_truth_stores"]:
        lines.append("canonical truth stores: " + ", ".join(item["id"] for item in report["canonical_truth_stores"]))
    if report["platforms"]:
        lines.append("platforms: " + ", ".join(report["platforms"]))
    if report["providers"]:
        lines.append("providers: " + ", ".join(report["providers"]))
    return "\n".join(lines)


def github_workflow_runs(repository: str, head: str, token: str) -> list[dict[str, Any]]:
    query = urllib.parse.urlencode({"head_sha": head, "event": "pull_request", "per_page": 100})
    request = urllib.request.Request(
        f"https://api.github.com/repos/{repository}/actions/runs?{query}",
        headers={"Accept": "application/vnd.github+json", "Authorization": f"Bearer {token}", "X-GitHub-Api-Version": "2022-11-28"},
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            payload = json.load(response)
    except Exception as exc:
        raise ValueError(f"cannot query GitHub workflow runs: {exc}") from exc
    runs = payload.get("workflow_runs")
    if not isinstance(runs, list):
        raise ValueError("GitHub workflow response missing workflow_runs")
    return runs


def enforce(policy: dict[str, Any], report: dict[str, Any], root: pathlib.Path, satisfied: set[str], repository: str | None, head: str | None, token: str | None, timeout_seconds: int) -> None:
    registry = policy["check_registry"]
    workflows: dict[str, list[str]] = {}
    for check_id in report.get("required_checks", []):
        entry = registry.get(check_id)
        if entry is None:
            raise ValueError(f"required check is not registered: {check_id}")
        kind = entry["kind"]
        if kind == "inline":
            if check_id not in satisfied:
                raise ValueError(f"inline required check has no success evidence: {check_id}")
        elif kind == "command":
            completed = subprocess.run(entry["command"], cwd=root, check=False)
            if completed.returncode:
                raise ValueError(f"required command check failed: {check_id}")
        else:
            workflows.setdefault(entry["workflow_name"], []).append(check_id)
    if not workflows:
        return
    if not repository or not head or not token:
        raise ValueError("workflow-backed checks require repository, head SHA, and GitHub token")
    deadline = time.monotonic() + max(0, timeout_seconds)
    pending = set(workflows)
    while pending:
        runs = github_workflow_runs(repository, head, token)
        for workflow_name in list(pending):
            candidates = [run for run in runs if run.get("name") == workflow_name]
            if not candidates:
                continue
            latest = max(candidates, key=lambda run: int(run.get("id", 0)))
            status = latest.get("status")
            conclusion = latest.get("conclusion")
            if status == "completed" and conclusion == "success":
                pending.remove(workflow_name)
            elif status == "completed" and conclusion in TERMINAL_BAD_CONCLUSIONS:
                checks = ", ".join(workflows[workflow_name])
                raise ValueError(f"required workflow {workflow_name!r} for {checks} concluded {conclusion}")
        if not pending:
            return
        if time.monotonic() >= deadline:
            raise ValueError(f"required workflows did not complete successfully on head {head}: {', '.join(sorted(pending))}")
        time.sleep(10)


def fixture_policy() -> dict[str, Any]:
    return {
        "schema_version": 1,
        "policy_id": "fixture-v1",
        "protected_policy_paths": ["policy.json", "checker.py"],
        "unsafe_code_policy": {"default": "forbid", "allowed_ffi_paths": ["ffi/**"], "required_check": "linux"},
        "source_of_truth_relationships": [{"id": "generated-doc", "sources": ["source.json"], "generated": ["generated.md"], "synchronized": [], "required_checks": ["documentation"]}],
        "canonical_truth_stores": [{"id": "state", "authority_paths": ["state.rs"], "governed_paths": ["state/**"], "parallel_authority_policy": "reject_or_protected_review", "required_checks": ["base-policy"]}],
        "check_registry": {
            "documentation": {"kind": "command", "command": [sys.executable, "-c", "pass"]},
            "linux": {"kind": "workflow", "workflow_name": "Linux"},
            "macos": {"kind": "workflow", "workflow_name": "macOS"},
            "windows": {"kind": "workflow", "workflow_name": "Windows"},
            "base-policy": {"kind": "inline"},
        },
        "rules": [
            {"id": "docs", "description": "docs", "include": ["docs/**"], "exclude": ["docs/architecture/**"], "required_checks": ["documentation"], "protected": False},
            {"id": "containment", "description": "containment", "include": ["crates/containment/**"], "required_checks": ["linux", "macos"], "conditional_checks": [{"check": "windows", "when": "windows-required", "substitute_evidence": ["documentation"]}], "platforms": ["linux", "macos", "windows"], "protected": True},
            {"id": "generated", "description": "generated", "include": ["source.json", "generated.md"], "required_checks": ["documentation"], "source_of_truth": ["generated-doc"], "protected": False},
            {"id": "state", "description": "state", "include": ["state.rs", "state/**"], "required_checks": ["base-policy"], "canonical_authorities": ["state"], "protected": True},
            {"id": "policy", "description": "policy", "include": ["policy.json", "checker.py"], "required_checks": ["base-policy"], "protected": True, "evaluate_against_base": True},
        ],
    }


def self_test() -> int:
    policy = fixture_policy()
    validate_policy(policy)
    docs = resolve(policy, ["docs/guide.md"])
    assert docs["required_checks"] == ["documentation"] and not docs["protected_change"]
    assert resolve(policy, ["docs\\guide.md"])["required_checks"] == docs["required_checks"]
    assert not resolve(policy, ["docs/architecture/authority.md"])["triggered_rules"]
    containment = resolve(policy, ["crates/containment/src/lib.rs"])
    assert containment["required_checks"] == ["documentation", "linux", "macos"]
    assert containment["conditional_checks"][0]["active"] is False
    conditioned = resolve(policy, ["crates/containment/src/lib.rs"], conditions={"windows-required"})
    assert conditioned["required_checks"] == ["linux", "macos", "windows"]
    assert conditioned["conditional_checks"][0]["active"] is True
    assert containment["platforms"] == ["linux", "macos", "windows"] and containment["protected_change"]
    generated = resolve(policy, ["source.json"])
    assert generated["source_of_truth_relationships"][0]["id"] == "generated-doc"
    assert generated["missing_evidence"] == ["documentation"]
    assert resolve(policy, ["source.json"], satisfied={"documentation"})["missing_evidence"] == []
    state = resolve(policy, ["state/new_authority.rs"])
    assert state["canonical_truth_stores"][0]["id"] == "state" and state["promotion"] == "protected-review"
    try:
        explain(policy, ["policy.json"], None)
    except ValueError as exc:
        assert "fail closed" in str(exc)
    else:
        raise AssertionError("policy mutation without base policy must fail closed")
    base = json.loads(json.dumps(policy)); base["policy_id"] = "fixture-base"
    report = explain(policy, ["policy.json"], base)
    assert report["evaluation_policy"] == "base" and report["required_checks"] == ["base-policy"]
    weakened = json.loads(json.dumps(policy)); weakened["protected_policy_paths"] = ["policy.json"]
    assert policy_paths_touched(weakened, ["checker.py"], base)
    broken = json.loads(json.dumps(policy)); broken["rules"][0]["required_checks"] = ["not-registered"]
    try:
        validate_policy(broken)
    except ValueError as exc:
        assert "unregistered" in str(exc)
    else:
        raise AssertionError("unregistered required check must fail closed")
    with tempfile.TemporaryDirectory() as temp:
        path = pathlib.Path(temp) / "policy.json"; path.write_text(json.dumps(policy), encoding="utf-8")
        assert load_policy(path)["policy_id"] == "fixture-v1"
        enforce(policy, {"required_checks": ["documentation", "base-policy"]}, pathlib.Path(temp), {"base-policy"}, None, None, None, 0)
    print("engineering policy self-test passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    validate_parser = sub.add_parser("validate")
    validate_parser.add_argument("--policy", type=pathlib.Path, required=True)
    validate_parser.add_argument("--root", type=pathlib.Path)
    explain_parser = sub.add_parser("explain")
    explain_parser.add_argument("--policy", type=pathlib.Path, required=True)
    explain_parser.add_argument("--base-policy", type=pathlib.Path)
    explain_parser.add_argument("--path", dest="paths", action="append", default=[])
    explain_parser.add_argument("--root", type=pathlib.Path, default=pathlib.Path("."))
    explain_parser.add_argument("--base")
    explain_parser.add_argument("--head", default="HEAD")
    explain_parser.add_argument("--task-type")
    explain_parser.add_argument("--satisfied", action="append", default=[])
    explain_parser.add_argument("--condition", action="append", default=[])
    explain_parser.add_argument("--json", action="store_true")
    enforce_parser = sub.add_parser("enforce")
    enforce_parser.add_argument("--policy", type=pathlib.Path, required=True)
    enforce_parser.add_argument("--report", type=pathlib.Path, required=True)
    enforce_parser.add_argument("--root", type=pathlib.Path, default=pathlib.Path("."))
    enforce_parser.add_argument("--satisfied", action="append", default=[])
    enforce_parser.add_argument("--repository", default=os.environ.get("GITHUB_REPOSITORY"))
    enforce_parser.add_argument("--head", default=os.environ.get("GITHUB_SHA"))
    enforce_parser.add_argument("--token-env", default="GITHUB_TOKEN")
    enforce_parser.add_argument("--timeout-seconds", type=int, default=600)
    sub.add_parser("self-test")
    args = parser.parse_args()
    try:
        if args.command == "self-test":
            return self_test()
        root = getattr(args, "root", None)
        root = root.resolve() if root is not None else None
        policy = load_policy(args.policy, root if args.command in {"validate", "enforce"} else None)
        if args.command == "validate":
            print(f"engineering policy valid: {policy['policy_id']}")
            return 0
        if args.command == "enforce":
            report = json.loads(args.report.read_text(encoding="utf-8"))
            token = os.environ.get(args.token_env)
            enforce(policy, report, root or pathlib.Path(".").resolve(), set(args.satisfied), args.repository, args.head, token, args.timeout_seconds)
            print("engineering policy required checks satisfied")
            return 0
        paths = list(args.paths)
        if args.base:
            paths.extend(changed_paths(args.root.resolve(), args.base, args.head))
        if not paths:
            raise ValueError("explain requires at least one --path or --base")
        base_policy = load_policy(args.base_policy) if args.base_policy else None
        report = explain(policy, paths, base_policy, args.task_type, set(args.satisfied), set(args.condition))
        print(json.dumps(report, indent=2, sort_keys=True) if args.json else render_human(report))
        return 0
    except (ValueError, OSError, json.JSONDecodeError) as exc:
        print(f"engineering policy error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())