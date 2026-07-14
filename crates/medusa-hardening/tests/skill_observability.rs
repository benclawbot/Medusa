use medusa_hardening::Observability;

#[test]
fn observability_records_skill_match_increment() {
    let directory = tempfile::tempdir().expect("tempdir");
    let obs = Observability::new(directory.path()).expect("observability");
    obs.record_skill_match("brainstorming").expect("match 1");
    obs.record_skill_match("brainstorming").expect("match 2");
    obs.record_skill_inject("brainstorming").expect("inject");
    let snapshot = obs.snapshot().expect("snapshot");
    assert_eq!(snapshot["counters"]["skill.match.brainstorming"], 2);
    assert_eq!(snapshot["counters"]["skill.inject.brainstorming"], 1);
    assert!(
        snapshot["counters"]
            .get("skill.handoff.brainstorming")
            .is_none()
    );
}

#[test]
fn observability_records_skill_handoff() {
    let directory = tempfile::tempdir().expect("tempdir");
    let obs = Observability::new(directory.path()).expect("observability");
    obs.record_skill_handoff("writing-plans").expect("handoff");
    let snapshot = obs.snapshot().expect("snapshot");
    assert_eq!(snapshot["counters"]["skill.handoff.writing_plans"], 1);
}
