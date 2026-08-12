//! Provider-neutral escalation policy and bounded reasoning packets.

mod lifecycle;
mod manual;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

pub use lifecycle::EscalationLifecycleEvent;
pub use manual::{AdviceEnvelope, export_packet, import_advice};

/// Supported transport boundary for an escalation request.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationMode {
    /// Export a packet for copy/paste into a normal ChatGPT conversation.
    Manual,
    /// Send through a documented model-provider API.
    Provider,
    /// Send through a configured MCP integration.
    Mcp,
}

/// Why local execution is requesting higher-level reasoning.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationReason {
    LowConfidence,
    ConfidenceCollapse,
    RepeatedRetryableFailure,
    TerminalFailureNeedsDecision,
    ConflictingEvidence,
    MilestoneReview,
    ExplicitUserRequest,
}

/// Inputs used to decide whether escalation is allowed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EscalationContext {
    pub confidence_basis_points: Option<u16>,
    pub recent_drop_basis_points: u16,
    pub consecutive_retryable_failures: u16,
    pub terminal_failure: bool,
    pub conflicting_evidence: bool,
    pub milestone_review: bool,
    pub explicit_user_request: bool,
    pub local_spike_completed: bool,
    pub escalations_used: u16,
    pub turns_since_last_escalation: Option<u16>,
}

/// Bounded policy that keeps normal execution local.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EscalationPolicy {
    pub minimum_confidence_basis_points: u16,
    pub maximum_recent_drop_basis_points: u16,
    pub retryable_failure_threshold: u16,
    pub maximum_escalations_per_task: u16,
    pub cooldown_turns: u16,
    pub require_local_spike: bool,
}

impl Default for EscalationPolicy {
    fn default() -> Self {
        Self {
            minimum_confidence_basis_points: 6_500,
            maximum_recent_drop_basis_points: 2_000,
            retryable_failure_threshold: 3,
            maximum_escalations_per_task: 3,
            cooldown_turns: 2,
            require_local_spike: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationDecision {
    ContinueLocally,
    Escalate {
        reasons: BTreeSet<EscalationReason>,
    },
    Blocked {
        reasons: BTreeSet<EscalationBlockReason>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationBlockReason {
    LocalSpikeRequired,
    TaskLimitReached,
    CooldownActive,
}

impl EscalationPolicy {
    #[must_use]
    pub fn evaluate(&self, context: &EscalationContext) -> EscalationDecision {
        let mut reasons = BTreeSet::new();
        if context
            .confidence_basis_points
            .is_some_and(|value| value < self.minimum_confidence_basis_points)
        {
            reasons.insert(EscalationReason::LowConfidence);
        }
        if context.recent_drop_basis_points > self.maximum_recent_drop_basis_points {
            reasons.insert(EscalationReason::ConfidenceCollapse);
        }
        if context.consecutive_retryable_failures >= self.retryable_failure_threshold {
            reasons.insert(EscalationReason::RepeatedRetryableFailure);
        }
        if context.terminal_failure {
            reasons.insert(EscalationReason::TerminalFailureNeedsDecision);
        }
        if context.conflicting_evidence {
            reasons.insert(EscalationReason::ConflictingEvidence);
        }
        if context.milestone_review {
            reasons.insert(EscalationReason::MilestoneReview);
        }
        if context.explicit_user_request {
            reasons.insert(EscalationReason::ExplicitUserRequest);
        }

        if reasons.is_empty() {
            return EscalationDecision::ContinueLocally;
        }

        let mut blocked = BTreeSet::new();
        if self.require_local_spike
            && !context.local_spike_completed
            && !context.explicit_user_request
        {
            blocked.insert(EscalationBlockReason::LocalSpikeRequired);
        }
        if context.escalations_used >= self.maximum_escalations_per_task {
            blocked.insert(EscalationBlockReason::TaskLimitReached);
        }
        if context
            .turns_since_last_escalation
            .is_some_and(|turns| turns < self.cooldown_turns)
            && !context.explicit_user_request
        {
            blocked.insert(EscalationBlockReason::CooldownActive);
        }

        if blocked.is_empty() {
            EscalationDecision::Escalate { reasons }
        } else {
            EscalationDecision::Blocked { reasons: blocked }
        }
    }
}

/// One repository read bound to the packet state.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ReadSetEntry {
    pub path: String,
    pub content_sha256: String,
}

/// Minimal evidence supplied to the external reasoning boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EscalationPacket {
    pub schema_version: u16,
    pub packet_id: String,
    pub session_id: String,
    pub task_id: String,
    pub mode: EscalationMode,
    pub objective: String,
    pub decision_question: String,
    pub constraints: Vec<String>,
    pub attempted_actions: Vec<String>,
    pub observed_failures: Vec<String>,
    pub evidence: Vec<String>,
    pub read_set: BTreeSet<ReadSetEntry>,
    pub metadata: BTreeMap<String, String>,
    pub reasons: BTreeSet<EscalationReason>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub digest_sha256: String,
}

#[derive(Serialize)]
struct UnsignedPacket<'a> {
    schema_version: u16,
    packet_id: &'a str,
    session_id: &'a str,
    task_id: &'a str,
    mode: EscalationMode,
    objective: &'a str,
    decision_question: &'a str,
    constraints: &'a [String],
    attempted_actions: &'a [String],
    observed_failures: &'a [String],
    evidence: &'a [String],
    read_set: &'a BTreeSet<ReadSetEntry>,
    metadata: &'a BTreeMap<String, String>,
    reasons: &'a BTreeSet<EscalationReason>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: &'a OffsetDateTime,
}

impl EscalationPacket {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        packet_id: impl Into<String>,
        session_id: impl Into<String>,
        task_id: impl Into<String>,
        mode: EscalationMode,
        objective: impl Into<String>,
        decision_question: impl Into<String>,
        reasons: BTreeSet<EscalationReason>,
        created_at: OffsetDateTime,
    ) -> Result<Self, &'static str> {
        let mut packet = Self {
            schema_version: 1,
            packet_id: packet_id.into(),
            session_id: session_id.into(),
            task_id: task_id.into(),
            mode,
            objective: objective.into(),
            decision_question: decision_question.into(),
            constraints: Vec::new(),
            attempted_actions: Vec::new(),
            observed_failures: Vec::new(),
            evidence: Vec::new(),
            read_set: BTreeSet::new(),
            metadata: BTreeMap::new(),
            reasons,
            created_at,
            digest_sha256: String::new(),
        };
        packet.validate()?;
        packet
            .refresh_digest()
            .map_err(|_| "could not hash packet")?;
        Ok(packet)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != 1 {
            return Err("unsupported escalation packet schema version");
        }
        for value in [
            self.packet_id.as_str(),
            self.session_id.as_str(),
            self.task_id.as_str(),
            self.objective.as_str(),
            self.decision_question.as_str(),
        ] {
            if value.trim().is_empty() {
                return Err("required escalation packet field cannot be empty");
            }
        }
        if self.reasons.is_empty() {
            return Err("escalation packet requires at least one reason");
        }
        Ok(())
    }

