#!/usr/bin/env python3
"""Validate public architecture against the authoritative production execution path."""

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


def forbid(text: str, needle: str, context: str) -> None:
    if needle in text:
        raise ArchitectureError(f"{context} contains stale capability wording {needle!r}")


def validate(root: Path) -> None:
    architecture = read(root, "docs/ARCHITECTURE.md")
    contributor = read(root, "docs/CONTRIBUTOR-ARCHITECTURE.md")
    evidence = read(root, "docs/CAPABILITY-EVIDENCE.md")
    trace = read(root, "docs/PRODUCTION-EXECUTION-TRACE.md")
    planning = read(root, "crates/medusa-runtime/src/production_orchestrator.rs")
    runtime_root = read(root, "crates/medusa-runtime/src/runtime_root_generated.rs")
    runtime = read(root, "crates/medusa-runtime/src/lib.rs")
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
            raise ArchitectureError(f"contributor map references missing path: {path}")

    cargo = tomllib.loads(cargo_text)
    metadata = cargo.get("workspace", {}).get("metadata", {}).get("medusa", {})
    expected = {
        "production_execution_model": "single-agent",
        "production_entrypoint": "medusa-runtime::RuntimeController -> run_prompt -> medusa-agent::AgentEngine",
        "orchestration_planning": "medusa-runtime::orchestration_planning (non-production metadata only)",
        "subagent_delegation": "design-only-disabled",
        "verification_gate": "repository",
    }
    if metadata != expected:
        raise ArchitectureError(
            "workspace.metadata.medusa must remain the exact production architecture authority: "
            f"expected {expected!r}, got {metadata!r}"
        )

    for document, context in ((architecture, "docs/ARCHITECTURE.md"), (contributor, "docs/CONTRIBUTOR-ARCHITECTURE.md"), (trace, "docs/PRODUCTION-EXECUTION-TRACE.md")):
        require(document, "RuntimeController", context)
        require(document, "run_prompt", context)
        require(document, "AgentEngine", context)
        require(document, "single-agent", context)

    require(architecture, "medusa-runtime::orchestration_planning", "docs/ARCHITECTURE.md")
    require(architecture, "does not call scheduler", "docs/ARCHITECTURE.md")
    require(architecture, "must not be rendered as proof", "docs/ARCHITECTURE.md")
    forbid(architecture, "production runtime entrypoint is `medusa-runtime::production_orchestrator`", "docs/ARCHITECTURE.md")

    require(contributor, "Design-only and disabled", "docs/CONTRIBUTOR-ARCHITECTURE.md")
    require(contributor, "not called by production `run_prompt`", "docs/CONTRIBUTOR-ARCHITECTURE.md")
    forbid(contributor, "| Production orchestration | `medusa-runtime::production_orchestrator`", "docs/CONTRIBUTOR-ARCHITECTURE.md")

    require(runtime_root, "mod production_orchestrator;", "runtime root")
    require(runtime_root, "pub mod orchestration_planning", "runtime root")
    forbid(runtime_root, "pub mod production_orchestrator;", "runtime root")

    require(runtime, "let engine = AgentEngine::new(provider, config);", "production run_prompt")
    require(runtime, ".step_with_observer_and_context(&mut session", "production run_prompt")
    run_prompt = runtime.split("fn run_prompt(", 1)[1].split("\nfn append_followups", 1)[0]
    for forbidden_call in (
        "production_orchestrator::",
        "orchestration_planning::",
        "medusa_multi_agent_scheduler",
        "medusa_workers",
        "medusa_worker_leases",
        "medusa_consensus",
        "validate_subagent_result",
    ):
        forbid(run_prompt, forbidden_call, "production run_prompt")
    if run_prompt.count("AgentEngine::new(") != 1:
        raise ArchitectureError("production run_prompt must construct exactly one AgentEngine")

    require(planning, "No workers or subagents are dispatched", "orchestration planning")
    require(planning, "allowed: false", "orchestration planning delegation policy")
    forbid(planning, "Production multi-agent execution is active", "orchestration planning")
    forbid(planning, "title: format!(\"Dispatch wave", "orchestration planning events")

    require(trace, "not called by `run_prompt`", "production execution trace")
    require(trace, "does not create worker engines", "production execution trace")
    require(trace, "must never be rendered as evidence", "production execution trace")

    require(evidence, "## Planned and scaffolding behavior", "docs/CAPABILITY-EVIDENCE.md")
    require(evidence, "not shipped production capabilities", "docs/CAPABILITY-EVIDENCE.md")
    shipped_section = evidence.split("## Shipped on `main`", 1)[1].split(
        "## Planned and scaffolding behavior", 1
    )[0]
    forbid(shipped_section, "parallel workers with isolated worktrees", "docs/CAPABILITY-EVIDENCE.md shipped section")


if __name__ == "__main__":
    try:
        validate(Path(".").resolve())
    except ArchitectureError as error:
        print(f"product-architecture-error: {error}", file=sys.stderr)
        raise SystemExit(1)
    print("product-architecture-ok")
