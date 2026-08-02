#!/usr/bin/env python3
"""Enforce Medusa's explicit unsafe-Rust and Windows FFI boundary."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any

import tomllib

POLICY_PATH = Path("docs/architecture/unsafe-rust-policy.json")
CONTAINMENT_CRATE = Path("crates/medusa-process-containment")
VALID_CLASSIFICATIONS = {"safe", "unsafe-ffi"}
UNSAFE_PATTERNS = (
    re.compile(r"\bunsafe\s*\{"),
    re.compile(r"\bunsafe\s+(?:extern\s+(?:r?\"[^\"]+\"\s+)?)?fn\b"),
    re.compile(r"\bunsafe\s+impl\b"),
    re.compile(r"\bunsafe\s+trait\b"),
    re.compile(r"\bunsafe\s+extern\b"),
)
ALLOW_ATTRIBUTE = re.compile(r"#\s*!?\s*\[\s*allow\s*\(\s*unsafe_code\s*\)\s*\]")


class UnsafeBoundaryError(RuntimeError):
    """Raised when the checked unsafe-Rust boundary drifts."""


def read_text(root: Path, relative: Path | str) -> str:
    path = root / relative
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError as exc:
        raise UnsafeBoundaryError(f"missing required path: {relative}") from exc


def load_toml(root: Path, relative: Path | str) -> dict[str, Any]:
    try:
        value = tomllib.loads(read_text(root, relative))
    except tomllib.TOMLDecodeError as exc:
        raise UnsafeBoundaryError(f"invalid TOML in {relative}: {exc}") from exc
    if not isinstance(value, dict):
        raise UnsafeBoundaryError(f"{relative} must contain a TOML table")
    return value


def load_policy(root: Path) -> dict[str, Any]:
    try:
        value = json.loads(read_text(root, POLICY_PATH))
    except json.JSONDecodeError as exc:
        raise UnsafeBoundaryError(f"invalid JSON in {POLICY_PATH}: {exc}") from exc
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise UnsafeBoundaryError("unsafe policy must be a schema_version 1 object")
    return value


def lint_level(manifest: dict[str, Any]) -> str | None:
    lints = manifest.get("lints")
    if not isinstance(lints, dict):
        return None
    rust = lints.get("rust")
    if not isinstance(rust, dict):
        return None
    value = rust.get("unsafe_code")
    return value if isinstance(value, str) else None


def validate_workspace_lints(root: Path) -> None:
    workspace = load_toml(root, "Cargo.toml")
    root_level = (
        workspace.get("workspace", {})
        .get("lints", {})
        .get("rust", {})
        .get("unsafe_code")
    )
    if root_level != "forbid":
        raise UnsafeBoundaryError(
            'workspace.lints.rust.unsafe_code must remain "forbid"'
        )
    members = workspace.get("workspace", {}).get("members")
    if not isinstance(members, list) or not members:
        raise UnsafeBoundaryError("workspace.members must be a non-empty list")
    for member in members:
        if not isinstance(member, str):
            raise UnsafeBoundaryError(f"workspace member is not a string: {member!r}")
        manifest_path = Path(member) / "Cargo.toml"
        manifest = load_toml(root, manifest_path)
        lints = manifest.get("lints")
        if member == CONTAINMENT_CRATE.as_posix():
            if lint_level(manifest) != "deny":
                raise UnsafeBoundaryError(
                    f'{manifest_path} must set [lints.rust] unsafe_code = "deny"'
                )
            continue
        inherits = isinstance(lints, dict) and lints.get("workspace") is True
        explicit = lint_level(manifest) in {"deny", "forbid"}
        if not (inherits or explicit):
            raise UnsafeBoundaryError(
                f"{manifest_path} must inherit workspace lints or explicitly deny unsafe_code"
            )


def strip_comments_and_literals(text: str) -> str:
    """Replace comments and Rust string/character literals with whitespace."""

    chars = list(text)
    index = 0
    block_depth = 0
    length = len(chars)
    while index < length:
        if block_depth:
            if index + 1 < length and chars[index] == "/" and chars[index + 1] == "*":
                chars[index] = chars[index + 1] = " "
                block_depth += 1
                index += 2
            elif index + 1 < length and chars[index] == "*" and chars[index + 1] == "/":
                chars[index] = chars[index + 1] = " "
                block_depth -= 1
                index += 2
            else:
                if chars[index] != "\n":
                    chars[index] = " "
                index += 1
            continue

        if index + 1 < length and chars[index] == "/" and chars[index + 1] == "/":
            chars[index] = chars[index + 1] = " "
            index += 2
            while index < length and chars[index] != "\n":
                chars[index] = " "
                index += 1
            continue
        if index + 1 < length and chars[index] == "/" and chars[index + 1] == "*":
            chars[index] = chars[index + 1] = " "
            block_depth = 1
            index += 2
            continue

        raw_start = None
        if chars[index] == "r":
            cursor = index + 1
            while cursor < length and chars[cursor] == "#":
                cursor += 1
            if cursor < length and chars[cursor] == '"':
                raw_start = (cursor, cursor - index - 1)
        if raw_start is not None:
            quote, hashes = raw_start
            for cursor in range(index, quote + 1):
                chars[cursor] = " "
            index = quote + 1
            terminator = '"' + "#" * hashes
            while index < length:
                if text.startswith(terminator, index):
                    for cursor in range(index, min(length, index + len(terminator))):
                        chars[cursor] = " "
                    index += len(terminator)
                    break
                if chars[index] != "\n":
                    chars[index] = " "
                index += 1
            continue

        if chars[index] == '"':
            chars[index] = " "
            index += 1
            escaped = False
            while index < length:
                current = chars[index]
                if current != "\n":
                    chars[index] = " "
                index += 1
                if escaped:
                    escaped = False
                elif current == "\\":
                    escaped = True
                elif current == '"':
                    break
            continue

        # Treat a quote as a character literal only when a closing quote is nearby.
        if chars[index] == "'":
            cursor = index + 1
            escaped = False
            while cursor < min(length, index + 8) and chars[cursor] != "\n":
                current = chars[cursor]
                if escaped:
                    escaped = False
                elif current == "\\":
                    escaped = True
                elif current == "'":
                    for position in range(index, cursor + 1):
                        chars[position] = " "
                    index = cursor + 1
                    break
                cursor += 1
            else:
                index += 1
            continue

        index += 1

    if block_depth:
        raise UnsafeBoundaryError("unterminated block comment while scanning Rust source")
    return "".join(chars)


def unsafe_occurrences(text: str) -> list[int]:
    scrubbed = strip_comments_and_literals(text)
    lines: set[int] = set()
    for pattern in UNSAFE_PATTERNS:
        for match in pattern.finditer(scrubbed):
            lines.add(scrubbed.count("\n", 0, match.start()) + 1)
    return sorted(lines)


def rust_sources(root: Path) -> set[str]:
    excluded = {".git", "target", "node_modules"}
    sources: set[str] = set()
    for path in root.rglob("*.rs"):
        relative = path.relative_to(root)
        if any(part in excluded for part in relative.parts):
            continue
        sources.add(relative.as_posix())
    return sources


def validate_policy_inventory(root: Path, policy: dict[str, Any]) -> set[str]:
    boundary = policy.get("containment_boundary")
    if not isinstance(boundary, dict):
        raise UnsafeBoundaryError("policy containment_boundary must be an object")
    if boundary.get("crate") != CONTAINMENT_CRATE.as_posix():
        raise UnsafeBoundaryError("policy containment crate path is incorrect")
    files = boundary.get("files")
    if not isinstance(files, list) or not files:
        raise UnsafeBoundaryError("policy containment files must be a non-empty list")

    classified: dict[str, str] = {}
    unsafe_modules: set[str] = set()
    for row in files:
        if not isinstance(row, dict):
            raise UnsafeBoundaryError(f"invalid policy file entry: {row!r}")
        path = row.get("path")
        module = row.get("module")
        classification = row.get("classification")
        if not all(isinstance(value, str) and value for value in (path, module, classification)):
            raise UnsafeBoundaryError(f"incomplete policy file entry: {row!r}")
        if classification not in VALID_CLASSIFICATIONS:
            raise UnsafeBoundaryError(f"invalid policy classification: {classification}")
        if path in classified:
            raise UnsafeBoundaryError(f"duplicate policy path: {path}")
        if not (root / path).is_file():
            raise UnsafeBoundaryError(f"policy path does not exist: {path}")
        classified[path] = classification
        if classification == "unsafe-ffi":
            reason = row.get("reason")
            if not isinstance(reason, str) or not reason.strip():
                raise UnsafeBoundaryError(f"unsafe module lacks review reason: {path}")
            unsafe_modules.add(module)

    actual = {
        path.relative_to(root).as_posix()
        for path in (root / CONTAINMENT_CRATE / "src").rglob("*.rs")
    }
    if actual != set(classified):
        raise UnsafeBoundaryError(
            "containment source inventory drift; "
            f"unclassified={sorted(actual - set(classified))}, "
            f"missing={sorted(set(classified) - actual)}"
        )

    lib = read_text(root, CONTAINMENT_CRATE / "src/lib.rs")
    for row in files:
        module = row["module"]
        classification = row["classification"]
        if module == "crate-root":
            continue
        declaration = re.compile(
            rf"(?m)(?:#\[[^\n]+\]\s*\n|//[^\n]*\n|\s)*"
            rf"#\[\s*allow\s*\(\s*unsafe_code\s*\)\s*\]\s*\n"
            rf"\s*mod\s+{re.escape(module)}\s*;"
        )
        has_exception = declaration.search(lib) is not None
        if classification == "unsafe-ffi" and not has_exception:
            raise UnsafeBoundaryError(
                f"unsafe module lacks a local #[allow(unsafe_code)] declaration: {module}"
            )
        if classification == "safe" and has_exception:
            raise UnsafeBoundaryError(
                f"safe module has an unsafe-code exception: {module}"
            )
    exception_count = len(ALLOW_ATTRIBUTE.findall(lib))
    if exception_count != len(unsafe_modules):
        raise UnsafeBoundaryError(
            "containment lib.rs must declare exactly one local unsafe-code "
            f"exception for each reviewed unsafe module; expected={len(unsafe_modules)}, "
            f"actual={exception_count}"
        )
    return {path for path, classification in classified.items() if classification == "unsafe-ffi"}


def validate_unsafe_locations(root: Path, allowed_paths: set[str]) -> None:
    violations: list[str] = []
    containment_root = (CONTAINMENT_CRATE / "src/lib.rs").as_posix()
    for relative in sorted(rust_sources(root)):
        text = read_text(root, relative)
        lines = unsafe_occurrences(text)
        if lines and relative not in allowed_paths:
            violations.append(
                f"unsafe Rust outside reviewed allowlist: {relative}:{','.join(map(str, lines))}"
            )
        if not lines and relative in allowed_paths:
            violations.append(f"stale unsafe allowlist entry: {relative}")
        if relative != containment_root and ALLOW_ATTRIBUTE.search(text):
            violations.append(
                f"unsafe_code lint exception must be declared only in containment lib.rs: {relative}"
            )
    if violations:
        raise UnsafeBoundaryError("\n".join(violations))


def validate(root: Path) -> None:
    root = root.resolve()
    validate_workspace_lints(root)
    policy = load_policy(root)
    allowed_paths = validate_policy_inventory(root, policy)
    validate_unsafe_locations(root, allowed_paths)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path("."))
    args = parser.parse_args()
    try:
        validate(args.root)
    except UnsafeBoundaryError as exc:
        print(f"unsafe-boundary-error: {exc}", file=sys.stderr)
        return 1
    print("unsafe-boundary-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
