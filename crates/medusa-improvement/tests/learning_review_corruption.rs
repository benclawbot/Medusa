use std::fs;

use medusa_improvement::learning_review::{
    LearningKind, LearningReviewItem, LearningReviewState, LearningReviewStore,
};

#[test]
fn tampered_audit_chain_blocks_export() {
    let repo = tempfile::tempdir().expect("repo");
    let store = LearningReviewStore::for_repository(repo.path());
    let item = LearningReviewItem {
        id: "tamper".into(),
        revision: 1,
        state: LearningReviewState::Proposed,
        kind: LearningKind::SessionFact,
        title: "Tamper fixture".into(),
        source_signal_ids: vec!["signal".into()],
        evidence_digests: vec!["a".repeat(64)],
        root_cause: "fixture".into(),
        generalized_rule: "fixture rule".into(),
        scope: "task".into(),
        confidence_milli: 700,
        proposed_solution: "session note".into(),
        non_applicable_contexts: Vec::new(),
        replay: None,
        conflicts_with: Default::default(),
        active_version: None,
        previous_version: None,
        created_at_unix_ms: 1,
        updated_at_unix_ms: 1,
    };
    store.upsert(item, 0, "test").expect("upsert");
    let audit = repo.path().join(".medusa/learning-review/audit.jsonl");
    let text = fs::read_to_string(&audit).expect("read audit");
    fs::write(&audit, text.replace("upsert", "tampered")).expect("tamper");
    assert!(store.export().is_err());
}
