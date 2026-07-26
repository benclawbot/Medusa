use medusa_testkit::{session_created_event, DeterministicClock, EventCollector};
use time::Duration;

#[test]
fn daemon_recovery_fixture_uses_explicit_time() {
    let mut clock = DeterministicClock::default();
    let mut events = EventCollector::default();

    events.push(session_created_event("recover interrupted session").expect("fixture"));
    clock.advance(Duration::minutes(2));

    assert_eq!(clock.now(), time::OffsetDateTime::UNIX_EPOCH + Duration::minutes(2));
    events.validate_all().expect("valid recovery event sequence");
}
