#!/usr/bin/env python3
"""Reject authoritative workflow steps that mutate Cargo.lock before validation."""

from __future__ import annotations

import argparse
from pathlib import Path

FORBIDDEN = ("cargo generate-lockfile", "cargo update")
WORKFLOW_DIR = Path(".github/workflows")


def violations(root: Path) -> list[str]:
    found: list[str] = []
    workflow_dir = root / WORKFLOW_DIR
    if not workflow_dir.exists():
        return [f"missing workflow directory: {WORKFLOW_DIR}"]
    for path in sorted(workflow_dir.glob("*.yml")):
        text = path.read_text(encoding="utf-8")
        for line_no, line in enumerate(text.splitlines(), start=1):
            stripped = line.strip()
            if stripped.startswith("#"):
                continue
            for command in FORBIDDEN:
                if command in stripped:
                    found.append(f"{path.relative_to(root)}:{line_no}: forbidden authoritative lockfile mutation: {command}")
    return found


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path("."))
    args = parser.parse_args()
    errors = violations(args.root.resolve())
    if errors:
        print("Cargo.lock authority violations:")
        for error in errors:
            print(f"- {error}")
        return 1
    print("Cargo.lock authority guard passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
