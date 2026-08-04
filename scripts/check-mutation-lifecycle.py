#!/usr/bin/env python3
"""Fail closed if production integration can move before dedicated review or verification."""

from pathlib import Path
import sys

root = Path(__file__).resolve().parents[1]
runtime = (root / "crates/medusa-runtime/src/lib.rs").read_text(encoding="utf-8")
coordinator = (root / "crates/medusa-runtime/src/mutating_worker_coordinator.rs").read_text(encoding="utf-8")
facade = (root / "crates/medusa-runtime/src/mutation_transaction.rs").read_text(encoding="utf-8")
transaction = (root / "crates/medusa-runtime/src/mutation_transaction_legacy.rs").read_text(encoding="utf-8")
reviewer = (root / "crates/medusa-runtime/src/parent_reviewer.rs").read_text(encoding="utf-8")
workers = (root / "crates/medusa-workers/src/lib.rs").read_text(encoding="utf-8")

errors: list[str] = []
if "integrate_prepared(" in coordinator or ".integrate_successful(" in coordinator:
    errors.append("mutating coordinator still integrates before parent review")

status_turn = runtime.find("engine.step_with_observer_and_context_and_turn_instruction")
provider = runtime.find("ConfiguredProvider::manager_from_config", status_turn)
completion = runtime.find("complete_after_parent_review", provider)
if status_turn < 0 or provider < status_turn or completion < provider:
    errors.append("dedicated transaction review is not ordered after the conversational status turn")
if "state.session_api_key.clone()" not in runtime or "cancel.as_ref()" not in runtime:
    errors.append("dedicated reviewer does not inherit active credential and cancellation authority")
if "implementation_evidence.as_ref().map" in runtime and "PARENT_REVIEW_TURN_INSTRUCTION" in runtime:
    errors.append("generic AgentEngine still receives the parent-review authority instruction")

if "AgentSession" in facade or "record_parent_review" in facade:
    errors.append("transaction facade still delegates authority to an AgentSession")
if "crate::parent_reviewer::complete(path, repo, provider, config, cancel, events)" not in facade:
    errors.append("transaction facade does not delegate to the dedicated reviewer")

ordered = [
    "record_review_decision(",
    "begin_verification()",
    "verify_independently(repo)",
    "authorize(repo)",
    "integrate(repo)",
    "reconcile(repo)",
]
positions = [reviewer.find(marker) for marker in ordered]
if any(position < 0 for position in positions) or positions != sorted(positions):
    errors.append(f"dedicated review lifecycle ordering is invalid: {dict(zip(ordered, positions))}")
if "tools: Vec::new()" not in reviewer:
    errors.append("dedicated parent reviewer advertises tools")
if "ResponseBlock::ToolUse" not in reviewer or "forbidden tool" not in reviewer:
    errors.append("dedicated parent reviewer does not fail closed on tool use")
if "parent-review-session.json" not in reviewer or "persist_journal" not in reviewer:
    errors.append("dedicated parent review lacks durable session evidence")

if "MutationLifecycle::IntegrationAuthorized" not in transaction:
    errors.append("integration is not gated by durable authorization")
if "pub fn integrate_authorized" not in workers:
    errors.append("worker manager lacks exact-commit authorized integration")
if "cleanup(std::slice::from_ref(&self.state.worker))" not in transaction:
    errors.append("transaction does not retain resources through reconciliation")

if errors:
    for error in errors:
        print(f"error: {error}")
    raise SystemExit(1)
print("transactional mutation lifecycle ordering is valid")
