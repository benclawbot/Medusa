use std::collections::BTreeSet;

use medusa_improvement::learning_review::{
    LearningKind, LearningReviewItem, LearningReviewState, LearningReviewStore,
};

#[test]
fn deletion_removes_retained_learning_content_but_keeps_auditability() {
    let repo = tempfile::tempdir().expect("repo");
    let store = LearningReviewStore::for_repository(repo.path());
    let item = LearningReviewItem {
        id: "delete-me".into(),
        revision: 1,
        state: LearningReviewState::Proposed,
        kind: LearningKind::UserPreference,
        title: "Detailed private preference".into(),
        source_signal_ids: vec!["signal".into()],
        evidence_digests: vec!["a".repeat(64)],
        root_cause: "private detail".into(),
        generalized_rule: "private generalized rule".into(),
        scope: "user".into(),
        confidence_milli: 800,
        proposed_solution: "memory".into(),
        non_applicable_contexts: Vec::new(),
        replay: None,
        conflicts_with: BTreeSet::new(),
        active_version: None,
        previous_version: None,
        created_at_unix_ms: 1,
        updated_at_unix_ms: 1,
    };
    let state = store.upsert(item, 0, "desktop").expect("upsert");
    let deleted = store
        .transition(
            "delete-me",
            LearningReviewState::Deleted,
            state.revision,
            "desktop",
            2,
        )
        .expect("delete");
    let item = &deleted.items[0];
    assert_eq!(item.state, LearningReviewState::Deleted);
    assert!(item.source_signal_ids.is_empty());
    assert!(item.root_cause.is_empty());
    assert_eq!(item.generalized_rule, "deleted");
    assert!(store.export().expect("audit").chain_valid);
}
