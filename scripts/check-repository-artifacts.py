#!/usr/bin/env python3
"""Reject transient repository artifacts that should never be committed."""

from __future__ import annotations

import argparse
import subprocess
from pathlib import Path, PurePosixPath


class ArtifactPolicyError(RuntimeError):
    pass


def violations(paths: list[str]) -> list[str]:
    problems: list[str] = []
    for raw in paths:
        path = PurePosixPath(raw)
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


def check(root: Path) -> None:
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
