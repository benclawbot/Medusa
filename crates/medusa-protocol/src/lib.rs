//! Versioned wire and append-only event contracts.

use medusa_core::{
    CorrelationId, ErrorCategory, ErrorCode, EventId, MedusaError, MedusaResult, SessionId,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

/// Current wire protocol version.
pub const CURRENT_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion { major: 1, minor: 4 };

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

/// Durable operator action kind. New variants require an explicit protocol version change.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionActionKind {
    Steer,
    FollowUp,
    ReplaceFollowUp,
    Cancel,
    GoalAdjustment,
}

/// Boundary at which an accepted session action may become authoritative context.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionActionDeliveryPolicy {
    NextSafeTurnBoundary,
    WhenIdle,
}

/// Whether an accepted action should wake a dormant runtime.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionActionWakePolicy {
    Immediate,
    OnBoundary,
    ExternalResume,
}

/// Journaled action lifecycle. Delivery completion means the action itself has been durably
/// applied; it does not imply that the resulting agent turn or repository task is complete.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionActionLifecycle {
    Queued,
    Selected,
    Preparing,
    Committing,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl SessionActionLifecycle {
    /// Whether this lifecycle state is terminal for the action delivery operation.
    #[must_use]
    pub const fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Validates the monotonic delivery lifecycle. A committing action may never silently roll
    /// back to queued; recovery must append evidence and choose a terminal/retry action explicitly.
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Queued, Self::Selected)
                | (Self::Queued, Self::Cancelled)
                | (Self::Selected, Self::Preparing)
                | (Self::Selected, Self::Cancelled)
                | (Self::Preparing, Self::Committing)
                | (Self::Preparing, Self::Failed)
                | (Self::Preparing, Self::Cancelled)
                | (Self::Committing, Self::Running)
                | (Self::Committing, Self::Failed)
                | (Self::Running, Self::Completed)
                | (Self::Running, Self::Failed)
                | (Self::Running, Self::Cancelled)
        )
    }
}

/// Immutable action admission record. `payload` is kind-specific but remains bound to this typed
/// envelope and cannot grant authority beyond the attached session/client that admitted it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionAction {
    pub action_id: String,
    pub idempotency_key: String,
    pub source: String,
    pub target_session_id: String,
    pub expected_session_revision: u64,
    pub kind: SessionActionKind,
    pub delivery_policy: SessionActionDeliveryPolicy,
    pub wake_policy: SessionActionWakePolicy,
    pub payload: Value,
}

impl SessionAction {
    /// Structural validation shared by every admission entrypoint.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.action_id.trim().is_empty() {
            return Err("session action id cannot be empty");
        }
        if self.idempotency_key.trim().is_empty() {
            return Err("session action idempotency key cannot be empty");
        }
        if self.source.trim().is_empty() {
            return Err("session action source cannot be empty");
        }
        if self.target_session_id.trim().is_empty() {
            return Err("session action target session cannot be empty");
        }
        Ok(())
    }
}

