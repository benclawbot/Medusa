#!/usr/bin/env python3
"""Fail closed if production integration can move before dedicated review or verification."""

from pathlib import Path
import sys

root = Path(__file__).resolve().parents[1]
runtime = (root / "crates/medusa-runtime/src/lib.rs").read_text(encoding="utf-8")
coordinator = (root / "crates/medusa-runtime/src/coordination/mutating_worker_coordinator.rs").read_text(encoding="utf-8")
facade = (root / "crates/medusa-runtime/src/mutation_transaction.rs").read_text(encoding="utf-8")
transaction_path = root / "crates/medusa-runtime/src/mutation_transaction_state.rs"
transaction = transaction_path.read_text(encoding="utf-8")
reviewer = (root / "crates/medusa-runtime/src/parent_reviewer.rs").read_text(encoding="utf-8")
workers = (root / "crates/medusa-workers/src/lib.rs").read_text(encoding="utf-8")

errors: list[str] = []
if "integrate_prepared(" in coordinator or ".integrate_successful(" in coordinator:
    errors.append("mutating coordinator still integrates before parent review")

provider = runtime.find("ConfiguredProvider::manager_from_config")
completion = runtime.find("complete_after_parent_review", provider)
if provider < 0 or completion < provider:
    errors.append("dedicated transaction review is not connected to runtime completion")
if "let result = if implementation_evidence.is_some()" not in runtime:
    errors.append("prepared mutations still enter the generic conversational model loop")
if (
    "mutation_completion_text(" not in runtime
    or "EventPayload::AssistantMessageRecorded" not in runtime
    or "EventPayload::SessionCompleted" not in runtime
):
    errors.append("accepted mutations lack deterministic durable terminal completion")
if "state.session_api_key.clone()" not in runtime or "cancel.as_ref()" not in runtime:
    errors.append("dedicated reviewer does not inherit active credential and cancellation authority")
if "implementation_evidence.as_ref().map" in runtime and "PARENT_REVIEW_TURN_INSTRUCTION" in runtime:
    errors.append("generic AgentEngine still receives the parent-review authority instruction")

legacy_path = root / "crates/medusa-runtime/src/mutation_transaction_legacy.rs"
if legacy_path.exists():
    errors.append("quarantined legacy mutation transaction module still exists")
if "mutation_transaction_legacy" in facade or "mod legacy" in facade:
    errors.append("transaction facade still exposes a legacy compatibility module")
if "AgentSession" in transaction or "record_parent_review" in transaction or "latest_assistant_text" in transaction:
    errors.append("durable transaction state still contains conversational review authority")
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
