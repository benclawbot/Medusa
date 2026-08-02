#!/usr/bin/env python3
"""Drive production CLI entrypoints and preserve removable v1 known-failure fixtures."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Callable


@dataclass(frozen=True)
class Result:
    id: str
    kind: str
    status: str
    evidence: str

    @property
    def passed(self) -> bool:
        return self.status in {"passed", "xfail-reproduced"}


def read_tree(root: Path, relative: str) -> str:
    base = root / relative
    if base.is_file():
        return base.read_text(encoding="utf-8")
    if not base.exists():
        return ""
    chunks: list[str] = []
    for path in sorted(base.rglob("*.rs")):
        chunks.append(f"\n// {path.relative_to(root)}\n")
        chunks.append(path.read_text(encoding="utf-8"))
    return "".join(chunks)


def function_bodies(text: str, function_name: str) -> list[str]:
    marker = f"fn {function_name}"
    bodies: list[str] = []
    cursor = 0
    while True:
        start = text.find(marker, cursor)
        if start < 0:
            break
        brace = text.find("{", start)
        if brace < 0:
            break
        depth = 0
        end = brace
        for end in range(brace, len(text)):
            if text[end] == "{":
                depth += 1
            elif text[end] == "}":
                depth -= 1
                if depth == 0:
                    bodies.append(text[brace : end + 1])
                    break
        cursor = max(end + 1, start + len(marker))
    return bodies


def integration_precedes_parent_review(root: Path) -> tuple[bool, str]:
    runtime = read_tree(root, "crates/medusa-runtime/src/lib.rs")
    integration = runtime.find("mutating_worker_coordinator::run_implementation")
    parent_execution = runtime.find("engine.step_with_observer_and_context", integration + 1)
    observed = integration >= 0 and parent_execution > integration
    return (
        observed,
        f"run_implementation_offset={integration}; parent_execution_offset={parent_execution}",
    )


def verification_drops_changed_paths(root: Path) -> tuple[bool, str]:
    coordinator = read_tree(root, "crates/medusa-runtime/src/mutating_worker_coordinator.rs")
    signature = "targeted_verification(&worker.worktree)"
    observed = signature in coordinator
    return observed, f"legacy_signature_present={observed}: {signature}"


def provider_capability_mismatch(root: Path) -> tuple[bool, str]:
    provider = read_tree(root, "crates/medusa-provider/src")
    markers = {
        "config_can_claim_streaming": "capabilities.streaming = config.model.streaming" in provider,
        "wire_forces_non_streaming": '"stream": false' in provider,
        "request_runs_on_detached_thread": 'name("medusa-provider-request"' in provider,
        "cancellation_returns_from_poll_loop": "if cancel.load(Ordering::SeqCst)" in provider
        and "recv_timeout" in provider,
    }
    observed = all(markers.values())
    return observed, json.dumps(markers, sort_keys=True)


PROBES: dict[str, Callable[[Path], tuple[bool, str]]] = {
    "integration-precedes-parent-review": integration_precedes_parent_review,
    "isolated-verification-drops-changed-paths": verification_drops_changed_paths,
    "provider-capability-mismatch": provider_capability_mismatch,
}


def load_expected_fixture_ids(root: Path) -> set[str]:
    path = root / "docs/architecture/baseline.json"
    payload = json.loads(path.read_text(encoding="utf-8"))
    fixtures = payload.get("known_failure_fixtures", [])
    expected: set[str] = set()
    for row in fixtures:
        if not isinstance(row, list) or len(row) != 5:
            raise RuntimeError(f"invalid known-failure fixture row: {row!r}")
        fixture_id, _issue, desired, _probe, _remove_when = row
        if desired is not False:
            raise RuntimeError(f"known failure must set desired=false: {fixture_id}")
        expected.add(fixture_id)
    return expected


def run_known_failures(root: Path) -> list[Result]:
    expected = load_expected_fixture_ids(root)
    missing_probe = sorted(expected - set(PROBES))
    stale_probe = sorted(set(PROBES) - expected)
    if missing_probe or stale_probe:
        return [
            Result(
                id="fixture-registry",
                kind="governance",
                status="failed",
                evidence=f"missing_probe={missing_probe}; stale_probe={stale_probe}",
            )
        ]
    results: list[Result] = []
    for fixture_id in sorted(expected):
        observed, evidence = PROBES[fixture_id](root)
        results.append(
            Result(
                id=fixture_id,
                kind="known-failure",
                status="xfail-reproduced" if observed else "unexpected-pass",
                evidence=evidence,
            )
        )
    return results


def run_command(command: list[str], timeout: int) -> tuple[bool, str]:
    try:
        completed = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return False, str(exc)
    output = (completed.stdout + completed.stderr).strip().replace("\n", " | ")
    if len(output) > 500:
        output = output[:497] + "..."
    return completed.returncode == 0, f"exit={completed.returncode}; output={output}"


def run_production_smoke(binary: Path, timeout: int) -> list[Result]:
    commands = {
        "entrypoint-cli": [str(binary), "--help"],
        "entrypoint-headless": [str(binary), "run", "--help"],
        "entrypoint-daemon": [str(binary), "__daemon-serve", "--help"],
        "entrypoint-update": [str(binary), "update", "--help"],
    }
    results: list[Result] = []
    for fixture_id, command in commands.items():
        passed, evidence = run_command(command, timeout)
        results.append(
            Result(
                id=fixture_id,
                kind="production-entrypoint",
                status="passed" if passed else "failed",
                evidence=evidence,
            )
        )
    return results


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path("."))
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--known-failures", action="store_true")
    parser.add_argument("--all", action="store_true")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--timeout", type=int, default=20)
    args = parser.parse_args()

    root = args.root.resolve()
    run_failures = args.known_failures or args.all or args.binary is None
    results: list[Result] = []
    if run_failures:
        results.extend(run_known_failures(root))
    if args.binary is not None:
        binary = args.binary if args.binary.is_absolute() else root / args.binary
        results.extend(run_production_smoke(binary.resolve(), args.timeout))
    elif args.all:
        results.append(
            Result(
                id="production-entrypoints",
                kind="configuration",
                status="failed",
                evidence="--all requires --binary so real entrypoints are exercised",
            )
        )

    payload = {
        "schema_version": 1,
        "passed": all(result.passed for result in results),
        "results": [asdict(result) for result in results],
    }
    if args.json:
        print(json.dumps(payload, indent=2, sort_keys=True))
    else:
        for result in results:
            print(f"{result.status}: {result.id}: {result.evidence}")
    return 0 if payload["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
