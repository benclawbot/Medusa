//! Deterministic fixtures for Medusa tests.

use medusa_core::{CorrelationId, MedusaResult, SessionId};
use medusa_protocol::{Actor, EventEnvelope, EventPayload};
use time::OffsetDateTime;

/// Creates a deterministic session-created event fixture.
pub fn session_created_event(objective: impl Into<String>) -> MedusaResult<EventEnvelope> {
    EventEnvelope::new(
        1,
        SessionId::parse("ses-01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("session fixture"),
        Actor::Coordinator,
        CorrelationId::parse("cor-01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("correlation fixture"),
        EventPayload::SessionCreated {
            objective: objective.into(),
        },
        None,
        OffsetDateTime::UNIX_EPOCH,
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
