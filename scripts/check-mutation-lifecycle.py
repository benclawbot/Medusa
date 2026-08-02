#!/usr/bin/env python3
"""Fail closed if production integration can move before review or verification."""

from pathlib import Path
import sys

root = Path(__file__).resolve().parents[1]
runtime = (root / "crates/medusa-runtime/src/lib.rs").read_text(encoding="utf-8")
coordinator = (root / "crates/medusa-runtime/src/mutating_worker_coordinator.rs").read_text(encoding="utf-8")
transaction = (root / "crates/medusa-runtime/src/mutation_transaction.rs").read_text(encoding="utf-8")
workers = (root / "crates/medusa-workers/src/lib.rs").read_text(encoding="utf-8")

errors: list[str] = []
if "integrate_prepared(" in coordinator or ".integrate_successful(" in coordinator:
    errors.append("mutating coordinator still integrates before parent review")
parent = runtime.find("engine.step_with_observer_and_context")
completion = runtime.find("complete_after_parent_review", parent)
if parent < 0 or completion < parent:
    errors.append("transaction completion is not ordered after parent model review")
ordered = [
    "record_parent_review(session)",
    "verify_independently(repo)",
    "authorize(repo)",
    "integrate(repo)",
    "reconcile(repo)",
]
positions = [transaction.find(marker) for marker in ordered]
if any(position < 0 for position in positions) or positions != sorted(positions):
    errors.append(f"transaction lifecycle ordering is invalid: {dict(zip(ordered, positions))}")
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
