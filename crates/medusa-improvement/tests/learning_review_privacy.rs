use medusa_improvement::learning_review::{LearningPrivacy, LearningReviewStore};

#[test]
fn private_defaults_survive_restart_and_stale_updates_fail() {
    let repo = tempfile::tempdir().expect("repo");
    let store = LearningReviewStore::for_repository(repo.path());
    let initial = store.snapshot().expect("initial");
    assert!(initial.privacy.capture_enabled);
    assert!(!initial.privacy.user_persistence_enabled);
    assert!(!initial.privacy.cross_repository_reuse_enabled);
    assert!(!initial.privacy.telemetry_enabled);

    let updated = store
        .update_privacy(
            LearningPrivacy {
                capture_enabled: false,
                user_persistence_enabled: false,
                cross_repository_reuse_enabled: false,
                telemetry_enabled: false,
                automatic_proposals_enabled: false,
            },
            initial.revision,
            "desktop",
        )
        .expect("privacy update");
    let reopened = LearningReviewStore::for_repository(repo.path())
        .snapshot()
        .expect("restart");
    assert_eq!(reopened.privacy, updated.privacy);
    assert!(
        store
            .update_privacy(
                LearningPrivacy::private_by_default(),
                initial.revision,
                "stale-tui"
            )
            .is_err()
    );
}
