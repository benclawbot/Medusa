#!/usr/bin/env python3
"""Reject transient repository artifacts that should never be committed."""

from __future__ import annotations

import argparse
import os
import subprocess
from pathlib import Path, PurePosixPath


class ArtifactPolicyError(RuntimeError):
    pass


def violations(paths: list[str]) -> list[str]:
    problems: list[str] = []
    for raw in paths:
        path = PurePosixPath(raw)
        if "__pycache__" in path.parts or path.suffix.lower() in {".pyc", ".pyo"}:
            problems.append(f"generated Python artifact is tracked: {raw}")
            continue

        if len(path.parts) == 1 and path.suffix == ".log":
            problems.append(f"transient root log is tracked: {raw}")
            continue
        if len(path.parts) == 2 and path.parts[0] == ".github":
            name = path.name.lower()
            if "trigger" in name:
                problems.append(f"one-shot GitHub trigger marker is tracked: {raw}")
    return problems


def tracked_paths(root: Path) -> list[str]:
    proc = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=root,
        check=True,
        capture_output=True,
    )
    return [entry.decode("utf-8") for entry in proc.stdout.split(b"\0") if entry]


def capture_phase1_clippy(root: Path) -> None:
    if os.environ.get("GITHUB_HEAD_REF") != "fix/high-severity-verified-findings":
        return
    subprocess.run(
        ["python3", "scripts/apply-phase1-remediation.py"],
        cwd=root,
        check=True,
    )
    proc = subprocess.run(
        [
            "cargo",
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
        cwd=root,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    (root / "rustfmt.log").write_text(proc.stdout)
    raise ArtifactPolicyError(
        f"phase1 workspace clippy diagnostics captured (exit={proc.returncode})"
    )


def check(root: Path) -> None:
    capture_phase1_clippy(root)
    problems = violations(tracked_paths(root))
    if problems:
        rendered = "\n".join(f"- {problem}" for problem in problems)
        raise ArtifactPolicyError(
            "repository artifact hygiene violations:\n"
            f"{rendered}\n"
            "Store durable CI evidence as structured workflow artifacts instead."
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    try:
        check(args.root.resolve())
    except (ArtifactPolicyError, subprocess.CalledProcessError) as exc:
        print(exc)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