    pub fn refresh_digest(&mut self) -> Result<(), serde_json::Error> {
        self.digest_sha256 = self.compute_digest()?;
        Ok(())
    }

    pub fn verify_digest(&self) -> Result<bool, serde_json::Error> {
        Ok(self.digest_sha256 == self.compute_digest()?)
    }

    fn compute_digest(&self) -> Result<String, serde_json::Error> {
        let unsigned = UnsignedPacket {
            schema_version: self.schema_version,
            packet_id: &self.packet_id,
            session_id: &self.session_id,
            task_id: &self.task_id,
            mode: self.mode,
            objective: &self.objective,
            decision_question: &self.decision_question,
            constraints: &self.constraints,
            attempted_actions: &self.attempted_actions,
            observed_failures: &self.observed_failures,
            evidence: &self.evidence,
            read_set: &self.read_set,
            metadata: &self.metadata,
            reasons: &self.reasons,
            created_at: &self.created_at,
        };
        let bytes = serde_json::to_vec(&unsigned)?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> EscalationContext {
        EscalationContext {
            confidence_basis_points: Some(8_000),
            recent_drop_basis_points: 0,
            consecutive_retryable_failures: 0,
            terminal_failure: false,
            conflicting_evidence: false,
            milestone_review: false,
            explicit_user_request: false,
            local_spike_completed: true,
            escalations_used: 0,
            turns_since_last_escalation: None,
        }
    }

    #[test]
    fn healthy_execution_stays_local() {
        assert_eq!(
            EscalationPolicy::default().evaluate(&context()),
            EscalationDecision::ContinueLocally
        );
    }

    #[test]
    fn low_confidence_escalates_after_local_spike() {
        let mut input = context();
        input.confidence_basis_points = Some(5_000);
        let EscalationDecision::Escalate { reasons } = EscalationPolicy::default().evaluate(&input)
        else {
            panic!("expected escalation");
        };
        assert!(reasons.contains(&EscalationReason::LowConfidence));
    }

    #[test]
    fn automatic_escalation_is_blocked_until_local_spike_completes() {
        let mut input = context();
        input.confidence_basis_points = Some(5_000);
        input.local_spike_completed = false;
        assert_eq!(
            EscalationPolicy::default().evaluate(&input),
            EscalationDecision::Blocked {
                reasons: BTreeSet::from([EscalationBlockReason::LocalSpikeRequired])
            }
        );
    }

    #[test]
    fn explicit_user_request_bypasses_spike_and_cooldown_but_not_task_cap() {
        let mut input = context();
        input.explicit_user_request = true;
        input.local_spike_completed = false;
        input.turns_since_last_escalation = Some(0);
        assert!(matches!(
            EscalationPolicy::default().evaluate(&input),
            EscalationDecision::Escalate { .. }
        ));
        input.escalations_used = EscalationPolicy::default().maximum_escalations_per_task;
        assert_eq!(
            EscalationPolicy::default().evaluate(&input),
            EscalationDecision::Blocked {
                reasons: BTreeSet::from([EscalationBlockReason::TaskLimitReached])
            }
        );
    }

    #[test]
    fn packet_digest_is_deterministic_and_detects_mutation() {
        let mut packet = EscalationPacket::new(
            "packet-1",
            "session-1",
            "task-1",
            EscalationMode::Manual,
            "fix the failing parser",
            "Which invariant is most likely violated?",
            BTreeSet::from([EscalationReason::ConflictingEvidence]),
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("packet");
        let original = packet.digest_sha256.clone();
        packet.metadata.insert("branch".into(), "main".into());
        assert!(!packet.verify_digest().expect("verify"));
        packet.refresh_digest().expect("refresh");
        assert_ne!(packet.digest_sha256, original);
        assert!(packet.verify_digest().expect("verify"));
    }
}
