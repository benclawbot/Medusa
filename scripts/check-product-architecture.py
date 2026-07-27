#!/usr/bin/env python3
"""Validate the public product architecture against production metadata."""

from __future__ import annotations

import re
import sys
from pathlib import Path

import tomllib


class ArchitectureError(RuntimeError):
    """Raised when architecture documents drift from production authority."""


REQUIRED_HEADINGS = (
    "## One-page orientation",
    "## Runtime event flow",
    "## Containment trust boundary",
    "## Orchestration and parent/subagent responsibility",
    "## Verification gate",
    "## Recovery-state lifecycle",
    "## Authoritative persisted records",
    "## Capability evidence and drift control",
)

REQUIRED_DIAGRAM_LABELS = (
    "Runtime event flow",
    "Containment trust boundary",
    "Orchestration and parent/subagent responsibility",
    "Verification gate",
    "Recovery-state lifecycle",
)

REQUIRED_CONCEPTS = ("Plan", "Execute Safely", "Recover")
REQUIRED_AUTHORITY_ROWS = ("Plans", "Execution", "Verification", "Reports", "Learning", "Recovery")
REQUIRED_CONTRIBUTOR_PATHS = (
    "crates/medusa-runtime",
    "crates/medusa-agent",
    "crates/medusa-process-containment",
    "crates/medusa-multi-agent-scheduler",
    "crates/medusa-recovery-coordinator",
)


def read(root: Path, relative: str) -> str:
    path = root / relative
    try:
        text = path.read_text(encoding="utf-8")
    except FileNotFoundError as exc:
        raise ArchitectureError(f"missing architecture input: {relative}") from exc
    if not text.strip():
        raise ArchitectureError(f"empty architecture input: {relative}")
    return text


def require(text: str, needle: str, context: str) -> None:
    if needle not in text:
        raise ArchitectureError(f"{context} is missing {needle!r}")


def validate(root: Path) -> None:
    architecture = read(root, "docs/ARCHITECTURE.md")
    contributor = read(root, "docs/CONTRIBUTOR-ARCHITECTURE.md")
    readme = read(root, "README.md")
    cargo_text = read(root, "Cargo.toml")

    require(readme, "docs/ARCHITECTURE.md", "README.md")
    require(readme, "docs/CONTRIBUTOR-ARCHITECTURE.md", "README.md")

    for heading in REQUIRED_HEADINGS:
        require(architecture, heading, "docs/ARCHITECTURE.md")
    for concept in REQUIRED_CONCEPTS:
        require(architecture, concept, "docs/ARCHITECTURE.md")
    for label in REQUIRED_DIAGRAM_LABELS:
        heading = f"## {label}"
        section = architecture.split(heading, 1)[1].split("\n## ", 1)[0]
        if "```mermaid" not in section:
            raise ArchitectureError(f"architecture section {label!r} requires a Mermaid diagram")
    for row in REQUIRED_AUTHORITY_ROWS:
        if not re.search(rf"^\| {re.escape(row)} \|", architecture, re.MULTILINE):
            raise ArchitectureError(f"authoritative persisted records is missing {row!r}")

    for path in REQUIRED_CONTRIBUTOR_PATHS:
        require(contributor, path, "docs/CONTRIBUTOR-ARCHITECTURE.md")
        if not (root / path).exists():
            raise ArchitectureError(f"contributor map references missing production path: {path}")

    cargo = tomllib.loads(cargo_text)
    metadata = cargo.get("workspace", {}).get("metadata", {}).get("medusa", {})
    expected = {
        "production_execution_model": "single-agent-orchestrated",
        "production_orchestrator": "medusa-runtime::production_orchestrator",
        "subagent_delegation": "planned-bounded-parent-accountable",
        "verification_gate": "repository",
    }
    if metadata != expected:
        raise ArchitectureError(
            "workspace.metadata.medusa must remain the exact production architecture authority: "
            f"expected {expected!r}, got {metadata!r}"
        )

    require(architecture, metadata["production_orchestrator"], "docs/ARCHITECTURE.md")
    require(architecture, "one `AgentEngine`", "docs/ARCHITECTURE.md")
    require(architecture, "does not yet dispatch subagents", "docs/ARCHITECTURE.md")
    require(architecture, "primary agent remains accountable", "docs/ARCHITECTURE.md")
    require(architecture, "repository verification gate", "docs/ARCHITECTURE.md")
    require(architecture, "platform- or prerequisite-limited", "docs/ARCHITECTURE.md")
    require(architecture, "scripts/check-product-architecture.py", "docs/ARCHITECTURE.md")
    require(contributor, metadata["production_orchestrator"], "docs/CONTRIBUTOR-ARCHITECTURE.md")
    require(contributor, "primary agent validates evidence", "docs/CONTRIBUTOR-ARCHITECTURE.md")
    require(contributor, "not dispatched by production `run_prompt`", "docs/CONTRIBUTOR-ARCHITECTURE.md")


if __name__ == "__main__":
    try:
        validate(Path(".").resolve())
    except ArchitectureError as error:
        print(f"product-architecture-error: {error}", file=sys.stderr)
        raise SystemExit(1)
    print("product-architecture-ok")
