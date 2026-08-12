#!/usr/bin/env python3
from pathlib import Path

root = Path(__file__).resolve().parents[1]

def read(relative: str) -> str:
    return (root / relative).read_text(encoding="utf-8")

checks = {
    "typed evidence crate": "pub struct EvidenceRecord" in read("crates/medusa-evidence/src/evidence.rs"),
    "content-addressed artifact store": "pub struct ArtifactStore" in read("crates/medusa-evidence/src/artifact.rs"),
    "durable read receipts": "ArtifactReadReceipt" in read("crates/medusa-evidence/src/artifact.rs"),
    "exact changed components": "pub struct ChangedComponent" in read("crates/medusa-evidence/src/change.rs"),
    "planner selects browser behavior": "BrowserBehavior" in read("crates/medusa-evidence/src/verification.rs"),
    "planner selects accessibility": "Accessibility" in read("crates/medusa-evidence/src/verification.rs"),
    "command outputs become receipts": "CommandReceipt::new" in read("crates/medusa-agent/src/verification_authority_legacy.rs"),
    "trusted exact-file formatting precedes verification": "prepare_components_for_verification" in read("crates/medusa-runtime/src/mutating_worker_coordinator.rs"),
    "browser verification is mandatory": "required_browser_verification" in read("crates/medusa-agent/src/verification.rs"),
    "accessibility behavior is inspected": "unlabeled_controls" in read("crates/medusa-agent/src/verification.rs"),
    "worker preserves git change kinds": "commit_changed_components" in read("crates/medusa-workers/src/lib.rs"),
    "isolated implementation uses authority": "authoritative_verification_for_components_at" in read("crates/medusa-runtime/src/mutating_worker_coordinator.rs"),
    "independent verification uses authority": "authoritative_verification_for_components_at" in read("crates/medusa-runtime/src/mutation_transaction_state.rs"),
    "dedicated review enters independent verification": "verify_independently(repo)" in read("crates/medusa-runtime/src/parent_reviewer.rs"),
    "scheduler validates evidence dependencies": "succeed_with_evidence" in read("crates/medusa-multi-agent-scheduler/src/lib.rs"),
    "coarse verifier deleted": "targeted_verification" not in read("crates/medusa-agent/src/verification.rs"),
    "changed-path-loss fixture deleted": "isolated-verification-drops-changed-paths" not in read("scripts/architecture-conformance.py"),
}
failed = [name for name, passed in checks.items() if not passed]
for name, passed in checks.items():
    print(f"{'passed' if passed else 'failed'}: {name}")
if failed:
    raise SystemExit(f"evidence authority drift: {failed}")
