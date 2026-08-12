use std::collections::BTreeSet;

use medusa_improvement::learning_review::{
    LearningKind, LearningReviewItem, LearningReviewState, LearningReviewStore, ReplaySummary,
};

fn item() -> LearningReviewItem {
    LearningReviewItem {
        id: "lesson-restart".into(),
        revision: 1,
        state: LearningReviewState::Proposed,
        kind: LearningKind::RepositoryLearning,
        title: "Verify before completion".into(),
        source_signal_ids: vec!["signal-restart".into()],
        evidence_digests: vec!["a".repeat(64)],
        root_cause: "completion was reported before verification".into(),
        generalized_rule: "verify authoritative checks before reporting completion".into(),
        scope: "repository".into(),
        confidence_milli: 950,
        proposed_solution: "add a completion verification gate".into(),
        non_applicable_contexts: vec!["explicit brainstorming".into()],
        replay: Some(ReplaySummary {
            reproduced: true,
            resolved: true,
            regression_count: 0,
            evidence_digests: vec!["b".repeat(64)],
        }),
        conflicts_with: BTreeSet::new(),
        active_version: None,
        previous_version: None,
        created_at_unix_ms: 1,
        updated_at_unix_ms: 1,
    }
}

#[test]
fn approved_activation_survives_restart_and_rolls_back() {
    let repo = tempfile::tempdir().expect("repo");
    let store = LearningReviewStore::for_repository(repo.path());
    let mut state = store.upsert(item(), 0, "desktop").expect("proposal");
    state = store
        .transition(
            "lesson-restart",
            LearningReviewState::Approved,
            state.revision,
            "desktop",
            2,
        )
        .expect("approve");
    state = store
        .transition(
            "lesson-restart",
            LearningReviewState::Validated,
            state.revision,
            "tui",
            3,
        )
        .expect("validate");
    state = store
        .transition(
            "lesson-restart",
            LearningReviewState::Active,
            state.revision,
            "desktop",
            4,
        )
        .expect("activate");

    let reopened = LearningReviewStore::for_repository(repo.path());
    let after_restart = reopened.snapshot().expect("restart snapshot");
    assert_eq!(after_restart.items[0].state, LearningReviewState::Active);
    assert_eq!(
        after_restart.items[0].active_version,
        state.items[0].active_version
    );

    let rolled_back = reopened
        .transition(
            "lesson-restart",
            LearningReviewState::RolledBack,
            after_restart.revision,
            "tui",
            5,
        )
        .expect("rollback");
    assert_eq!(rolled_back.items[0].state, LearningReviewState::RolledBack);
    assert!(reopened.export().expect("audit export").chain_valid);
}
