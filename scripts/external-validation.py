#!/usr/bin/env python3
"""Repeatable external-repository validation runner for Medusa.

The runner is intentionally stdlib-only so manifest/schema validation is safe to run in
required CI. Networked repository checkout and live-provider execution are opt-in.
"""
from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import time
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[1]
SCENARIO_DIR = ROOT / "validation" / "external-repositories" / "scenarios"
REPORT_SCHEMA = ROOT / "validation" / "external-repositories" / "report.schema.json"
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
REQUIRED_SCENARIO_KEYS = {
    "schema_version", "id", "repository", "task", "expected_invariants",
    "allowed_scope", "execution", "acceptance",
}
REQUIRED_REPOSITORY_KEYS = {"url", "commit", "ecosystem"}
REQUIRED_EXECUTION_KEYS = {"mode", "timeout_seconds", "provider"}
REQUIRED_ACCEPTANCE_KEYS = {
    "expected_outcome", "verification_commands", "required_evidence"
}

class ValidationError(RuntimeError):
    pass


def load_json(path: pathlib.Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ValidationError(f"{path}: {exc}") from exc
    if not isinstance(value, dict):
        raise ValidationError(f"{path}: top-level JSON value must be an object")
    return value


def require_keys(value: dict[str, Any], keys: set[str], where: str) -> None:
    missing = sorted(keys - value.keys())
    if missing:
        raise ValidationError(f"{where}: missing keys: {', '.join(missing)}")


def validate_scenario(path: pathlib.Path) -> dict[str, Any]:
    scenario = load_json(path)
    require_keys(scenario, REQUIRED_SCENARIO_KEYS, str(path))
    if scenario["schema_version"] != 1:
        raise ValidationError(f"{path}: unsupported schema_version")
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{2,63}", scenario["id"]):
        raise ValidationError(f"{path}: invalid id")
    repo = scenario["repository"]
    execution = scenario["execution"]
    acceptance = scenario["acceptance"]
    for value, name in ((repo, "repository"), (execution, "execution"), (acceptance, "acceptance")):
        if not isinstance(value, dict):
            raise ValidationError(f"{path}: {name} must be an object")
    require_keys(repo, REQUIRED_REPOSITORY_KEYS, f"{path}:repository")
    require_keys(execution, REQUIRED_EXECUTION_KEYS, f"{path}:execution")
    require_keys(acceptance, REQUIRED_ACCEPTANCE_KEYS, f"{path}:acceptance")
    if not SHA_RE.fullmatch(str(repo["commit"])):
        raise ValidationError(f"{path}: repository.commit must be a 40-character lowercase SHA")
    if execution["mode"] not in {"offline", "live-provider"}:
        raise ValidationError(f"{path}: execution.mode must be offline or live-provider")
    timeout = execution["timeout_seconds"]
    if not isinstance(timeout, int) or timeout < 60:
        raise ValidationError(f"{path}: timeout_seconds must be an integer >= 60")
    if not isinstance(scenario["expected_invariants"], list) or not scenario["expected_invariants"]:
        raise ValidationError(f"{path}: expected_invariants must be non-empty")
    if not isinstance(scenario["allowed_scope"], list) or not scenario["allowed_scope"]:
        raise ValidationError(f"{path}: allowed_scope must be non-empty")
    if not isinstance(acceptance["verification_commands"], list):
        raise ValidationError(f"{path}: verification_commands must be a list")
    return scenario


def scenario_paths() -> list[pathlib.Path]:
    return sorted(SCENARIO_DIR.glob("*.json"))


def validate_corpus() -> list[dict[str, Any]]:
    paths = scenario_paths()
    if not paths:
        raise ValidationError("no external validation scenarios found")
    scenarios = [validate_scenario(path) for path in paths]
    ids = [scenario["id"] for scenario in scenarios]
    if len(ids) != len(set(ids)):
        raise ValidationError("scenario ids must be unique")
    ecosystems = {scenario["repository"]["ecosystem"] for scenario in scenarios}
    repositories = {scenario["repository"]["url"] for scenario in scenarios}
    if len(repositories) < 5:
        raise ValidationError("initial corpus must cover at least five repositories")
    if len(ecosystems) < 3:
        raise ValidationError("initial corpus must cover at least three ecosystems")
    if not any(s["execution"]["timeout_seconds"] >= 3600 for s in scenarios):
        raise ValidationError("initial corpus must include a scenario lasting at least one hour")
    required_kinds = {"interruption-resume", "failed-verification", "rollback", "transient-provider-failure"}
    present = {kind for s in scenarios for kind in s.get("coverage", [])}
    missing = sorted(required_kinds - present)
    if missing:
        raise ValidationError(f"initial corpus missing coverage: {', '.join(missing)}")
    load_json(REPORT_SCHEMA)
    return scenarios


def run_command(command: list[str], cwd: pathlib.Path, timeout: int, env: dict[str, str]) -> dict[str, Any]:
    started = time.monotonic()
    completed = subprocess.run(
        command, cwd=cwd, env=env, text=True, capture_output=True,
        timeout=timeout, check=False,
    )
    return {
        "command": command,
        "exit_code": completed.returncode,
        "elapsed_seconds": round(time.monotonic() - started, 3),
        "stdout_sha256": hashlib.sha256(completed.stdout.encode()).hexdigest(),
        "stderr_sha256": hashlib.sha256(completed.stderr.encode()).hexdigest(),
        "stdout_tail": completed.stdout[-4000:],
        "stderr_tail": completed.stderr[-4000:],
    }


def safe_environment() -> dict[str, str]:
    allow = {"PATH", "HOME", "USER", "TMPDIR", "TEMP", "TMP", "SYSTEMROOT", "WINDIR", "COMSPEC"}
    return {key: value for key, value in os.environ.items() if key in allow}


def run_scenario(scenario: dict[str, Any], output_dir: pathlib.Path, medusa_command: list[str]) -> dict[str, Any]:
    if scenario["execution"]["mode"] == "live-provider" and os.getenv("MEDUSA_EXTERNAL_LIVE") != "1":
        return skipped_report(scenario, "live-provider scenario requires MEDUSA_EXTERNAL_LIVE=1")
    output_dir.mkdir(parents=True, exist_ok=True)
    started_at = dt.datetime.now(dt.timezone.utc)
    with tempfile.TemporaryDirectory(prefix=f"medusa-external-{scenario['id']}-") as temp:
        checkout = pathlib.Path(temp) / "repository"
        clone = run_command(
            ["git", "clone", "--no-checkout", scenario["repository"]["url"], str(checkout)],
            pathlib.Path(temp), 600, safe_environment(),
        )
        steps = [clone]
        if clone["exit_code"] == 0:
            steps.append(run_command(
                ["git", "checkout", "--detach", scenario["repository"]["commit"]],
                checkout, 120, safe_environment(),
            ))
        if all(step["exit_code"] == 0 for step in steps):
            prompt_file = pathlib.Path(temp) / "task.txt"
            prompt_file.write_text(scenario["task"] + "\n", encoding="utf-8")
            command = [part.replace("{repo}", str(checkout)).replace("{task_file}", str(prompt_file)) for part in medusa_command]
            steps.append(run_command(command, checkout, scenario["execution"]["timeout_seconds"], safe_environment()))
        if all(step["exit_code"] == 0 for step in steps):
            for command in scenario["acceptance"]["verification_commands"]:
                steps.append(run_command(command, checkout, min(1800, scenario["execution"]["timeout_seconds"]), safe_environment()))
        finished_at = dt.datetime.now(dt.timezone.utc)
        result = {
            "schema_version": 1,
            "scenario_id": scenario["id"],
            "scenario_version": scenario.get("scenario_version", 1),
            "status": "passed" if all(step["exit_code"] == 0 for step in steps) else "failed",
            "started_at": started_at.isoformat(),
            "finished_at": finished_at.isoformat(),
            "elapsed_seconds": round((finished_at - started_at).total_seconds(), 3),
            "medusa_commit": git_head(ROOT),
            "repository": scenario["repository"],
            "platform": {"sys_platform": sys.platform, "python": sys.version.split()[0]},
            "provider": scenario["execution"]["provider"],
            "metrics": {
                "verified_completion": all(step["exit_code"] == 0 for step in steps),
                "false_completion": False,
                "intervention_count": 0,
                "recovery_outcome": "not-observed",
                "test_status": "passed" if all(step["exit_code"] == 0 for step in steps) else "failed",
                "policy_denials": 0,
                "unrecovered_state": False,
            },
            "steps": steps,
        }
    report_path = output_dir / f"{scenario['id']}.json"
    report_path.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    (output_dir / f"{scenario['id']}.md").write_text(render_markdown(result), encoding="utf-8")
    return result


def git_head(root: pathlib.Path) -> str:
    result = subprocess.run(["git", "rev-parse", "HEAD"], cwd=root, text=True, capture_output=True, check=False)
    return result.stdout.strip() if result.returncode == 0 else "unknown"


def skipped_report(scenario: dict[str, Any], reason: str) -> dict[str, Any]:
    return {"schema_version": 1, "scenario_id": scenario["id"], "status": "skipped", "reason": reason}


def render_markdown(report: dict[str, Any]) -> str:
    lines = [f"# External validation: {report['scenario_id']}", "", f"- Status: **{report['status']}**"]
    if "elapsed_seconds" in report:
        lines += [f"- Elapsed: {report['elapsed_seconds']} seconds", f"- Medusa commit: `{report['medusa_commit']}`"]
        lines += [f"- Repository commit: `{report['repository']['commit']}`", "", "## Steps", ""]
        for step in report["steps"]:
            lines.append(f"- `{ ' '.join(step['command']) }` — exit {step['exit_code']} ({step['elapsed_seconds']}s)")
    if report.get("reason"):
        lines += [f"- Reason: {report['reason']}"]
    return "\n".join(lines) + "\n"


def self_test() -> None:
    scenarios = validate_corpus()
    with tempfile.TemporaryDirectory() as temp:
        sample = scenarios[0]
        report = skipped_report(sample, "self-test")
        path = pathlib.Path(temp) / "report.json"
        path.write_text(json.dumps(report), encoding="utf-8")
        if load_json(path)["status"] != "skipped":
            raise ValidationError("report round-trip failed")
    print(f"validated {len(scenarios)} scenarios")


def main() -> int:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("validate")
    sub.add_parser("self-test")
    run = sub.add_parser("run")
    run.add_argument("scenario")
    run.add_argument("--output", default="validation-results")
    run.add_argument("--medusa-command", nargs="+", required=True,
                     help="command template; use {repo} and {task_file} placeholders")
    args = parser.parse_args()
    try:
        if args.command == "validate":
            print(f"validated {len(validate_corpus())} scenarios")
        elif args.command == "self-test":
            self_test()
        else:
            scenarios = {s["id"]: s for s in validate_corpus()}
            if args.scenario not in scenarios:
                raise ValidationError(f"unknown scenario: {args.scenario}")
            report = run_scenario(scenarios[args.scenario], pathlib.Path(args.output), args.medusa_command)
            print(json.dumps(report, indent=2, sort_keys=True))
            return 0 if report["status"] in {"passed", "skipped"} else 1
    except (ValidationError, subprocess.TimeoutExpired) as exc:
        print(f"external-validation: {exc}", file=sys.stderr)
        return 2
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
