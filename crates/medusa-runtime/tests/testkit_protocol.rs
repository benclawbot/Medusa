use medusa_testkit::session_created_event;

#[test]
fn deterministic_session_event_survives_runtime_transport_roundtrip() {
    let event = session_created_event("repair workspace").expect("deterministic fixture");
    event.validate().expect("fixture must be protocol-valid");

    let encoded = serde_json::to_vec(&event).expect("serialize event");
    let decoded: medusa_protocol::EventEnvelope =
        serde_json::from_slice(&encoded).expect("deserialize event");

    decoded
        .validate()
        .expect("round-tripped event remains valid");
    assert_eq!(decoded.sequence, 1);
}
