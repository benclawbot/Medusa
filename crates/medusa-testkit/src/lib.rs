//! Deterministic fixtures for Medusa tests.

#[allow(clippy::too_many_arguments)]
pub mod data_lifecycle;
pub mod resilience;

use medusa_core::{
    CorrelationId, ErrorCategory, ErrorCode, EventId, MedusaError, MedusaResult, SessionId,
};
use medusa_protocol::{Actor, EventEnvelope, EventPayload};
use time::OffsetDateTime;

/// Creates a deterministic, checksummed session-created event fixture.
pub fn session_created_event(objective: impl Into<String>) -> MedusaResult<EventEnvelope> {
    let session_id = SessionId::parse("ses-01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .map_err(|error| invalid_fixture("session", error))?;
    let correlation_id = CorrelationId::parse("cor-01ARZ3NDEKTSV4RRFFQ69G5FAW")
        .map_err(|error| invalid_fixture("correlation", error))?;
    let event_id = EventId::parse("evt-01ARZ3NDEKTSV4RRFFQ69G5FAX")
        .map_err(|error| invalid_fixture("event", error))?;

    let mut event = EventEnvelope::new(
        1,
        session_id,
        Actor::Coordinator,
        correlation_id,
        EventPayload::SessionCreated {
            objective: objective.into(),
        },
        None,
        OffsetDateTime::UNIX_EPOCH,
    )?;
    event.event_id = event_id;
    event.checksum = event.compute_checksum()?;
    Ok(event)
}

fn invalid_fixture(kind: &str, error: &str) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        format!("invalid deterministic {kind} fixture: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_validates() {
        session_created_event("fix bug")
            .expect("fixture")
            .validate()
            .expect("valid event");
    }
}
