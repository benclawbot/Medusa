use medusa_improvement::learning_review::LearningReviewStore;

#[test]
fn review_and_export_require_no_network_service() {
    let repo = tempfile::tempdir().expect("repo");
    let store = LearningReviewStore::for_repository(repo.path());
    let snapshot = store.snapshot().expect("local snapshot");
    assert!(snapshot.items.is_empty());
    let export = store.export().expect("local export");
    assert!(export.chain_valid);
}
