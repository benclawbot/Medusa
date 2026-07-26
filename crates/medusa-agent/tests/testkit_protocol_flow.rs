use medusa_testkit::{session_created_event, EventCollector};

#[test]
fn agent_emits_valid_deterministic_session_event() {
    let mut events = EventCollector::default();
    events.push(session_created_event("apply approved patch").expect("fixture"));

    assert!(!events.is_empty());
    events.validate_all().expect("valid agent event sequence");
}
