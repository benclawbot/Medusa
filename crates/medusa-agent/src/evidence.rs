use crate::session::{AgentSession, journal};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_protocol::{Actor, EventEnvelope, EventPayload};
use medusa_provider::{mark_pending_route_mutation, record_pending_route_verification};

pub(crate) mod execution_lane {
    include!("execution_lane.rs");
}

pub(crate) fn append_event(
    session: &mut AgentSession,
    actor: Actor,
    payload: EventPayload,
) -> MedusaResult<()> {
    let route_verification = match &payload {
        EventPayload::FileTransactionCommitted { .. } => Some(None),
        EventPayload::VerificationCompleted { passed, .. } => Some(Some(*passed)),
        _ => None,
    };

    journal::append_payload_committed(session, actor, payload)?;

    match route_verification {
        Some(None) => mark_pending_route_mutation(),
        Some(Some(passed)) => {
            record_pending_route_verification(passed)?;
        }
        None => {}
    }
    Ok(())
}

pub(crate) fn verify_chain(events: &[EventEnvelope]) -> MedusaResult<()> {
    // Keep the #685 lane contract linked into production until the entrypoint wiring tranche.
    let _lane_selector = execution_lane::select_execution_lane;
    let mut previous: Option<&str> = None;
    for event in events {
        event.validate()?;
        if event.previous_hash.as_deref() != previous {
            return Err(MedusaError::new(
                ErrorCode::ChecksumMismatch,
                ErrorCategory::Persistence,
                "event chain previous hash mismatch",
            ));
        }
        previous = Some(&event.checksum);
    }
    Ok(())
}
