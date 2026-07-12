use medusa_core::{CorrelationId, ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_protocol::{Actor, EventEnvelope, EventPayload};
use time::OffsetDateTime;

use crate::session::AgentSession;

pub(crate) fn append_event(
    session: &mut AgentSession,
    actor: Actor,
    payload: EventPayload,
) -> MedusaResult<()> {
    let previous_hash = session.events.last().map(|event| event.checksum.clone());
    let event = EventEnvelope::new(
        session.events.len() as u64 + 1,
        session.id.clone(),
        actor,
        CorrelationId::new(),
        payload,
        previous_hash,
        OffsetDateTime::now_utc(),
    )?;
    session.events.push(event);
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
