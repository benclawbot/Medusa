#!/usr/bin/env python3
"""Reject production Rust exemptions for panic-prone Clippy lints."""

from __future__ import annotations

import argparse
import re
import subprocess
from pathlib import Path


FORBIDDEN = {
    "clippy::unwrap_used",
    "clippy::expect_used",
    "clippy::panic",
}
ALLOW_ATTRIBUTE = re.compile(r"#\s*!?\[\s*allow\s*\((?P<body>.*?)\)\s*\]", re.DOTALL)


class PanicExemptionPolicyError(RuntimeError):
    pass


def production_rust_path(path: str) -> bool:
    normalized = path.replace("\\", "/")
    return normalized.endswith(".rs") and (
        normalized.startswith("src/") or "/src/" in normalized
    )


def violations(files: dict[str, str]) -> list[str]:
    problems: list[str] = []
    for path, text in sorted(files.items()):
        if not production_rust_path(path):
            continue
        for match in ALLOW_ATTRIBUTE.finditer(text):
            body = match.group("body")
            found = sorted(lint for lint in FORBIDDEN if lint in body)
            if not found:
                continue
            line = text.count("\n", 0, match.start()) + 1
            problems.append(
                f"{path}:{line}: production panic-lint exemption: {', '.join(found)}"
            )
    return problems


def tracked_rust_files(root: Path) -> dict[str, str]:
    proc = subprocess.run(
        ["git", "ls-files", "-z", "*.rs"],
        cwd=root,
        check=True,
        capture_output=True,
    )
    files: dict[str, str] = {}
    for raw in proc.stdout.split(b"\0"):
        if not raw:
            continue
        path = raw.decode("utf-8")
        files[path] = (root / path).read_text(encoding="utf-8")
    return files


def check(root: Path) -> None:
    problems = violations(tracked_rust_files(root))
    if problems:
        rendered = "\n".join(f"- {problem}" for problem in problems)
        raise PanicExemptionPolicyError(
            "production panic-lint exemptions are forbidden:\n"
            f"{rendered}\n"
            "Propagate a typed error or use an infallible contract instead."
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    try:
        check(args.root.resolve())
    except (PanicExemptionPolicyError, subprocess.CalledProcessError, OSError) as exc:
        print(exc)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
