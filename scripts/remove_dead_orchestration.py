#!/usr/bin/env python3
"""Remove superseded orchestration authorities after production coordinator proof."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

CHECKER = Path("scripts/check-product-architecture.py")
HOOK_START = "# BEGIN DEAD ORCHESTRATION CLEANUP\n"
HOOK_END = "# END DEAD ORCHESTRATION CLEANUP\n"


def edit_once(path: str, old: str, new: str) -> None:
    target = Path(path)
    source = target.read_text()
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one anchor, found {count}: {old[:120]!r}")
    target.write_text(source.replace(old, new, 1))


def remove_exact(path: str, snippet: str, expected: int = 1) -> None:
    target = Path(path)
    source = target.read_text()
    count = source.count(snippet)
    if count != expected:
        raise SystemExit(f"{path}: expected {expected} occurrences, found {count}: {snippet[:120]!r}")
    target.write_text(source.replace(snippet, ""))


remove_exact(
    "crates/medusa-agent/src/engine.rs",
    '#[allow(dead_code)]\n#[path = "autonomous_execution.rs"]\nmod autonomous_execution;\n',
)
remove_exact(
    "crates/medusa-agent/src/engine.rs",
    '\ninclude!(concat!(\n    env!("CARGO_MANIFEST_DIR"),\n    "/src/autonomous_engine.rs"\n));\n',
)
for path in (
    "crates/medusa-agent/src/autonomous_execution.rs",
    "crates/medusa-agent/src/autonomous_engine.rs",
    "crates/medusa-agent/src/transaction_pipeline.rs",
):
    Path(path).unlink()

for dependency in (
    'medusa-commit-barrier = { path = "../medusa-commit-barrier" }\n',
    'medusa-conflict-resolution = { path = "../medusa-conflict-resolution" }\n',
    'medusa-consensus = { path = "../medusa-consensus" }\n',
    'medusa-repository-rollback = { path = "../medusa-repository-rollback" }\n',
    'medusa-transaction-coordinator = { path = "../medusa-transaction-coordinator" }\n',
):
    remove_exact("crates/medusa-agent/Cargo.toml", dependency)

for member in (
    '  "crates/medusa-commit-barrier",\n',
    '  "crates/medusa-repository-rollback",\n',
    '  "crates/medusa-conflict-resolution",\n',
    '  "crates/medusa-consensus",\n',
):
    remove_exact("Cargo.toml", member)

for directory in (
    "crates/medusa-commit-barrier",
    "crates/medusa-repository-rollback",
    "crates/medusa-conflict-resolution",
    "crates/medusa-consensus",
):
    shutil.rmtree(directory)

edit_once(
    "crates/medusa-cli/src/product_acceptance.rs",
    '''        Scenario {
            id: "verification-rollback",
            guarantee: "Repository changes can be rolled back after failed or rejected integration.",
            package: "medusa-repository-rollback",
            filter: None,
            required_marker: None,
        },
''',
    '''        Scenario {
            id: "verification-rollback",
            guarantee: "A failed worktree integration restores the exact pre-integration repository HEAD.",
            package: "medusa-workers",
            filter: Some("integration_conflict_rolls_back_to_the_preintegration_head"),
            required_marker: Some("integration_conflict_rolls_back_to_the_preintegration_head"),
        },
''',
)
edit_once(
    "scripts/product-acceptance-smoke.py",
    '''    {
        "id": "verification-rollback",
        "guarantee": "Failed or rejected integration can roll repository changes back.",
        "args": ["test", "-p", "medusa-repository-rollback", "--locked"],
        "marker": None,
    },
''',
    '''    {
        "id": "verification-rollback",
        "guarantee": "A failed worktree integration restores the exact pre-integration repository HEAD.",
        "args": [
            "test",
            "-p",
            "medusa-workers",
            "integration_conflict_rolls_back_to_the_preintegration_head",
            "--locked",
            "--",
            "--nocapture",
        ],
        "marker": "integration_conflict_rolls_back_to_the_preintegration_head",
    },
''',
)

edit_once(
    "docs/CONTRIBUTOR-ARCHITECTURE.md",
    '| Commit barrier and consensus | Design-only supporting paths | `crates/medusa-commit-barrier`, `crates/medusa-consensus` | `crates/medusa-conflict-resolution` |\n',
    '',
)
edit_once(
    "docs/CONTRIBUTOR-ARCHITECTURE.md",
    '| Filesystem transaction safety | Shipped | `crates/medusa-agent/src/transaction.rs` | `crates/medusa-repository-rollback` |\n',
    '| Filesystem transaction safety | Shipped | `crates/medusa-agent/src/transaction.rs` | approval receipts and session rollback evidence |\n',
)
edit_once(
    "docs/CONTRIBUTOR-ARCHITECTURE.md",
    '| Recovery coordination | `crates/medusa-recovery-coordinator` | `crates/medusa-repository-rollback` |\n',
    '| Recovery coordination | `crates/medusa-recovery-coordinator` | runtime checkpoints, worktree receipts, and failure history |\n',
)
edit_once(
    "docs/CONTRIBUTOR-ARCHITECTURE.md",
    'Current coordinated execution constructs separate planner and risk-reviewer sessions. A mutating objective then creates one execution-specific implementer worktree, runs an implementer `AgentEngine` there, validates its changed paths and verification evidence, and integrates its deterministic commit. The parent session is read-only and owns review, reporting, and the final verification gate. Dynamic multi-implementer decomposition and autonomous team steering remain later promotion slices.\n',
    'Current coordinated execution constructs separate planner and risk-reviewer sessions. A mutating objective then creates one execution-specific implementer worktree, runs an implementer `AgentEngine` there, validates its changed paths and verification evidence, and integrates its deterministic commit. The parent session is read-only and owns review, reporting, and the final verification gate. Typed worker status, steering, cancellation, and shutdown are exposed through the shared runtime; dynamic model-driven team expansion remains a later promotion boundary.\n',
)

edit_once(
    "docs/ARCHITECTURE.md",
    '**Current boundary:** the shipped path supports the current single implementer contract. Autonomous nested delegation, model-driven dynamic team expansion, consensus voting, and distributed multi-worker transaction coordination remain outside the production entrypoint until separately promoted with behavioral and recovery proof.\n',
    '**Current boundary:** the shipped path supports the current single implementer contract and typed operator steering. Model-driven dynamic team expansion remains outside the production entrypoint until separately promoted with behavioral and recovery proof. Superseded autonomous, consensus, commit-barrier, and universal transaction authorities have been removed rather than retained as design-only code.\n',
)
edit_once(
    "docs/PRODUCTION-EXECUTION-TRACE.md",
    'Autonomous nested delegation, model-driven team expansion, consensus voting, commit barriers, and distributed multi-worker transaction coordination require separate production evidence. Their workspace crates are not the current integration authority.\n',
    'Model-driven nested team expansion beyond the current bounded contracts requires separate production evidence. Superseded autonomous, consensus, commit-barrier, and universal transaction authorities are not retained in the workspace.\n',
)
edit_once(
    "docs/CAPABILITY-EVIDENCE.md",
    'The current production capability supports one mutating implementer contract after parallel read-only preflight. Autonomous nested delegation, dynamic multi-implementer task creation, consensus voting, commit barriers, and distributed transaction coordination remain design-only until a production caller, recovery path, observability contract, and behavioral proof are merged. Their presence in the workspace must not be presented as active behavior.\n',
    'The current production capability supports one mutating implementer contract after parallel read-only preflight, with typed status, steering, cancellation, and shutdown. Model-driven nested team expansion remains unshipped until a production caller, recovery path, observability contract, and behavioral proof are merged. Superseded autonomous, consensus, commit-barrier, and universal transaction implementations are removed instead of being presented as dormant capability.\n',
)

Path("docs/DEAD-ORCHESTRATION-CLEANUP.md").write_text(
    """# Dead orchestration cleanup\n\nThe production coordinator now owns team tasks, leases, role-bound `AgentEngine` sessions, worktree mutation, integration, steering, cancellation, recovery, and completion evidence.\n\nThis cleanup removes the disconnected `AgentEngine` autonomous state machine and its public APIs, the orphan universal transaction pipeline, and standalone consensus, commit-barrier, conflict-resolution, and repository-rollback crates that had no production caller.\n\n`medusa-execution-orchestrator` remains the runtime lifecycle/checkpoint state model. `medusa-transaction-coordinator` remains where independently used by capability logic. `medusa-agent::transaction`, `WorkerExecutionController`, `medusa-workers`, and the scheduler remain because production callers and behavioral tests protect distinct invariants.\n"""
)

checker = CHECKER.read_text()
start = checker.index(HOOK_START)
end = checker.index(HOOK_END, start) + len(HOOK_END)
CHECKER.write_text(checker[:start] + checker[end:])
Path(__file__).unlink()
for marker in Path(".github").glob("dead-orchestration-marker-*"):
    marker.unlink()
subprocess.run(["cargo", "fmt", "--all"], check=True)
