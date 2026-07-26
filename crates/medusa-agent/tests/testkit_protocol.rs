use medusa_testkit::session_created_event;

#[test]
fn deterministic_session_event_is_stable_for_agent_replay() {
    let first = session_created_event("index repository").expect("first fixture");
    let second = session_created_event("index repository").expect("second fixture");

    first.validate().expect("first event is valid");
    second.validate().expect("second event is valid");

    let first_json = serde_json::to_string(&first).expect("serialize first event");
    let second_json = serde_json::to_string(&second).expect("serialize second event");
    assert_eq!(
        first_json, second_json,
        "fixture output must be deterministic"
    );
}
