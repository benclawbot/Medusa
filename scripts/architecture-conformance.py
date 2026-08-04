#!/usr/bin/env python3
"""Drive production CLI entrypoints and preserve removable v1 known-failure fixtures."""

from __future__ import annotations

import argparse
import json
import subprocess
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


PROBES: dict[str, Callable[[Path], tuple[bool, str]]] = {}


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
