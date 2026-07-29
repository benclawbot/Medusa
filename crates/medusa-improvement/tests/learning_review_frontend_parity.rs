use medusa_improvement::learning_review::LearningReviewStore;

#[test]
fn both_frontends_read_the_same_authoritative_revision() {
    let repo = tempfile::tempdir().expect("repo");
    let desktop = LearningReviewStore::for_repository(repo.path())
        .snapshot()
        .expect("desktop snapshot");
    let tui = LearningReviewStore::for_repository(repo.path())
        .snapshot()
        .expect("tui snapshot");
    assert_eq!(desktop, tui);
}
