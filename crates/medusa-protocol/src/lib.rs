//! Versioned wire and append-only event contracts.

use medusa_core::{
    CorrelationId, ErrorCategory, ErrorCode, EventId, MedusaError, MedusaResult, SessionId,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

/// Current wire protocol version.
pub const CURRENT_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };

/// Independently versioned wire protocol.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    /// Whether this consumer can accept a message produced by `peer`.
    #[must_use]
    pub const fn accepts(self, peer: Self) -> bool {
        self.major == peer.major && peer.minor <= self.minor
    }

    /// Enforces compatibility.
    pub fn ensure_compatible(self, peer: Self) -> MedusaResult<()> {
        if self.accepts(peer) {
            return Ok(());
        }
        Err(MedusaError::new(
            ErrorCode::IncompatibleProtocol,
            ErrorCategory::Validation,
            format!(
                "local protocol {}.{} cannot accept peer {}.{}",
                self.major, self.minor, peer.major, peer.minor
            ),
        ))
    }
}

/// Event actor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Actor {
    User,
    Coordinator,
    Worker(String),
    System(String),
}

/// Durable session state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Created,
    Bootstrapping,
    Understanding,
    Planning,
    Executing,
    Verifying,
    Reviewing,
    Learning,
    Completed,
    Blocked,
    Paused,
    CancelRequested,
    Cancelled,
    Crashed,
    Recovering,
    BudgetExhausted,
}

/// Typed append-only event payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum EventPayload {
    SessionCreated {
        objective: String,
    },
    SessionStateChanged {
        from: SessionState,
        to: SessionState,
    },
    UserPromptReceived {
        text: String,
    },
    GoalUpdated {
        objective: String,
    },
    ConversationCompacted {
        original_messages: u32,
        retained_messages: u32,
    },
    AssumptionRecorded {
        assumption: String,
        rationale: String,
    },
    PlanCreated {
        plan: Value,
    },
    PlanUpdated {
        update: Value,
    },
    ModelRequestStarted {
        provider: String,
        model: String,
    },
    ModelResponseReceived {
        response_id: Option<String>,
        usage: Value,
    },
    ToolCallRequested {
        tool: String,
        arguments: Value,
    },
    ToolCallDenied {
        tool: String,
        reason: String,
    },
    ToolExecutionStarted {
        tool: String,
    },
    ToolOutputChunk {
        artifact_ref: String,
        byte_count: u64,
    },
    ToolExecutionCompleted {
        tool: String,
        exit_code: Option<i32>,
    },
    FileTransactionCommitted {
        paths: Vec<String>,
        rollback_ref: String,
    },
    CheckpointCreated {
        checkpoint_id: String,
    },
    VerificationStarted {
        commands: Vec<String>,
    },
    VerificationCompleted {
        passed: bool,
        evidence: Vec<String>,
    },
    SessionPaused {
        reason: String,
    },
    SessionResumed,
    SessionCompleted {
        report_ref: String,
    },
    SessionFailed {
        error: MedusaError,
    },
}

/// Integrity-protected event envelope.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventEnvelope {
    pub protocol_version: ProtocolVersion,
    pub event_id: EventId,
    pub sequence: u64,
    pub session_id: SessionId,
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    pub actor: Actor,
    pub correlation_id: CorrelationId,
    pub payload: EventPayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_hash: Option<String>,
    pub checksum: String,
}

#[derive(Serialize)]
struct HashMaterial<'a> {
    protocol_version: ProtocolVersion,
    event_id: &'a EventId,
    sequence: u64,
    session_id: &'a SessionId,
    #[serde(with = "time::serde::rfc3339")]
    timestamp: OffsetDateTime,
    actor: &'a Actor,
    correlation_id: &'a CorrelationId,
    payload: &'a EventPayload,
    previous_hash: &'a Option<String>,
}

impl EventEnvelope {
    /// Creates a checksummed event.
    pub fn new(
        sequence: u64,
        session_id: SessionId,
        actor: Actor,
        correlation_id: CorrelationId,
        payload: EventPayload,
        previous_hash: Option<String>,
        timestamp: OffsetDateTime,
    ) -> MedusaResult<Self> {
        let mut event = Self {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            event_id: EventId::new(),
            sequence,
            session_id,
            timestamp,
            actor,
            correlation_id,
            payload,
            previous_hash,
            checksum: String::new(),
        };
        event.checksum = event.compute_checksum()?;
        Ok(event)
    }

    /// Computes the canonical SHA-256 checksum.
    pub fn compute_checksum(&self) -> MedusaResult<String> {
        let material = HashMaterial {
            protocol_version: self.protocol_version,
            event_id: &self.event_id,
            sequence: self.sequence,
            session_id: &self.session_id,
            timestamp: self.timestamp,
            actor: &self.actor,
            correlation_id: &self.correlation_id,
            payload: &self.payload,
            previous_hash: &self.previous_hash,
        };
        let bytes = serde_json::to_vec(&material).map_err(|error| {
            MedusaError::new(
                ErrorCode::InvalidEvent,
                ErrorCategory::Internal,
                format!("failed to serialize event checksum material: {error}"),
            )
        })?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }

    /// Verifies protocol compatibility and checksum integrity.
    pub fn validate(&self) -> MedusaResult<()> {
        CURRENT_PROTOCOL_VERSION.ensure_compatible(self.protocol_version)?;
        if self.compute_checksum()? == self.checksum {
            return Ok(());
        }
        Err(MedusaError::new(
            ErrorCode::ChecksumMismatch,
            ErrorCategory::Persistence,
            "event checksum does not match payload",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sample(objective: String) -> EventEnvelope {
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

    proptest! {
        #[test]
        fn event_schema_round_trips(objective in ".{0,256}") {
            let event = sample(objective);
            let json = serde_json::to_string(&event).expect("serialize");
            prop_assert_eq!(serde_json::from_str::<EventEnvelope>(&json).expect("deserialize"), event);
        }
    }

    #[test]
    fn tampering_is_detected() {
        let mut event = sample("original".into());
        event.payload = EventPayload::SessionCreated {
            objective: "tampered".into(),
        };
        assert_eq!(
            event.validate().expect_err("tamper").code,
            ErrorCode::ChecksumMismatch
        );
    }

    #[test]
    fn compatibility_rules_are_explicit() {
        assert!(
            ProtocolVersion { major: 1, minor: 3 }.accepts(ProtocolVersion { major: 1, minor: 2 })
        );
        assert!(
            !ProtocolVersion { major: 1, minor: 2 }.accepts(ProtocolVersion { major: 1, minor: 3 })
        );
        assert!(
            !ProtocolVersion { major: 1, minor: 9 }.accepts(ProtocolVersion { major: 2, minor: 0 })
        );
    }
}
