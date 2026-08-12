#!/usr/bin/env python3
"""Repeatable external-repository validation runner for Medusa.

Required CI only validates the corpus and runner contracts. Network checkout and
provider execution are explicit opt-in operations.
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import pathlib
import re
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
    if not isinstance(acceptance["required_evidence"], list) or not acceptance["required_evidence"]:
        raise ValidationError(f"{path}: required_evidence must be non-empty")
    provider = execution["provider"]
    if not isinstance(provider, dict) or not provider.get("kind"):
        raise ValidationError(f"{path}: execution.provider.kind is required")
    if execution["mode"] == "offline" and not provider.get("fixture"):
        raise ValidationError(f"{path}: offline scenarios require execution.provider.fixture")
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
    present = {kind for scenario in scenarios for kind in scenario.get("coverage", [])}
    missing = sorted(required_kinds - present)
    if missing:
        raise ValidationError(f"initial corpus missing coverage: {', '.join(missing)}")
    load_json(REPORT_SCHEMA)
    return scenarios


def run_command(command: list[str], cwd: pathlib.Path, timeout: int, env: dict[str, str]) -> dict[str, Any]:
    started = time.monotonic()
    try:
        completed = subprocess.run(
            command, cwd=cwd, env=env, text=True, capture_output=True,
            timeout=timeout, check=False,
        )
        return {
            "command": command,
            "exit_code": completed.returncode,
            "timed_out": False,
            "elapsed_seconds": round(time.monotonic() - started, 3),
            "stdout_sha256": hashlib.sha256(completed.stdout.encode()).hexdigest(),
            "stderr_sha256": hashlib.sha256(completed.stderr.encode()).hexdigest(),
            "stdout_tail": completed.stdout[-4000:],
            "stderr_tail": completed.stderr[-4000:],
        }
    except subprocess.TimeoutExpired as exc:
        stdout = exc.stdout if isinstance(exc.stdout, str) else ""
        stderr = exc.stderr if isinstance(exc.stderr, str) else ""
        return {
            "command": command,
            "exit_code": 124,
            "timed_out": True,
            "elapsed_seconds": round(time.monotonic() - started, 3),
            "stdout_sha256": hashlib.sha256(stdout.encode()).hexdigest(),
            "stderr_sha256": hashlib.sha256(stderr.encode()).hexdigest(),
            "stdout_tail": stdout[-4000:],
            "stderr_tail": stderr[-4000:] or f"timed out after {timeout} seconds",
        }


def safe_environment() -> dict[str, str]:
    allow = {"PATH", "HOME", "USER", "TMPDIR", "TEMP", "TMP", "SYSTEMROOT", "WINDIR", "COMSPEC"}
    return {key: value for key, value in os.environ.items() if key in allow}


def command_template(
    template: list[str], scenario: dict[str, Any], checkout: pathlib.Path,
    task_file: pathlib.Path, evidence_file: pathlib.Path,
) -> list[str]:
    provider = scenario["execution"]["provider"]
    replacements = {
        "{repo}": str(checkout),
        "{task_file}": str(task_file),
        "{task}": scenario["task"],
        "{provider_fixture}": str(provider.get("fixture", "")),
        "{evidence_file}": str(evidence_file),
    }
    rendered = []
    for part in template:
        for placeholder, value in replacements.items():
            part = part.replace(placeholder, value)
        rendered.append(part)
    return rendered


def require_execution_contract(scenario: dict[str, Any], template: list[str]) -> None:
    joined = "\n".join(template)
    if "{task}" not in joined and "{task_file}" not in joined:
        raise ValidationError("--medusa-command must include {task} or {task_file}")
    if "{evidence_file}" not in joined:
        raise ValidationError("--medusa-command must include {evidence_file}")
    if scenario["execution"]["mode"] == "offline" and "{provider_fixture}" not in joined:
        raise ValidationError(
            "offline execution refused: --medusa-command must pass {provider_fixture} to a replay adapter"
        )


def changed_paths(checkout: pathlib.Path) -> list[str]:
    step = run_command(
        ["git", "status", "--porcelain=v1", "--untracked-files=all"],
        checkout, 60, safe_environment(),
    )
    if step["exit_code"] != 0:
        return []
    paths = []
    for line in step["stdout_tail"].splitlines():
        path = line[3:].strip()
        if " -> " in path:
            path = path.split(" -> ", 1)[1]
        if path:
            paths.append(path.replace("\\", "/"))
    return sorted(set(paths))


def path_allowed(path: str, patterns: list[str]) -> bool:
    candidate = pathlib.PurePosixPath(path)
    return any(candidate.match(pattern) for pattern in patterns)


def validate_evidence(
    scenario: dict[str, Any], evidence_file: pathlib.Path, paths: list[str], verification_ok: bool,
) -> tuple[bool, dict[str, Any], list[str]]:
    errors: list[str] = []
    if not evidence_file.is_file():
        return False, {}, ["execution did not write the required evidence file"]
    try:
        evidence = load_json(evidence_file)
    except ValidationError as exc:
        return False, {}, [str(exc)]
    observed = evidence.get("evidence", [])
    if not isinstance(observed, list):
        errors.append("evidence.evidence must be a list")
        observed = []
    required = set(scenario["acceptance"]["required_evidence"])
    missing = sorted(required - set(str(item) for item in observed))
    if missing:
        errors.append(f"missing required evidence: {', '.join(missing)}")
    if not paths:
        errors.append("repository contains no recorded changes")
    out_of_scope = [path for path in paths if not path_allowed(path, scenario["allowed_scope"])]
    if out_of_scope:
        errors.append(f"changes outside allowed_scope: {', '.join(out_of_scope)}")
    if not verification_ok:
        errors.append("verification commands did not all pass")
    if evidence.get("outcome") != scenario["acceptance"]["expected_outcome"]:
        errors.append(
            "evidence outcome does not match acceptance.expected_outcome: "
            f"{evidence.get('outcome')!r}"
        )
    return not errors, evidence, errors


def write_report(output_dir: pathlib.Path, result: dict[str, Any]) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    scenario_id = result["scenario_id"]
    (output_dir / f"{scenario_id}.json").write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    (output_dir / f"{scenario_id}.md").write_text(render_markdown(result), encoding="utf-8")


def run_scenario(scenario: dict[str, Any], output_dir: pathlib.Path, medusa_command: list[str]) -> dict[str, Any]:
    if scenario["execution"]["mode"] == "live-provider" and os.getenv("MEDUSA_EXTERNAL_LIVE") != "1":
        result = skipped_report(scenario, "live-provider scenario requires MEDUSA_EXTERNAL_LIVE=1")
        write_report(output_dir, result)
        return result
    require_execution_contract(scenario, medusa_command)
    started_at = dt.datetime.now(dt.timezone.utc)
    steps: list[dict[str, Any]] = []
    evidence: dict[str, Any] = {}
    evidence_errors: list[str] = []
    paths: list[str] = []
    with tempfile.TemporaryDirectory(prefix=f"medusa-external-{scenario['id']}-") as temp:
        temp_path = pathlib.Path(temp)
        checkout = temp_path / "repository"
        task_file = temp_path / "task.txt"
        evidence_file = temp_path / "execution-evidence.json"
        clone = run_command(
            ["git", "clone", "--no-checkout", scenario["repository"]["url"], str(checkout)],
            temp_path, 600, safe_environment(),
        )
        steps.append(clone)
        if clone["exit_code"] == 0:
            steps.append(run_command(
                ["git", "checkout", "--detach", scenario["repository"]["commit"]],
                checkout, 120, safe_environment(),
            ))
        if all(step["exit_code"] == 0 for step in steps):
            task_file.write_text(scenario["task"] + "\n", encoding="utf-8")
            command = command_template(medusa_command, scenario, checkout, task_file, evidence_file)
            env = safe_environment()
            env["MEDUSA_EXTERNAL_SCENARIO_ID"] = scenario["id"]
            env["MEDUSA_EXTERNAL_EVIDENCE_FILE"] = str(evidence_file)
            env["MEDUSA_PROVIDER_REPLAY_FIXTURE"] = str(
                scenario["execution"]["provider"].get("fixture", "")
            )
            steps.append(run_command(command, checkout, scenario["execution"]["timeout_seconds"], env))
        execution_ok = all(step["exit_code"] == 0 for step in steps)
        verification_steps = []
        if execution_ok:
            for command in scenario["acceptance"]["verification_commands"]:
                step = run_command(
                    command, checkout,
                    min(1800, scenario["execution"]["timeout_seconds"]),
                    safe_environment(),
                )
                verification_steps.append(step)
                steps.append(step)
        verification_ok = bool(verification_steps) and all(
            step["exit_code"] == 0 for step in verification_steps
        )
        if checkout.exists():
            paths = changed_paths(checkout)
        evidence_ok, evidence, evidence_errors = validate_evidence(
            scenario, evidence_file, paths, verification_ok
        )
        finished_at = dt.datetime.now(dt.timezone.utc)
        passed = execution_ok and evidence_ok
        metrics = evidence.get("metrics", {}) if isinstance(evidence.get("metrics"), dict) else {}
        result = {
            "schema_version": 1,
            "scenario_id": scenario["id"],
            "scenario_version": scenario.get("scenario_version", 1),
            "status": "passed" if passed else "failed",
            "started_at": started_at.isoformat(),
            "finished_at": finished_at.isoformat(),
            "elapsed_seconds": round((finished_at - started_at).total_seconds(), 3),
            "medusa_commit": git_head(ROOT),
            "repository": scenario["repository"],
            "platform": {"sys_platform": sys.platform, "python": sys.version.split()[0]},
            "provider": scenario["execution"]["provider"],
            "metrics": {
                "verified_completion": passed,
                "false_completion": bool(evidence.get("completion_claimed")) and not passed,
                "intervention_count": int(metrics.get("intervention_count", 0)),
                "recovery_outcome": str(metrics.get("recovery_outcome", "not-observed")),
                "test_status": "passed" if verification_ok else "failed",
                "policy_denials": int(metrics.get("policy_denials", 0)),
                "unrecovered_state": bool(metrics.get("unrecovered_state", not passed)),
            },
            "changed_paths": paths,
            "evidence_errors": evidence_errors,
            "steps": steps,
        }
    write_report(output_dir, result)
    return result


def git_head(root: pathlib.Path) -> str:
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=root,
        text=True, capture_output=True, check=False,
    )
    return result.stdout.strip() if result.returncode == 0 else "unknown"


def skipped_report(scenario: dict[str, Any], reason: str) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "scenario_id": scenario["id"],
        "scenario_version": scenario.get("scenario_version", 1),
        "status": "skipped",
        "reason": reason,
    }


def render_markdown(report: dict[str, Any]) -> str:
    lines = [f"# External validation: {report['scenario_id']}", "", f"- Status: **{report['status']}**"]
    if "elapsed_seconds" in report:
        lines += [
            f"- Elapsed: {report['elapsed_seconds']} seconds",
            f"- Medusa commit: `{report['medusa_commit']}`",
            f"- Repository commit: `{report['repository']['commit']}`",
            "",
            "## Steps",
            "",
        ]
        for step in report["steps"]:
            suffix = " (timed out)" if step.get("timed_out") else ""
            lines.append(
                f"- `{' '.join(step['command'])}` — exit {step['exit_code']} "
                f"({step['elapsed_seconds']}s){suffix}"
            )
        if report.get("evidence_errors"):
            lines += ["", "## Evidence failures", ""]
            lines.extend(f"- {error}" for error in report["evidence_errors"])
    if report.get("reason"):
        lines += [f"- Reason: {report['reason']}"]
    return "\n".join(lines) + "\n"


def self_test() -> None:
    scenarios = validate_corpus()
    offline = next(scenario for scenario in scenarios if scenario["execution"]["mode"] == "offline")
    try:
        require_execution_contract(offline, ["medusa", "{task}", "{evidence_file}"])
    except ValidationError:
        pass
    else:
        raise ValidationError("offline execution accepted without {provider_fixture}")
    with tempfile.TemporaryDirectory() as temp:
        timeout_step = run_command(
            [sys.executable, "-c", "import time; time.sleep(1)"],
            pathlib.Path(temp), 0.01, safe_environment(),
        )
        if timeout_step["exit_code"] != 124 or not timeout_step["timed_out"]:
            raise ValidationError("timeout was not converted to a failed step")
        report = skipped_report(scenarios[0], "self-test")
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
    run.add_argument(
        "--medusa-command", nargs="+", required=True,
        help=(
            "execution adapter template; requires {task} or {task_file}, {evidence_file}, "
            "and {provider_fixture} for offline scenarios"
        ),
    )
    args = parser.parse_args()
    try:
        if args.command == "validate":
            print(f"validated {len(validate_corpus())} scenarios")
        elif args.command == "self-test":
            self_test()
        else:
            scenarios = {scenario["id"]: scenario for scenario in validate_corpus()}
            if args.scenario not in scenarios:
                raise ValidationError(f"unknown scenario: {args.scenario}")
            report = run_scenario(
                scenarios[args.scenario], pathlib.Path(args.output), args.medusa_command
            )
            print(json.dumps(report, indent=2, sort_keys=True))
            return 0 if report["status"] in {"passed", "skipped"} else 1
    except ValidationError as exc:
        print(f"external-validation: {exc}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
