//! Deterministic fixtures for Medusa tests.

use medusa_core::{CorrelationId, ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use medusa_protocol::{Actor, EventEnvelope, EventPayload};
use time::OffsetDateTime;

/// Creates a deterministic session-created event fixture.
pub fn session_created_event(objective: impl Into<String>) -> MedusaResult<EventEnvelope> {
    let session_id = SessionId::parse("ses-01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .map_err(|error| invalid_fixture("session", error))?;
    let correlation_id = CorrelationId::parse("cor-01ARZ3NDEKTSV4RRFFQ69G5FAW")
        .map_err(|error| invalid_fixture("correlation", error))?;

    EventEnvelope::new(
        1,
        session_id,
        Actor::Coordinator,
        correlation_id,
        EventPayload::SessionCreated {
            objective: objective.into(),
        },
        None,
        OffsetDateTime::UNIX_EPOCH,
    )
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
