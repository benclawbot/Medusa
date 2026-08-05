#!/usr/bin/env python3
"""Delete the obsolete conversational parent-review compatibility authority."""

from __future__ import annotations

import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LEGACY = ROOT / "crates/medusa-runtime/src/mutation_transaction_legacy.rs"
STATE = ROOT / "crates/medusa-runtime/src/mutation_transaction_state.rs"
FACADE = ROOT / "crates/medusa-runtime/src/mutation_transaction.rs"
REVIEWER = ROOT / "crates/medusa-runtime/src/parent_reviewer.rs"
LIFECYCLE_GUARD = ROOT / "scripts/check-mutation-lifecycle.py"
EVIDENCE_GUARD = ROOT / "scripts/check-evidence-authority.py"
DELETION_DOC = ROOT / "docs/architecture/LEGACY-DELETION.md"
WORKFLOW = ROOT / ".github/workflows/pr-654-delete-review-compat.yml"
SCRIPT = Path(__file__).resolve()


def run(*args: str) -> None:
    subprocess.run(args, cwd=ROOT, check=True)


def replace_once(source: str, old: str, new: str, label: str) -> str:
    count = source.count(old)
    if count != 1:
        raise RuntimeError(f"expected exactly one {label} match, found {count}")
    return source.replace(old, new, 1)


