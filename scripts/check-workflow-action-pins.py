#!/usr/bin/env python3
"""Reject mutable external GitHub Actions references in repository workflows."""

from __future__ import annotations

import argparse
import re
from dataclasses import dataclass
from pathlib import Path

WORKFLOW_DIR = Path(".github/workflows")
ALLOWLIST_PATH = Path(".github/workflow-action-pin-allowlist.txt")
USES_RE = re.compile(r"^\s*(?:-\s*)?uses:\s*['\"]?([^'\"\s#]+)")
IMMUTABLE_REF_RE = re.compile(r"^[0-9a-fA-F]{40}$")


@dataclass(frozen=True)
class Violation:
    path: Path
    line: int
    reference: str
    reason: str

    def render(self) -> str:
        return f"{self.path}:{self.line}: {self.reference}: {self.reason}"


def load_allowlist(path: Path) -> set[str]:
    if not path.exists():
        return set()
    return {
        line.strip()
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    }


def is_external(reference: str) -> bool:
    return not (reference.startswith("./") or reference.startswith("docker://"))


def immutable_reference(reference: str) -> bool:
    if "@" not in reference:
        return False
    _, ref = reference.rsplit("@", 1)
    return bool(IMMUTABLE_REF_RE.fullmatch(ref))


def find_violations(root: Path, allowlist_path: Path | None = None) -> list[Violation]:
    root = root.resolve()
    workflows = root / WORKFLOW_DIR
    allowlist = load_allowlist(
        allowlist_path if allowlist_path is not None else root / ALLOWLIST_PATH
    )
    violations: list[Violation] = []

    if not workflows.exists():
        return violations

    paths = sorted((*workflows.glob("*.yml"), *workflows.glob("*.yaml")))
    for path in paths:
        for line_number, line in enumerate(
            path.read_text(encoding="utf-8").splitlines(), start=1
        ):
            match = USES_RE.match(line)
            if match is None:
                continue
            reference = match.group(1)
            if not is_external(reference) or reference in allowlist:
                continue
            if not immutable_reference(reference):
                violations.append(
                    Violation(
                        path=path.relative_to(root),
                        line=line_number,
                        reference=reference,
                        reason="external uses reference must be pinned to a full 40-hex commit SHA",
                    )
                )

    return violations


def check(root: Path, allowlist_path: Path | None = None) -> None:
    violations = find_violations(root, allowlist_path)
    if not violations:
        return
    rendered = "\n".join(f"- {violation.render()}" for violation in violations)
    raise RuntimeError(
        "mutable GitHub Actions references are forbidden:\n"
        f"{rendered}\n"
        f"Pin external actions/reusable workflows to immutable commit SHAs or explicitly allowlist an exact reference in {ALLOWLIST_PATH}."
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--allowlist", type=Path)
    args = parser.parse_args()

    try:
        check(args.root, args.allowlist)
    except RuntimeError as exc:
        print(exc)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
