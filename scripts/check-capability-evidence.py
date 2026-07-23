#!/usr/bin/env python3
"""Validate Medusa capability claims against repository evidence.

This checker is intentionally repository-local and deterministic. It validates that
public claims reference files that exist in the checked-out commit, that shipped
claims name tests and canonical gates, and that status documents avoid volatile
snapshots such as open-PR states or unsupported "tests passing" assertions.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any

REQUIRED_CLAIM_FIELDS = {
    "id",
    "status",
    "summary",
    "production_paths",
    "test_paths",
    "gates",
}
ALLOWED_STATUSES = {"shipped", "experimental", "planned"}
CANONICAL_GATES = {
    "CI",
    "Daemon",
    "Desktop",
    "Refactor Guardrails",
    "Release Gates",
}
VOLATILE_PATTERNS = {
    "open PR state": re.compile(r"\b(?:open|draft|pending)\s+(?:pull request|PR)\s+#?\d+", re.I),
    "unsupported passing snapshot": re.compile(r"\b(?:all\s+)?tests?\s+(?:are\s+)?passing\b", re.I),
    "dated PR snapshot": re.compile(r"status snapshot:.*(?:PR|pull request)\s+#?\d+", re.I),
    "superseded final document": re.compile(r"\bFINAL\.md\b"),
}


class EvidenceError(RuntimeError):
    """Raised when the evidence manifest or public documents drift."""


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
    path = root / relative
    if not path.exists():
        raise EvidenceError(f"deleted or missing path referenced by {context}: {relative}")


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
                    f"{relative} contains {label}: {match.group(0)!r}; "
                    "record durable code/gate evidence instead"
                )

    readme = (root / "README.md").read_text(encoding="utf-8")
    if "docs/CAPABILITY-EVIDENCE.md" not in readme:
        raise EvidenceError("README.md must link to docs/CAPABILITY-EVIDENCE.md")
    ledger = (root / "docs/CAPABILITY-EVIDENCE.md").read_text(encoding="utf-8")
    for heading in ("## Evidence rules", "## Shipped on `main`", "## Canonical gates"):
        if heading not in ledger:
            raise EvidenceError(f"capability ledger is missing heading: {heading}")


def validate_claims(root: Path, manifest: dict[str, Any]) -> None:
    if manifest.get("schema_version") != 1:
        raise EvidenceError("unsupported capability claim schema_version")
    claims = manifest.get("claims")
    if not isinstance(claims, list) or not claims:
        raise EvidenceError("claims must be a non-empty list")

    seen: set[str] = set()
    for index, claim in enumerate(claims):
        context = f"claims[{index}]"
        if not isinstance(claim, dict):
            raise EvidenceError(f"{context} must be an object")
        missing = REQUIRED_CLAIM_FIELDS - claim.keys()
        if missing:
            raise EvidenceError(f"{context} missing fields: {sorted(missing)}")

        claim_id = claim["id"]
        if not isinstance(claim_id, str) or not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", claim_id):
            raise EvidenceError(f"invalid claim id in {context}: {claim_id!r}")
        if claim_id in seen:
            raise EvidenceError(f"duplicate claim id: {claim_id}")
        seen.add(claim_id)

        status = claim["status"]
        if status not in ALLOWED_STATUSES:
            raise EvidenceError(f"invalid status for {claim_id}: {status!r}")
        if not isinstance(claim["summary"], str) or not claim["summary"].strip():
            raise EvidenceError(f"claim {claim_id} requires a summary")

        for field in ("production_paths", "test_paths", "gates"):
            value = claim[field]
            if not isinstance(value, list):
                raise EvidenceError(f"claim {claim_id} field {field} must be a list")

        if status == "shipped":
            if not claim["production_paths"]:
                raise EvidenceError(f"shipped claim {claim_id} has no production evidence")
            if not claim["test_paths"]:
                raise EvidenceError(f"shipped claim {claim_id} has no test evidence")
            if not claim["gates"]:
                raise EvidenceError(f"shipped claim {claim_id} has no gate evidence")

        for relative in claim["production_paths"]:
            if not isinstance(relative, str):
                raise EvidenceError(f"claim {claim_id} production_paths must contain strings")
            require_path(root, relative, f"claim {claim_id} production evidence")
        for relative in claim["test_paths"]:
            if not isinstance(relative, str):
                raise EvidenceError(f"claim {claim_id} test_paths must contain strings")
            require_path(root, relative, f"claim {claim_id} test evidence")
        unknown_gates = set(claim["gates"]) - CANONICAL_GATES
        if unknown_gates:
            raise EvidenceError(f"claim {claim_id} references unknown gates: {sorted(unknown_gates)}")


def validate_ledger_coverage(root: Path, manifest: dict[str, Any]) -> None:
    ledger = (root / "docs/CAPABILITY-EVIDENCE.md").read_text(encoding="utf-8")
    for claim in manifest["claims"]:
        if claim["status"] == "shipped" and f"`{claim['id']}`" not in ledger:
            raise EvidenceError(
                f"shipped claim {claim['id']} is missing from docs/CAPABILITY-EVIDENCE.md"
            )


def validate(root: Path, manifest_path: Path) -> None:
    manifest = load_json(manifest_path)
    validate_documents(root, manifest)
    validate_claims(root, manifest)
    validate_ledger_coverage(root, manifest)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path("."))
    parser.add_argument(
        "--manifest",
        type=Path,
        default=Path("docs/CAPABILITY-CLAIMS.json"),
    )
    args = parser.parse_args()
    root = args.root.resolve()
    manifest_path = args.manifest
    if not manifest_path.is_absolute():
        manifest_path = root / manifest_path
    try:
        validate(root, manifest_path)
    except EvidenceError as exc:
        print(f"capability-evidence-error: {exc}", file=sys.stderr)
        return 1
    print("capability-evidence-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
