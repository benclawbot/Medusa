use medusa_core::{CorrelationId, ErrorCode, SessionId};
use medusa_protocol::{
    Actor, CURRENT_PROTOCOL_VERSION, EventEnvelope, EventPayload, ProtocolVersion, SessionAction,
    SessionActionDeliveryPolicy, SessionActionKind, SessionActionLifecycle,
    SessionActionWakePolicy,
};
use proptest::prelude::*;
use serde_json::json;
use time::OffsetDateTime;

fn sample_event(objective: String) -> EventEnvelope {
    EventEnvelope::new(
        1,
        SessionId::new(),
        Actor::Coordinator,
        CorrelationId::new(),
        EventPayload::SessionCreated { objective },
        None,
        OffsetDateTime::UNIX_EPOCH,
    )
    .expect("event")
}

fn lifecycle(index: u8) -> SessionActionLifecycle {
    match index % 8 {
        0 => SessionActionLifecycle::Queued,
        1 => SessionActionLifecycle::Selected,
        2 => SessionActionLifecycle::Preparing,
        3 => SessionActionLifecycle::Committing,
        4 => SessionActionLifecycle::Running,
        5 => SessionActionLifecycle::Completed,
        6 => SessionActionLifecycle::Failed,
        _ => SessionActionLifecycle::Cancelled,
    }
}

proptest! {
    #[test]
    fn arbitrary_wire_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        if let Ok(event) = serde_json::from_slice::<EventEnvelope>(&bytes) {
            let _ = event.validate();
        }
    }

    #[test]
    fn checksummed_events_reject_payload_tampering(
        original in ".{0,256}",
        replacement in ".{0,256}"
    ) {
        prop_assume!(original != replacement);
        let mut event = sample_event(original);
        event.payload = EventPayload::SessionCreated { objective: replacement };
        let error = event.validate().expect_err("tampered event must fail");
        prop_assert_eq!(error.code, ErrorCode::ChecksumMismatch);
    }

    #[test]
    fn terminal_action_lifecycle_never_reactivates(from in 0_u8..8, to in 0_u8..8) {
        let from = lifecycle(from);
        let to = lifecycle(to);
        if from.terminal() {
            prop_assert!(!from.can_transition_to(to));
        }
    }

    #[test]
    fn action_validation_rejects_missing_authority_fields(
        empty_field in 0_u8..4,
        expected_revision in any::<u64>()
    ) {
        let mut action = SessionAction {
            action_id: "action-1".to_owned(),
            idempotency_key: "idem-1".to_owned(),
            source: "tui:client-1".to_owned(),
            target_session_id: "session-1".to_owned(),
            expected_session_revision: expected_revision,
            kind: SessionActionKind::Steer,
            delivery_policy: SessionActionDeliveryPolicy::NextSafeTurnBoundary,
            wake_policy: SessionActionWakePolicy::OnBoundary,
            payload: json!({"text": "continue"}),
        };
        match empty_field {
            0 => action.action_id.clear(),
            1 => action.idempotency_key.clear(),
            2 => action.source.clear(),
            _ => action.target_session_id.clear(),
        }
        prop_assert!(action.validate().is_err());
    }

    #[test]
    fn protocol_compatibility_is_major_strict_and_minor_monotonic(
        local_major in any::<u16>(),
        local_minor in any::<u16>(),
        peer_major in any::<u16>(),
        peer_minor in any::<u16>()
    ) {
        let local = ProtocolVersion { major: local_major, minor: local_minor };
        let peer = ProtocolVersion { major: peer_major, minor: peer_minor };
        prop_assert_eq!(
            local.accepts(peer),
            local_major == peer_major && peer_minor <= local_minor
        );
    }
}

#[test]
fn malformed_known_event_shapes_fail_closed() {
    let valid = sample_event("objective".to_owned());
    let mut value = serde_json::to_value(valid).expect("serialize");

    value["unexpected_authority"] = json!(true);
    assert!(serde_json::from_value::<EventEnvelope>(value).is_err());

    let incompatible = ProtocolVersion {
        major: CURRENT_PROTOCOL_VERSION.major.saturating_add(1),
        minor: 0,
    };
    assert!(
        CURRENT_PROTOCOL_VERSION
            .ensure_compatible(incompatible)
            .is_err()
    );
}

#[test]
fn only_declared_action_lifecycle_edges_are_accepted() {
    use SessionActionLifecycle as L;

    let states = [
        L::Queued,
        L::Selected,
        L::Preparing,
        L::Committing,
        L::Running,
        L::Completed,
        L::Failed,
        L::Cancelled,
    ];
    let allowed = [
        (L::Queued, L::Selected),
        (L::Queued, L::Cancelled),
        (L::Selected, L::Preparing),
        (L::Selected, L::Cancelled),
        (L::Preparing, L::Committing),
        (L::Preparing, L::Failed),
        (L::Preparing, L::Cancelled),
        (L::Committing, L::Running),
        (L::Committing, L::Failed),
        (L::Running, L::Completed),
        (L::Running, L::Failed),
        (L::Running, L::Cancelled),
    ];

    for from in states {
        for to in states {
            assert_eq!(
                from.can_transition_to(to),
                allowed.contains(&(from, to)),
                "unexpected lifecycle edge {from:?} -> {to:?}"
            );
        }
    }
}
