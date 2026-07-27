#!/usr/bin/env python3
"""Validate Medusa capability maturity against repository evidence."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any

REQUIRED_CLAIM_FIELDS = {
    "id",
    "maturity",
    "summary",
    "owner",
    "production_paths",
    "test_paths",
    "gates",
    "entrypoints",
    "supported_platforms",
    "external_dependencies",
    "observability",
    "documentation",
    "promotion_checklist",
    "default_enabled",
    "opt_in",
    "dependencies",
}
ALLOWED_MATURITIES = {"production", "preview", "experimental", "design-only"}
MATURITY_RANK = {"design-only": 0, "experimental": 1, "preview": 2, "production": 3}
ALLOWED_PLATFORMS = {"linux", "macos", "windows"}
CANONICAL_GATES = {"CI", "Daemon", "Desktop", "Refactor Guardrails", "Release Gates"}
PRODUCTION_CHECKLIST = {
    "owner assigned",
    "entrypoint identified",
    "behavioral tests present",
    "supported platforms declared",
    "observability documented",
    "public documentation linked",
}
VOLATILE_PATTERNS = {
    "open PR state": re.compile(r"\b(?:open|draft|pending)\s+(?:pull request|PR)\s+#?\d+", re.I),
    "unsupported passing snapshot": re.compile(r"\b(?:all\s+)?tests?\s+(?:are\s+)?passing\b", re.I),
    "dated PR snapshot": re.compile(r"status snapshot:.*(?:PR|pull request)\s+#?\d+", re.I),
    "superseded final document": re.compile(r"\bFINAL\.md\b"),
}


class EvidenceError(RuntimeError):
    """Raised when capability metadata or public documents drift."""


def load_json(path: Path) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as exc:
        raise EvidenceError(f"missing manifest: {path}") from exc
    except json.JSONDecodeError as exc:
        raise EvidenceError(f"invalid JSON in {path}: {exc}") from exc
    if not isinstance(payload, dict):
        raise EvidenceError(f"manifest root must be an object: {path}")
    return payload


def require_path(root: Path, relative: str, context: str) -> None:
    if not relative or Path(relative).is_absolute() or ".." in Path(relative).parts:
        raise EvidenceError(f"unsafe or empty path in {context}: {relative!r}")
    if not (root / relative).exists():
        raise EvidenceError(f"deleted or missing path referenced by {context}: {relative}")


def require_string_list(value: Any, context: str) -> list[str]:
    if not isinstance(value, list) or any(not isinstance(item, str) for item in value):
        raise EvidenceError(f"{context} must be a list of strings")
    return value


def validate_documents(root: Path, manifest: dict[str, Any]) -> None:
    documents = manifest.get("required_documents")
    if not isinstance(documents, list) or not documents:
        raise EvidenceError("required_documents must be a non-empty list")
    for relative in documents:
        if not isinstance(relative, str):
            raise EvidenceError("required_documents entries must be strings")
        require_path(root, relative, "required_documents")
        text = (root / relative).read_text(encoding="utf-8")
        if not text.strip():
            raise EvidenceError(f"required document is empty: {relative}")
        for label, pattern in VOLATILE_PATTERNS.items():
            match = pattern.search(text)
            if match:
                raise EvidenceError(
                    f"{relative} contains {label}: {match.group(0)!r}; record durable evidence instead"
                )

    readme = (root / "README.md").read_text(encoding="utf-8")
    if "docs/CAPABILITY-EVIDENCE.md" not in readme:
        raise EvidenceError("README.md must link to docs/CAPABILITY-EVIDENCE.md")
    ledger = (root / "docs/CAPABILITY-EVIDENCE.md").read_text(encoding="utf-8")
    for heading in ("## Evidence rules", "## Capability maturity matrix", "## Canonical gates"):
        if heading not in ledger:
            raise EvidenceError(f"capability ledger is missing heading: {heading}")


def validate_maturity_model(manifest: dict[str, Any]) -> None:
    if manifest.get("schema_version") != 2:
        raise EvidenceError("unsupported capability claim schema_version")
    model = manifest.get("maturity_model")
    if not isinstance(model, dict) or set(model) != ALLOWED_MATURITIES:
        raise EvidenceError("maturity_model must define production, preview, experimental, and design-only")
    if any(not isinstance(description, str) or not description.strip() for description in model.values()):
        raise EvidenceError("maturity_model descriptions must be non-empty strings")


def validate_claim_shape(root: Path, claim: dict[str, Any], context: str) -> None:
    missing = REQUIRED_CLAIM_FIELDS - claim.keys()
    if missing:
        raise EvidenceError(f"{context} missing fields: {sorted(missing)}")
    claim_id = claim["id"]
    if not isinstance(claim_id, str) or not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", claim_id):
        raise EvidenceError(f"invalid claim id in {context}: {claim_id!r}")
    if claim["maturity"] not in ALLOWED_MATURITIES:
        raise EvidenceError(f"invalid maturity for {claim_id}: {claim['maturity']!r}")
    for field in ("summary", "owner"):
        if not isinstance(claim[field], str) or not claim[field].strip():
            raise EvidenceError(f"claim {claim_id} requires {field}")

    list_fields = (
        "production_paths",
        "test_paths",
        "gates",
        "entrypoints",
        "supported_platforms",
        "external_dependencies",
        "observability",
        "documentation",
        "promotion_checklist",
        "dependencies",
    )
    for field in list_fields:
        require_string_list(claim[field], f"claim {claim_id} field {field}")
    if not isinstance(claim["default_enabled"], bool):
        raise EvidenceError(f"claim {claim_id} default_enabled must be boolean")
    if claim["opt_in"] is not None and not isinstance(claim["opt_in"], str):
        raise EvidenceError(f"claim {claim_id} opt_in must be a string or null")

    for field in ("production_paths", "test_paths", "observability", "documentation"):
        for relative in claim[field]:
            require_path(root, relative, f"claim {claim_id} {field}")
    unknown_gates = set(claim["gates"]) - CANONICAL_GATES
    if unknown_gates:
        raise EvidenceError(f"claim {claim_id} references unknown gates: {sorted(unknown_gates)}")
    unknown_platforms = set(claim["supported_platforms"]) - ALLOWED_PLATFORMS
    if unknown_platforms:
        raise EvidenceError(f"claim {claim_id} references unknown platforms: {sorted(unknown_platforms)}")


def validate_maturity_policy(claim: dict[str, Any]) -> None:
    claim_id = claim["id"]
    maturity = claim["maturity"]
    if maturity == "production":
        required_non_empty = (
            "production_paths",
            "test_paths",
            "gates",
            "entrypoints",
            "supported_platforms",
            "observability",
            "documentation",
        )
        for field in required_non_empty:
            if not claim[field]:
                raise EvidenceError(f"production claim {claim_id} has no {field}")
        missing = PRODUCTION_CHECKLIST - set(claim["promotion_checklist"])
        if missing:
            raise EvidenceError(f"production claim {claim_id} has incomplete promotion checklist: {sorted(missing)}")
        if not claim["default_enabled"]:
            raise EvidenceError(f"production claim {claim_id} must be default enabled")
        if claim["opt_in"] is not None:
            raise EvidenceError(f"production claim {claim_id} must not require opt_in")
    elif maturity in {"preview", "experimental"}:
        if claim["default_enabled"]:
            raise EvidenceError(f"non-production claim {claim_id} must not be default enabled")
        if not claim["opt_in"]:
            raise EvidenceError(f"{maturity} claim {claim_id} requires an explicit opt_in")
    else:
        if claim["default_enabled"]:
            raise EvidenceError(f"design-only claim {claim_id} must not be default enabled")
        if claim["entrypoints"]:
            raise EvidenceError(f"design-only claim {claim_id} must not expose production entrypoints")
        if claim["opt_in"] is not None:
            raise EvidenceError(f"design-only claim {claim_id} must not expose an opt_in")


def validate_claims(root: Path, manifest: dict[str, Any]) -> None:
    claims = manifest.get("claims")
    if not isinstance(claims, list) or not claims:
        raise EvidenceError("claims must be a non-empty list")
    seen: set[str] = set()
    by_id: dict[str, dict[str, Any]] = {}
    for index, claim in enumerate(claims):
        context = f"claims[{index}]"
        if not isinstance(claim, dict):
            raise EvidenceError(f"{context} must be an object")
        validate_claim_shape(root, claim, context)
        validate_maturity_policy(claim)
        if claim["id"] in seen:
            raise EvidenceError(f"duplicate claim id: {claim['id']}")
        seen.add(claim["id"])
        by_id[claim["id"]] = claim

    for claim in claims:
        for dependency_id in claim["dependencies"]:
            dependency = by_id.get(dependency_id)
            if dependency is None:
                raise EvidenceError(f"claim {claim['id']} references unknown dependency: {dependency_id}")
            if claim["maturity"] == "production" and dependency["maturity"] != "production":
                raise EvidenceError(
                    f"production claim {claim['id']} depends on non-production capability {dependency_id}"
                )
            if MATURITY_RANK[dependency["maturity"]] < MATURITY_RANK[claim["maturity"]] and claim["default_enabled"]:
                raise EvidenceError(
                    f"default-enabled claim {claim['id']} exceeds dependency maturity {dependency_id}"
                )


def validate_ledger_coverage(root: Path, manifest: dict[str, Any]) -> None:
    ledger = (root / "docs/CAPABILITY-EVIDENCE.md").read_text(encoding="utf-8")
    for claim in manifest["claims"]:
        marker = f"`{claim['id']}`"
        if marker not in ledger:
            raise EvidenceError(f"claim {claim['id']} is missing from docs/CAPABILITY-EVIDENCE.md")
        maturity_marker = f"`{claim['maturity']}`"
        if maturity_marker not in ledger:
            raise EvidenceError(f"ledger does not describe maturity {claim['maturity']}")


def validate(root: Path, manifest_path: Path) -> None:
    manifest = load_json(manifest_path)
    validate_maturity_model(manifest)
    validate_documents(root, manifest)
    validate_claims(root, manifest)
    validate_ledger_coverage(root, manifest)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path("."))
    parser.add_argument("--manifest", type=Path, default=Path("docs/CAPABILITY-CLAIMS.json"))
    args = parser.parse_args()
    root = args.root.resolve()
    manifest_path = args.manifest if args.manifest.is_absolute() else root / args.manifest
    try:
        validate(root, manifest_path)
    except EvidenceError as exc:
        print(f"capability-evidence-error: {exc}", file=sys.stderr)
        return 1
    print("capability-evidence-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
