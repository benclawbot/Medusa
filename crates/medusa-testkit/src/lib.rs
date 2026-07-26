//! Deterministic fixtures for Medusa tests.
//!
//! This crate is test support only. Production crates must depend on it only
//! through `[dev-dependencies]` so deterministic fixtures never ship in the
//! normal runtime dependency graph.

use medusa_core::{CorrelationId, ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use medusa_protocol::{Actor, EventEnvelope, EventPayload};
use time::{Duration, OffsetDateTime};

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

/// A manually advanced clock for deterministic timeout and recovery tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeterministicClock {
    now: OffsetDateTime,
}

impl Default for DeterministicClock {
    fn default() -> Self {
        Self::at_unix_epoch()
    }
}

impl DeterministicClock {
    /// Starts a clock at the Unix epoch.
    #[must_use]
    pub const fn at_unix_epoch() -> Self {
        Self {
            now: OffsetDateTime::UNIX_EPOCH,
        }
    }

    /// Starts a clock at an explicit instant.
    #[must_use]
    pub const fn new(now: OffsetDateTime) -> Self {
        Self { now }
    }

    /// Returns the current deterministic instant.
    #[must_use]
    pub const fn now(&self) -> OffsetDateTime {
        self.now
    }

    /// Advances the clock without consulting wall-clock time.
    pub fn advance(&mut self, duration: Duration) {
        self.now += duration;
    }
}

/// Collects protocol events and provides concise assertion helpers.
#[derive(Debug, Default, Clone)]
pub struct EventCollector {
    events: Vec<EventEnvelope>,
}

impl EventCollector {
    /// Records one event in emission order.
    pub fn push(&mut self, event: EventEnvelope) {
        self.events.push(event);
    }

    /// Returns all recorded events in emission order.
    #[must_use]
    pub fn events(&self) -> &[EventEnvelope] {
        &self.events
    }

    /// Returns the number of recorded events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Returns true when no events were recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Validates every recorded envelope.
    pub fn validate_all(&self) -> MedusaResult<()> {
        for event in &self.events {
            event.validate().map_err(|error| invalid_fixture("event", error))?;
        }
        Ok(())
    }
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

    #[test]
    fn deterministic_clock_advances_without_wall_time() {
        let mut clock = DeterministicClock::default();
        clock.advance(Duration::seconds(30));
        assert_eq!(clock.now(), OffsetDateTime::UNIX_EPOCH + Duration::seconds(30));
    }

    #[test]
    fn collector_preserves_order_and_validates_events() {
        let mut collector = EventCollector::default();
        collector.push(session_created_event("first").expect("first fixture"));
        collector.push(session_created_event("second").expect("second fixture"));

        assert_eq!(collector.len(), 2);
        collector.validate_all().expect("valid fixtures");
    }
}
