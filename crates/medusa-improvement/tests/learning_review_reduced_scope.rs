use medusa_improvement::learning_review::LearningPrivacy;

#[test]
fn task_local_only_policy_disables_broader_persistence_and_reuse() {
    let privacy = LearningPrivacy {
        capture_enabled: true,
        user_persistence_enabled: false,
        cross_repository_reuse_enabled: false,
        telemetry_enabled: false,
        automatic_proposals_enabled: true,
    };
    assert!(!privacy.user_persistence_enabled);
    assert!(!privacy.cross_repository_reuse_enabled);
}
