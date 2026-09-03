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
    "crates/medusa-workers",
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
    workspaces = read(root, "docs/WORKSPACES.md")
    multi_agent = read(root, "docs/MULTI_AGENT_EXECUTION.md")
    planning = read(root, "crates/medusa-runtime/src/coordination/production_orchestrator.rs")
    read_only_coordinator = read(root, "crates/medusa-runtime/src/coordination/multi_agent_coordinator.rs")
    mutating_coordinator = read(root, "crates/medusa-runtime/src/coordination/mutating_worker_coordinator.rs")
    mutating_coordinator += "\n" + read(root, "crates/medusa-runtime/src/coordination/mutating_worker_coordinator_inner.rs")
    mutating_coordinator += "\n" + read(root, "crates/medusa-runtime/src/coordination/mutating_worker_coordinator_support.rs")
    mutation_transaction = read(root, "crates/medusa-runtime/src/mutation_transaction_state.rs")
    parallel_mutation = read(root, "crates/medusa-runtime/src/parallel_mutation.rs")
    parallel_batch = read(root, "crates/medusa-runtime/src/parallel_mutation_batch.rs")
    workspace_backend = read(root, "crates/medusa-runtime/src/workspace_worker_manager.rs")
    workspace_api = read(root, "crates/medusa-runtime/src/workspace.rs")
    workers = read(root, "crates/medusa-workers/src/lib.rs")
    runtime = read(root, "crates/medusa-runtime/src/lib.rs")
    coordination = read(root, "crates/medusa-runtime/src/coordination/mod.rs")
    readme = read(root, "README.md")
    cargo_text = read(root, "Cargo.toml")

    require(readme, "docs/ARCHITECTURE.md", "README.md")
    require(readme, "docs/CONTRIBUTOR-ARCHITECTURE.md", "README.md")
    require(readme, "docs/WORKSPACES.md", "README.md")
    for heading in REQUIRED_HEADINGS:
        require(architecture, heading, "docs/ARCHITECTURE.md")
    for concept in REQUIRED_CONCEPTS:
        require(architecture, concept, "docs/ARCHITECTURE.md")
    for label in REQUIRED_DIAGRAM_LABELS:
        section = architecture.split(f"## {label}", 1)[1].split("\n## ", 1)[0]
        if "```mermaid" not in section:
            raise ArchitectureError(f"architecture section {label!r} requires a Mermaid diagram")
    for row in REQUIRED_AUTHORITY_ROWS:
        if not re.search(rf"^\| {re.escape(row)} \|", architecture, re.MULTILINE):
            raise ArchitectureError(f"authoritative persisted records is missing {row!r}")
    for path in REQUIRED_CONTRIBUTOR_PATHS:
        require(contributor, path, "docs/CONTRIBUTOR-ARCHITECTURE.md")
        if not (root / path).exists():
            raise ArchitectureError(f"contributor map references missing path: {path}")

    metadata = tomllib.loads(cargo_text).get("workspace", {}).get("metadata", {}).get("medusa", {})
    expected = {
        "production_execution_model": "bounded-teammates-with-workspace-isolated-mutation",
        "production_entrypoint": "medusa-runtime::RuntimeController -> run_prompt -> coordination::multi_agent_coordinator::run_preflight -> coordination::mutating_worker_coordinator::run_implementation when required -> workspace-isolated candidate verification -> dedicated durable parent reviewer -> independent verification -> authorization -> integration -> reconciliation -> canonical terminal persistence",
        "orchestration_planning": "production runtime path; task contracts drive durable read-only preflight, conflict-aware bounded Git parallel mutation when safe, and isolated Git or directory implementation",
        "subagent_delegation": "production; bounded read-only planner and risk-reviewer teammates plus centrally scheduled Git mutation DAG children when safe; directory mutation remains one isolated snapshot implementer; nested delegation is denied",
        "verification_gate": "typed-evidence-and-changed-component-authority",
    }
    if metadata != expected:
        raise ArchitectureError(f"workspace.metadata.medusa must remain the exact production architecture authority: expected {expected!r}, got {metadata!r}")

    for document, context in (
        (architecture, "docs/ARCHITECTURE.md"),
        (contributor, "docs/CONTRIBUTOR-ARCHITECTURE.md"),
        (trace, "docs/PRODUCTION-EXECUTION-TRACE.md"),
    ):
        for needle in ("RuntimeController", "run_prompt", "AgentEngine", "read-only", "parent"):
            require(document, needle, context)

    for needle in (
        "MultiAgentCoordinator",
        "conflict-aware mutation DAG",
        "content-addressed",
        "workspace verification gate",
        "IntegrationBarrier",
    ):
        require(architecture, needle, "docs/ARCHITECTURE.md")
    for stale in (
        "Current boundary:** the shipped path supports the current single implementer contract",
        "current production mutation slice dispatches exactly one implementer contract",
        "Dynamic multi-implementer decomposition remains a later promotion boundary",
    ):
        forbid(architecture + trace + multi_agent, stale, "production architecture documentation")

    require(contributor, "Production multi-agent coordinator", "docs/CONTRIBUTOR-ARCHITECTURE.md")
    require(contributor, "Production mutating worker coordinator", "docs/CONTRIBUTOR-ARCHITECTURE.md")
    require(contributor, "called by production `run_prompt`", "docs/CONTRIBUTOR-ARCHITECTURE.md")

    require(runtime, "pub(crate) mod coordination;", "runtime root")
    require(coordination, "pub(crate) mod multi_agent_coordinator;", "coordination root")
    require(coordination, "pub(crate) mod mutating_worker_coordinator;", "coordination root")
    require(coordination, "pub mod production_orchestrator;", "coordination root")
    require(runtime, "pub mod orchestration_planning", "runtime root")
    forbid(runtime, "pub mod production_orchestrator;", "runtime root")
    require(runtime, "fn run_prompt(", "runtime implementation")
    require(runtime, "AgentEngine::new_with_cancellation", "runtime implementation")
    require(
        runtime,
        ".step_with_observer_and_context_and_turn_instruction_for_phase(",
        "runtime implementation",
    )

    for needle in (
        "multi_agent_coordinator::run_preflight",
        "mutating_worker_coordinator::run_implementation",
        "mutation_transaction::complete_after_parent_review",
        "production_orchestrator::requires_mutation",
        "TeamRole::Reviewer",
        "implementation_evidence",
    ):
        require(runtime, needle, "runtime integration")

    for needle in (
        "thread::scope",
        "WorkerExecutionController",
        "TeamRuntime",
        "Mode::ReadOnly",
        "repository_fingerprint",
        "accept_persisted_completion",
        "recover_interrupted",
    ):
        require(read_only_coordinator, needle, "production read-only coordinator")
    forbid(read_only_coordinator, "Mode::Yolo", "production read-only coordinator")

    for needle in (
        "Mode::Yolo",
        "TeamRole::Implementer",
        "open_or_create_worker",
        "validate_changed_paths",
        "prepare_components_for_verification",
        "authoritative_verification_for_components_at",
        "finalize_worker",
        "discard_untracked_runtime_state",
        "recover_interrupted",
        "run_parallel_implementations",
    ):
        require(mutating_coordinator, needle, "production mutating coordinator")

    for needle in (
        "MAX_PARALLEL_MUTATORS",
        "MutationDag::build",
        "MutationResourceKind::Manifest",
        "MutationResourceKind::Lockfile",
        "MutationResourceKind::Migration",
        "directory workspaces use one isolated content-addressed snapshot implementer",
    ):
        require(parallel_mutation, needle, "parallel mutation planner")

    for needle in (
        "IntegrationBarrier",
        "run_parallel_implementations",
        "deterministic_integration_order",
        "parallel-mutation-metrics.json",
        "aggregate verification",
    ):
        require(parallel_batch, needle, "parallel mutation aggregate authority")

    for needle in (
        "WorkspaceMutationBackend",
        "Directory",
        "directory_manifest",
        "materialize_detached_commit",
        "integrate_authorized",
        "drifted before integration",
        "fails closed on symlink",
    ):
        require(workspace_backend, needle, "workspace mutation backend")
    for needle in ("WorkspaceKind", "Ephemeral", "start_workspace", "cleanup"):
        require(workspace_api, needle, "workspace API")

    for needle in (
        "verify_independently",
        "record_authoritative_verification",
        "authorize",
        "integrate_authorized",
        "reconcile",
        "commit_tree_matches_head",
    ):
        require(mutation_transaction, needle, "production mutation transaction")

    for needle in (
        "open_or_create_worker",
        "worker path overlap rejected before integration",
        "reset",
        "--hard",
        "commit_tree_matches_head",
        "worktree",
        "branch",
        "-D",
    ):
        require(workers, needle, "Git worktree manager")

    for needle in (
        "read_only_objective",
        "TaskKind::Analysis",
        "TaskKind::RiskReview",
        "TaskKind::Implementation",
        "AgentRole::Planner",
        "AgentRole::Researcher",
        "DelegationPolicy",
        "requires_mutation",
    ):
        require(planning, needle, "production orchestration planning")
    forbid(planning, "No workers or subagents are dispatched", "production orchestration planning")

    for needle in (
        "multi_agent_coordinator::run_preflight",
        "mutating_worker_coordinator::run_implementation",
        "parallel_mutation::decomposition_for",
        "content-addressed snapshot",
        "zero-tool parent review",
        "rollback",
    ):
        require(trace, needle, "production execution trace")

    for needle in (
        "Git workspace",
        "Directory workspace",
        "Ephemeral workspace",
        "Parallel **mutating** implementers are intentionally Git-only",
        "nested autonomous delegation remains disabled",
    ):
        require(workspaces, needle, "docs/WORKSPACES.md")

    require(evidence, "## Planned and scaffolding behavior", "docs/CAPABILITY-EVIDENCE.md")
    maturity_heading = "## Capability maturity matrix"
    certification_heading = "## Architecture v2 certification authority"
    require(evidence, maturity_heading, "docs/CAPABILITY-EVIDENCE.md")
    require(evidence, certification_heading, "docs/CAPABILITY-EVIDENCE.md")
    shipped = evidence.split(maturity_heading, 1)[1].split(certification_heading, 1)[0]
    for needle in (
        "`multi-agent-research` | `production`",
        "Linux, macOS, Windows",
    ):
        require(shipped, needle, "docs/CAPABILITY-EVIDENCE.md production section")


if __name__ == "__main__":
    try:
        validate(Path(".").resolve())
    except ArchitectureError as error:
        print(f"product-architecture-error: {error}", file=sys.stderr)
        raise SystemExit(1)
    print("product-architecture-ok")
