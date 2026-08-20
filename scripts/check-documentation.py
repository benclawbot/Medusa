#!/usr/bin/env python3
"""Validate repository Markdown links and the reviewed documentation inventory."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import urllib.parse
from pathlib import Path


HISTORICAL_MARKER = "> Historical record —"
INLINE_LINK = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")
REFERENCE_LINK = re.compile(r"(?m)^\s*\[[^\]]+\]:\s*(\S+)")


class DocumentationError(RuntimeError):
    """Raised when current documentation and its reviewed inventory disagree."""


def is_governed_markdown(root: Path, path: Path) -> bool:
    """Return whether a Markdown file belongs to Medusa's reviewed documentation surface."""
    relative = path.relative_to(root)
    return not relative.parts or relative.parts[0] != "skills"


def markdown_paths(root: Path) -> list[Path]:
    result = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "--", "*.md"],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    )
    return [
        root / line
        for line in sorted(set(result.stdout.splitlines()))
        if line and (root / line).is_file() and is_governed_markdown(root, root / line)
    ]


def normalize_link_target(raw: str) -> str:
    target = raw.strip()
    if target.startswith("<") and ">" in target:
        target = target[1 : target.index(">")]
    elif " " in target:
        target = target.split(" ", 1)[0]
    return urllib.parse.unquote(target).split("#", 1)[0].split("?", 1)[0]


def validate_links(root: Path, paths: list[Path]) -> None:
    failures: list[str] = []
    for path in paths:
        text = path.read_text(encoding="utf-8")
        targets = INLINE_LINK.findall(text) + REFERENCE_LINK.findall(text)
        for raw in targets:
            target = normalize_link_target(raw)
            if not target or target.startswith(("http://", "https://", "mailto:", "#")):
                continue
            if re.match(r"^[A-Za-z][A-Za-z0-9+.-]*:", target):
                continue
            if any(character in target for character in ("{", "}", "*", "$")):
                continue
            candidate = (root / target.lstrip("/")) if target.startswith("/") else (path.parent / target)
            if not candidate.resolve().exists():
                failures.append(f"{path.relative_to(root)} -> {raw}")
    if failures:
        rendered = "\n".join(f"  {failure}" for failure in failures)
        raise DocumentationError(f"broken local Markdown links:\n{rendered}")


def canonical_document_bytes(path: Path) -> bytes:
    """Return UTF-8 content with platform line endings normalized to LF."""
    return path.read_text(encoding="utf-8").encode("utf-8")


def document_sha256(path: Path) -> str:
    return hashlib.sha256(canonical_document_bytes(path)).hexdigest()


def build_inventory(root: Path, paths: list[Path]) -> dict[str, object]:
    entries = []
    for path in paths:
        relative = path.relative_to(root).as_posix()
        data = canonical_document_bytes(path)
        text = data.decode("utf-8")
        entries.append(
            {
                "path": relative,
                "disposition": "historical" if HISTORICAL_MARKER in text else "current",
                "sha256": hashlib.sha256(data).hexdigest(),
            }
        )
    return {
        "schema_version": 1,
        "review_scope": "issue-771-final-documentation-reconciliation",
        "historical_marker": HISTORICAL_MARKER,
        "documents": entries,
    }


def validate_inventory(root: Path, expected: dict[str, object], write: bool) -> None:
    path = root / "docs/documentation-inventory.json"
    rendered = json.dumps(expected, indent=2, ensure_ascii=False) + "\n"
    if write:
        path.write_text(rendered, encoding="utf-8", newline="\n")
        return
    try:
        current = path.read_text(encoding="utf-8")
    except FileNotFoundError as error:
        raise DocumentationError("missing docs/documentation-inventory.json") from error
    if current != rendered:
        raise DocumentationError(
            "documentation inventory is stale; review the changed documents and run "
            "python scripts/check-documentation.py --write"
        )


def validate_governance(root: Path) -> None:
    index = (root / "docs/README.md").read_text(encoding="utf-8")
    for required in (
        "DOCUMENTATION-GOVERNANCE.md",
        "documentation-inventory.json",
        "provider-support.json",
        "architecture/INDEX.md",
    ):
        if required not in index:
            raise DocumentationError(f"docs/README.md must link to {required}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path("."))
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    root = args.root.resolve()
    try:
        paths = markdown_paths(root)
        expected = build_inventory(root, paths)
        if args.write:
            validate_inventory(root, expected, True)
        validate_links(root, paths)
        validate_governance(root)
        if not args.write:
            validate_inventory(root, expected, False)
    except (DocumentationError, FileNotFoundError, UnicodeDecodeError, subprocess.SubprocessError) as error:
        print(f"documentation-error: {error}", file=sys.stderr)
        return 1
    print(f"documentation-ok:{len(paths)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
