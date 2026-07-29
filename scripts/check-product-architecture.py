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
    coordinator = read(root, "crates/medusa-runtime/src/multi_agent_coordinator.rs")
    runtime_root = read(root, "crates/medusa-runtime/src/runtime_root_generated.rs")
    runtime = read(root, "crates/medusa-runtime/src/runtime_impl.rs")
    build_main = read(root, "crates/medusa-runtime/build_main.rs")
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
        "production_execution_model": "bounded-read-only-teammates-with-parent-owned-mutation",
        "production_entrypoint": "medusa-runtime::RuntimeController -> run_prompt -> multi_agent_coordinator::run_preflight -> bounded medusa-agent::AgentEngine teammates -> parent medusa-agent::AgentEngine",
        "orchestration_planning": "production runtime path; task contracts and schedule waves drive durable read-only teammate dispatch",
        "subagent_delegation": "production-read-only; mutating teammate dispatch remains disabled until worktree isolation and guarded integration are enabled",
        "verification_gate": "repository",
    }
    if metadata != expected:
        raise ArchitectureError(
            "workspace.metadata.medusa must remain the exact production architecture authority: "
            f"expected {expected!r}, got {metadata!r}"
        )

    for document, context in (
        (architecture, "docs/ARCHITECTURE.md"),
        (contributor, "docs/CONTRIBUTOR-ARCHITECTURE.md"),
        (trace, "docs/PRODUCTION-EXECUTION-TRACE.md"),
    ):
        require(document, "RuntimeController", context)
        require(document, "run_prompt", context)
        require(document, "AgentEngine", context)
        require(document, "read-only", context)
        require(document, "parent", context)

    require(architecture, "MultiAgentCoordinator", "docs/ARCHITECTURE.md")
    require(architecture, "sole mutation authority", "docs/ARCHITECTURE.md")
    require(architecture, "repository verification gate", "docs/ARCHITECTURE.md")
    forbid(architecture, "run_prompt does not call scheduler", "docs/ARCHITECTURE.md")

    require(contributor, "Production multi-agent coordinator", "docs/CONTRIBUTOR-ARCHITECTURE.md")
    require(contributor, "called by production `run_prompt`", "docs/CONTRIBUTOR-ARCHITECTURE.md")
    require(contributor, "Mutating worker", "docs/CONTRIBUTOR-ARCHITECTURE.md")

    require(runtime_root, "mod production_orchestrator;", "runtime root")
    require(runtime_root, "pub mod orchestration_planning", "runtime root")
    forbid(runtime_root, "pub mod production_orchestrator;", "runtime root")

    require(runtime, "fn run_prompt(", "runtime implementation")
    require(runtime, "AgentEngine::new_with_cancellation", "runtime implementation")
    require(runtime, ".step_with_observer_and_context(", "runtime implementation")
    if runtime.split("fn run_prompt(", 1)[1].split("\nfn append_followups", 1)[0].count(
        "AgentEngine::new_with_cancellation"
    ) != 1:
        raise ArchitectureError("parent run_prompt must construct exactly one parent AgentEngine")

    for needle in (
        "multi_agent_coordinator::run_preflight",
        "multi_agent_coordinator::verify_repository",
        "production_orchestrator::plan",
        "coordinator_evidence",
    ):
        require(build_main, needle, "runtime build integration")

    for needle in (
        "thread::scope",
        "WorkerExecutionController",
        "TeamRuntime",
        "Mode::ReadOnly",
        "AgentExecutionPolicy::for_team_role",
        "repository_fingerprint",
        "targeted_verification",
        "accept_persisted_completion",
        "recover_interrupted",
    ):
        require(coordinator, needle, "production multi-agent coordinator")
    forbid(coordinator, "Mode::Full", "production read-only coordinator")
    require(coordinator, "parent remains responsible for all mutations", "production multi-agent coordinator")

    require(planning, "Independent read-only teammates are dispatched", "production orchestration planning")
    require(planning, "AgentRole::Researcher", "production orchestration planning")
    require(planning, "allowed: matches!", "production orchestration delegation policy")
    forbid(planning, "No workers or subagents are dispatched", "production orchestration planning")

    require(trace, "multi_agent_coordinator::run_preflight", "production execution trace")
    require(trace, "repository-content fingerprint", "production execution trace")
    require(trace, "sole mutation", "production execution trace")
    require(trace, "Mutating teammate dispatch is not part", "production execution trace")

    require(evidence, "## Planned and scaffolding behavior", "docs/CAPABILITY-EVIDENCE.md")
    require(evidence, "`multi-agent-research` | `production`", "docs/CAPABILITY-EVIDENCE.md")
    shipped_section = evidence.split("## Production capability evidence", 1)[1].split(
        "## Planned and scaffolding behavior", 1
    )[0]
    require(shipped_section, "read-only planner and risk-reviewer", "docs/CAPABILITY-EVIDENCE.md production section")
    forbid(shipped_section, "isolated worktrees", "docs/CAPABILITY-EVIDENCE.md production section")


if __name__ == "__main__":
    try:
        validate(Path(".").resolve())
    except ArchitectureError as error:
        print(f"product-architecture-error: {error}", file=sys.stderr)
        raise SystemExit(1)
    print("product-architecture-ok")
