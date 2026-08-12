use std::collections::BTreeSet;

use medusa_improvement::learning_review::{
    LearningKind, LearningReviewItem, LearningReviewState, LearningReviewStore,
};

#[test]
fn export_contains_digests_not_raw_multimodal_content() {
    let repo = tempfile::tempdir().expect("repo");
    let store = LearningReviewStore::for_repository(repo.path());
    let item = LearningReviewItem {
        id: "privacy-export".into(),
        revision: 1,
        state: LearningReviewState::Proposed,
        kind: LearningKind::RepositoryLearning,
        title: "Do not retain raw multimodal context".into(),
        source_signal_ids: vec!["signal-private".into()],
        evidence_digests: vec!["c".repeat(64)],
        root_cause: "raw session context is not required for the generalized rule".into(),
        generalized_rule: "retain only bounded generalized behavior and evidence digests".into(),
        scope: "repository".into(),
        confidence_milli: 900,
        proposed_solution: "redacted learning record".into(),
        non_applicable_contexts: Vec::new(),
        replay: None,
        conflicts_with: BTreeSet::new(),
        active_version: None,
        previous_version: None,
        created_at_unix_ms: 1,
        updated_at_unix_ms: 1,
    };
    store.upsert(item, 0, "test").expect("upsert");
    let json = serde_json::to_string(&store.export().expect("export")).expect("json");
    assert!(!json.contains("data:image/"));
    assert!(!json.contains("microphone transcript:"));
    assert!(!json.contains("audio transcript:"));
    assert!(json.contains(&"c".repeat(64)));
}
