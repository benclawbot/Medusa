use std::collections::BTreeSet;

use medusa_improvement::learning_review::{
    LearningKind, LearningReviewItem, LearningReviewState, LearningReviewStore, ReplaySummary,
};

#[test]
fn conflicting_learning_cannot_activate_silently() {
    let repo = tempfile::tempdir().expect("repo");
    let store = LearningReviewStore::for_repository(repo.path());
    let item = LearningReviewItem {
        id: "conflict-a".into(),
        revision: 1,
        state: LearningReviewState::Proposed,
        kind: LearningKind::UserPreference,
        title: "Conflicting preference".into(),
        source_signal_ids: vec!["signal-a".into()],
        evidence_digests: vec!["a".repeat(64)],
        root_cause: "feedback conflicts with an active preference".into(),
        generalized_rule: "always ask before running checks".into(),
        scope: "user".into(),
        confidence_milli: 900,
        proposed_solution: "review the conflicting preference".into(),
        non_applicable_contexts: Vec::new(),
        replay: Some(ReplaySummary {
            reproduced: true,
            resolved: true,
            regression_count: 0,
            evidence_digests: vec!["b".repeat(64)],
        }),
        conflicts_with: BTreeSet::from(["conflict-b".into()]),
        active_version: None,
        previous_version: None,
        created_at_unix_ms: 1,
        updated_at_unix_ms: 1,
    };
    let mut state = store.upsert(item, 0, "desktop").expect("proposal");
    state = store
        .transition(
            "conflict-a",
            LearningReviewState::Approved,
            state.revision,
            "desktop",
            2,
        )
        .expect("approve");
    state = store
        .transition(
            "conflict-a",
            LearningReviewState::Validated,
            state.revision,
            "tui",
            3,
        )
        .expect("validate");
    assert!(
        store
            .transition(
                "conflict-a",
                LearningReviewState::Active,
                state.revision,
                "desktop",
                4
            )
            .is_err()
    );
    assert_ne!(
        store.snapshot().expect("snapshot").items[0].state,
        LearningReviewState::Active
    );
}
