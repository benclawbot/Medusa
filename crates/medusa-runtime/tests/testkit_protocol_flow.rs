use medusa_testkit::{session_created_event, DeterministicClock, EventCollector};
use time::Duration;

#[test]
fn runtime_protocol_flow_is_deterministic() {
    let mut clock = DeterministicClock::default();
    let mut events = EventCollector::default();

    events.push(session_created_event("resume verified runtime turn").expect("fixture"));
    clock.advance(Duration::seconds(5));

    assert_eq!(events.len(), 1);
    assert_eq!(clock.now(), time::OffsetDateTime::UNIX_EPOCH + Duration::seconds(5));
    events.validate_all().expect("valid protocol flow");
}