/// Typed append-only event payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum EventPayload {
    SessionCreated {
        objective: String,
    },
    /// Immutable, redacted runtime configuration bound to a session generation.
    RuntimeConfigurationBound {
        schema_version: u16,
        fingerprint: String,
        snapshot: Value,
    },
    SessionStateChanged {
        from: SessionState,
        to: SessionState,
    },
    UserPromptReceived {
        text: String,
    },
    UserFollowupQueued {
        command_id: String,
        prompt: Value,
    },
    UserFollowupDequeued {
        command_id: String,
        text: String,
    },
    SessionActionAccepted {
        action: SessionAction,
    },
    SessionActionRejected {
        action: SessionAction,
        authoritative_revision: u64,
        reason: String,
    },
    SessionActionLifecycleChanged {
        action_id: String,
        from: SessionActionLifecycle,
        to: SessionActionLifecycle,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence: Option<Value>,
    },
    SessionActionTranscriptLinked {
        action_id: String,
        transcript_event_sequence: u64,
    },
    GoalUpdated {
        objective: String,
    },
    ConversationCompacted {
        original_messages: u32,
        retained_messages: u32,
        #[serde(default)]
        generation: u32,
        #[serde(default)]
        source_event_sequences: Vec<u64>,
        #[serde(default)]
        preserved_sections: Vec<String>,
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
    QuestionRequested {
        question: Value,
    },
    ApprovalRequested {
        request: Value,
    },
    ApprovalDecisionRecorded {
        decision: Value,
    },
    AssistantMessageRecorded {
        message: Value,
    },
    TeamStateChanged {
        snapshot: Value,
    },
    WorkerEvidenceRecorded {
        evidence: Value,
    },
    IntegrationReceiptRecorded {
        receipt: Value,
    },
    RecoveryActionCompleted {
        receipt: Value,
    },
    CheckpointRestoreRequested {
        checkpoint_id: String,
        source_cursor: u64,
    },
    CancellationRequested {
        source: String,
    },
    CancellationCompleted,
    RuntimeTurnFinished,
    RuntimeFailed {
        message: String,
    },
    SessionReset {
        reason: String,
    },
    ModelRequestStarted {
        provider: String,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_fingerprint: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        manifest_ref: Option<String>,
        #[serde(default)]
        attempt_ordinal: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_request_id: Option<String>,
    },
    ModelResponseReceived {
        response_id: Option<String>,
        usage: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_fingerprint: Option<String>,
    },
    ModelRequestFailed {
        request_id: String,
        request_fingerprint: String,
        error: String,
    },
    ProviderExecutionRecorded {
        status: Value,
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
    ToolExecutionTimingRecorded {
        tool_use_id: String,
        tool: String,
        queue_duration_ns: u64,
        execution_duration_ns: u64,
        expected_duration_ms: u64,
        concurrency_cost: u16,
        cached: bool,
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
    use serde_json::json;

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
    fn session_action_contract_round_trips_and_rejects_rollback() {
        let action = SessionAction {
            action_id: "action-1".to_owned(),
            idempotency_key: "idem-1".to_owned(),
            source: "desktop:client-1".to_owned(),
            target_session_id: "session-1".to_owned(),
            expected_session_revision: 7,
            kind: SessionActionKind::Steer,
            delivery_policy: SessionActionDeliveryPolicy::NextSafeTurnBoundary,
            wake_policy: SessionActionWakePolicy::OnBoundary,
            payload: json!({"text": "use the new constraint"}),
        };
        action.validate().expect("valid action");
        let event = EventEnvelope::new(
            8,
            SessionId::new(),
            Actor::User,
            CorrelationId::new(),
            EventPayload::SessionActionAccepted { action },
            None,
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("action event");
        let json = serde_json::to_string(&event).expect("serialize action event");
        assert_eq!(
            serde_json::from_str::<EventEnvelope>(&json).expect("deserialize action event"),
            event
        );
        assert!(
            !SessionActionLifecycle::Committing.can_transition_to(SessionActionLifecycle::Queued)
        );
        assert!(
            SessionActionLifecycle::Committing.can_transition_to(SessionActionLifecycle::Running)
        );
    }

    #[test]
    fn replacement_and_rejection_contracts_round_trip() {
        let action = SessionAction {
            action_id: "action-2".to_owned(),
            idempotency_key: "idem-2".to_owned(),
            source: "telegram:client-2".to_owned(),
            target_session_id: "session-1".to_owned(),
            expected_session_revision: 9,
            kind: SessionActionKind::ReplaceFollowUp,
            delivery_policy: SessionActionDeliveryPolicy::WhenIdle,
            wake_policy: SessionActionWakePolicy::OnBoundary,
            payload: json!({"text": "replacement", "replaces_action_id": "action-1"}),
        };
        let event = EventEnvelope::new(
            10,
            SessionId::new(),
            Actor::User,
            CorrelationId::new(),
            EventPayload::SessionActionRejected {
                action,
                authoritative_revision: 10,
                reason: "stale_revision".to_owned(),
            },
            None,
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("rejection event");
        let json = serde_json::to_string(&event).expect("serialize rejection");
        assert_eq!(
            serde_json::from_str::<EventEnvelope>(&json).expect("deserialize rejection"),
            event
        );
    }

    #[test]
    fn tool_execution_timing_round_trips() {
        let event = EventEnvelope::new(
            2,
            SessionId::new(),
            Actor::Coordinator,
            CorrelationId::new(),
            EventPayload::ToolExecutionTimingRecorded {
                tool_use_id: "tool-1".to_owned(),
                tool: "fs_read".to_owned(),
                queue_duration_ns: 125,
                execution_duration_ns: 500,
                expected_duration_ms: 10,
                concurrency_cost: 1,
                cached: false,
            },
            None,
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("timing event");
        let json = serde_json::to_string(&event).expect("serialize timing event");
        assert_eq!(
            serde_json::from_str::<EventEnvelope>(&json).expect("deserialize timing event"),
            event
        );
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