def main() -> None:
    if not LEGACY.is_file() or STATE.exists():
        raise RuntimeError("unexpected mutation transaction migration state")

    state = LEGACY.read_text(encoding="utf-8")
    state = replace_once(
        state,
        "//! Durable review-before-integration transaction for every production mutation.",
        "//! Authoritative durable state machine for every production mutation.",
        "state module documentation",
    )
    state = replace_once(
        state,
        "use medusa_agent::{AgentSession, authoritative_verification_for_components_at};",
        "use medusa_agent::authoritative_verification_for_components_at;",
        "AgentSession import",
    )
    state = replace_once(
        state,
        "use medusa_provider::{MessageBlock, Role};\n",
        "",
        "conversational provider imports",
    )
    state = replace_once(
        state,
        "use medusa_review_model::{\n    PARENT_REVIEW_RESPONSE_REQUIREMENT, ParentReviewOutcome, ParentReviewResponse,\n    ParentReviewResponseError, final_parent_review_line, validate_parent_review_response,\n};",
        "use medusa_review_model::PARENT_REVIEW_RESPONSE_REQUIREMENT;",
        "legacy review parser imports",
    )
    state = replace_once(
        state,
        '''    pub fn record_parent_review(
        &mut self,
        session: &AgentSession,
    ) -> Result<ParentReviewDecision, String> {
        let text = latest_assistant_text(session)
            .ok_or_else(|| "parent reviewer produced no assistant text".to_owned())?;
        let outcome = decode_parent_review_response(&text)?;
        self.record_review_decision(outcome.decision, outcome.rationale, session.id.as_str())
    }

''',
        "",
        "AgentSession review adapter",
    )
    state = replace_once(
        state,
        '''pub fn complete_after_parent_review(
    path: &Path,
    repo: &Path,
    session: &AgentSession,
    events: &Sender<RuntimeEvent>,
) -> Result<TransactionCompletion, String> {
    let mut transaction = MutationTransaction::open(path)?;
    match transaction.record_parent_review(session)? {
        ParentReviewDecision::RevisionRequested => {
            let rationale = transaction
                .state
                .review
                .as_ref()
                .map(|receipt| receipt.rationale.clone())
                .unwrap_or_else(|| "parent requested revision".to_owned());
            transaction.emit(events);
            Ok(TransactionCompletion::RevisionRequested(rationale))
        }
        ParentReviewDecision::Accepted => {
            transaction.emit(events);
            transaction.begin_verification()?;
            transaction.emit(events);
            transaction.verify_independently(repo)?;
            transaction.emit(events);
            transaction.authorize(repo)?;
            transaction.emit(events);
            transaction.integrate(repo)?;
            transaction.emit(events);
            let receipt = transaction.reconcile(repo)?;
            transaction.emit(events);
            Ok(TransactionCompletion::Reconciled(receipt))
        }
    }
}

''',
        "",
        "conversational completion adapter",
    )
    state = replace_once(
        state,
        '''fn latest_assistant_text(session: &AgentSession) -> Option<String> {
    session.messages.iter().rev().find_map(|message| {
        if message.role != Role::Assistant {
            return None;
        }
        let text = message
            .content
            .iter()
            .filter_map(|block| match block {
                MessageBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\\n");
        (!text.trim().is_empty()).then_some(text)
    })
}

fn decode_parent_review_response(text: &str) -> Result<ParentReviewOutcome, String> {
    let final_line = final_parent_review_line(text).map_err(|error| error.to_string())?;
    let response: ParentReviewResponse = serde_json::from_str(final_line).map_err(|error| {
        ParentReviewResponseError::InvalidEnvelope(error.to_string()).to_string()
    })?;
    validate_parent_review_response(response, final_line).map_err(|error| error.to_string())
}

''',
        "",
        "legacy conversational parser helpers",
    )
    state = replace_once(
        state,
        '''    #[test]
    fn typed_parent_review_envelope_is_required_at_runtime_boundary() {
        let accepted = decode_parent_review_response(
            "The prepared patch is correct.\\n{\\\"schema_version\\\":1,\\\"decision\\\":\\\"accepted\\\",\\\"rationale\\\":\\\"exact patch and evidence agree\\\"}",
        )
        .expect("typed response");
        assert_eq!(accepted.decision, ParentReviewDecision::Accepted);
        assert_eq!(accepted.response_fingerprint.len(), 64);

        assert!(decode_parent_review_response(
            "MEDUSA_REVIEW_ACCEPTED: exact patch and evidence agree"
        )
        .is_err());
        assert!(decode_parent_review_response(
            "{\\\"schema_version\\\":1,\\\"decision\\\":\\\"accepted\\\",\\\"rationale\\\":\\\"ok\\\",\\\"extra\\\":true}"
        )
        .is_err());
        assert!(decode_parent_review_response(
            "{\\\"schema_version\\\":1,\\\"decision\\\":\\\"accepted\\\",\\\"rationale\\\":\\\"ok\\\"}\\ntrailing"
        )
        .is_err());
    }

''',
        "",
        "legacy parser boundary test",
    )
    STATE.write_text(state, encoding="utf-8")
    LEGACY.unlink()

    facade = FACADE.read_text(encoding="utf-8")
    facade = replace_once(
        facade,
        '''#[allow(dead_code)]
#[path = "mutation_transaction_legacy.rs"]
mod legacy;

pub use legacy::*;
''',
        '''#[path = "mutation_transaction_state.rs"]
mod state;

pub use state::*;
''',
        "transaction state module facade",
    )
    FACADE.write_text(facade, encoding="utf-8")

    reviewer = REVIEWER.read_text(encoding="utf-8")
    reviewer = replace_once(
        reviewer,
        '''    #[test]
    fn tool_use_fails_closed_and_is_durable() {
''',
        '''    #[test]
    fn typed_parent_review_envelope_is_required_at_runtime_boundary() {
        let invalid = [
            "MEDUSA_REVIEW_ACCEPTED: exact patch and evidence agree",
            "{\\\"schema_version\\\":1,\\\"decision\\\":\\\"accepted\\\",\\\"rationale\\\":\\\"ok\\\",\\\"extra\\\":true}",
            "{\\\"schema_version\\\":1,\\\"decision\\\":\\\"accepted\\\",\\\"rationale\\\":\\\"ok\\\"}\\ntrailing",
        ];
        for response in invalid {
            let root = tempdir().expect("temporary journal");
            let provider = FakeProvider::text(response);
            let error = review_packet(
                &provider,
                &Config::default(),
                &AtomicBool::new(false),
                &packet(root.path()),
            )
            .expect_err("invalid review response must fail closed");
            assert!(error.contains("review response"));
            let journal = load_journal(&packet(root.path()).journal_path)
                .expect("journal read")
                .expect("journal");
            assert_eq!(journal.state, ReviewJournalState::Failed);
        }
    }

    #[test]
    fn tool_use_fails_closed_and_is_durable() {
''',
        "dedicated reviewer parser regression test",
    )
    REVIEWER.write_text(reviewer, encoding="utf-8")

    lifecycle = LIFECYCLE_GUARD.read_text(encoding="utf-8")
    lifecycle = replace_once(
        lifecycle,
        'transaction = (root / "crates/medusa-runtime/src/mutation_transaction_legacy.rs").read_text(encoding="utf-8")',
        'transaction_path = root / "crates/medusa-runtime/src/mutation_transaction_state.rs"\ntransaction = transaction_path.read_text(encoding="utf-8")',
        "lifecycle guard transaction path",
    )
    lifecycle = replace_once(
        lifecycle,
        'if "mutation_completion_text(" not in runtime or "EventPayload::AssistantMessageRecorded" not in runtime:\n    errors.append("accepted mutations lack a deterministic durable completion response")',
        'if (\n    "mutation_completion_text(" not in runtime\n    or "EventPayload::AssistantMessageRecorded" not in runtime\n    or "EventPayload::SessionCompleted" not in runtime\n):\n    errors.append("accepted mutations lack deterministic durable terminal completion")',
        "durable terminal completion guard",
    )
    lifecycle = replace_once(
        lifecycle,
        'if "AgentSession" in facade or "record_parent_review" in facade:\n    errors.append("transaction facade still delegates authority to an AgentSession")',
        'legacy_path = root / "crates/medusa-runtime/src/mutation_transaction_legacy.rs"\nif legacy_path.exists():\n    errors.append("quarantined legacy mutation transaction module still exists")\nif "mutation_transaction_legacy" in facade or "mod legacy" in facade:\n    errors.append("transaction facade still exposes a legacy compatibility module")\nif "AgentSession" in transaction or "record_parent_review" in transaction or "latest_assistant_text" in transaction:\n    errors.append("durable transaction state still contains conversational review authority")',
        "legacy review reintroduction guards",
    )
    LIFECYCLE_GUARD.write_text(lifecycle, encoding="utf-8")

    evidence = EVIDENCE_GUARD.read_text(encoding="utf-8")
    evidence = replace_once(
        evidence,
        '"independent verification uses authority": "authoritative_verification_for_components_at" in read("crates/medusa-runtime/src/mutation_transaction_legacy.rs"),',
        '"independent verification uses authority": "authoritative_verification_for_components_at" in read("crates/medusa-runtime/src/mutation_transaction_state.rs"),',
        "evidence guard transaction path",
    )
    EVIDENCE_GUARD.write_text(evidence, encoding="utf-8")

    document = DELETION_DOC.read_text(encoding="utf-8")
    document = replace_once(document, "- [ ] #632:", "- [x] #632:", "#632 checklist status")
    document = replace_once(
        document,
        "- Remaining #632 deletion target: remove the quarantined compatibility parser after recovery fixtures migrate to the dedicated journal path.",
        "- The quarantined conversational parser, `AgentSession` review adapter, duplicate completion helper, legacy module name, and free-form marker boundary test have been deleted. The surviving durable mutation state machine is explicitly named `mutation_transaction_state.rs`; typed response validation and failure journals are owned only by the dedicated reviewer.",
        "#632 final deletion receipt",
    )
    DELETION_DOC.write_text(document, encoding="utf-8")

    run("cargo", "fmt", "--all")
    run("cargo", "test", "-p", "medusa-runtime", "--", "--nocapture")
    run("cargo", "clippy", "-p", "medusa-runtime", "--all-targets", "--", "-D", "warnings")
    run("python3", "scripts/check-mutation-lifecycle.py")
    run("python3", "scripts/check-evidence-authority.py")

    forbidden = {
        "legacy file": LEGACY.exists(),
        "legacy facade path": "mutation_transaction_legacy" in FACADE.read_text(encoding="utf-8"),
        "AgentSession state authority": "AgentSession" in STATE.read_text(encoding="utf-8"),
        "conversation review adapter": "record_parent_review" in STATE.read_text(encoding="utf-8"),
        "assistant parser": "latest_assistant_text" in STATE.read_text(encoding="utf-8"),
    }
    present = [name for name, found in forbidden.items() if found]
    if present:
        raise RuntimeError(f"legacy review authority remains: {present}")

    WORKFLOW.unlink(missing_ok=True)
    SCRIPT.unlink(missing_ok=True)
    run("git", "add", "-A")
    run("git", "diff", "--cached", "--check")
    run("git", "commit", "-m", "Delete conversational review compatibility")
    run("git", "push", "origin", "HEAD:agent/654-delete-conversational-review-compat")


if __name__ == "__main__":
    main()
