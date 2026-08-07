use crate::session::{AgentSession, journal};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_protocol::{Actor, EventEnvelope, EventPayload};

pub(crate) mod execution_lane {
    include!("execution_lane.rs");
}

pub(crate) fn append_event(
    session: &mut AgentSession,
    actor: Actor,
    payload: EventPayload,
) -> MedusaResult<()> {
    journal::append_payload_committed(session, actor, payload)?;
    Ok(())
}

pub(crate) fn verify_chain(events: &[EventEnvelope]) -> MedusaResult<()> {
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
