//! Confidence history and spike gating for autonomous task execution.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Stable identifier for one todo item.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TodoId(String);

impl TodoId {
    pub fn parse(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("todo identifier cannot be empty");
        }
        if trimmed.len() > 96 {
            return Err("todo identifier is too long");
        }
        if !trimmed
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err("todo identifier contains unsupported characters");
        }
        Ok(Self(trimmed.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Confidence value represented as basis points from 0 to 10,000.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Confidence(u16);

impl Confidence {
    pub const MIN: Self = Self(0);
    pub const MAX: Self = Self(10_000);

    pub fn from_basis_points(value: u16) -> Result<Self, &'static str> {
        if value > Self::MAX.0 {
            return Err("confidence cannot exceed 10000 basis points");
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn basis_points(self) -> u16 {
        self.0
    }
}

/// Why confidence changed.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceReason {
    InitialEstimate,
    RepositoryInspection,
    DependencyResolved,
    AssumptionInvalidated,
    ToolFailure,
    VerificationPassed,
    VerificationFailed,
    UserClarification,
    ManualAdjustment,
}

/// One immutable confidence observation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConfidenceObservation {
    pub sequence: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    pub confidence: Confidence,
    pub reason: ConfidenceReason,
    pub summary: String,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub evidence_refs: BTreeSet<String>,
}

impl ConfidenceObservation {
    pub fn new(
        sequence: u64,
        recorded_at: OffsetDateTime,
        confidence: Confidence,
        reason: ConfidenceReason,
        summary: impl Into<String>,
    ) -> Result<Self, &'static str> {
        let summary = summary.into();
        if sequence == 0 {
            return Err("confidence observation sequence must start at one");
        }
        if summary.trim().is_empty() {
            return Err("confidence observation summary cannot be empty");
        }
        Ok(Self {
            sequence,
            recorded_at,
            confidence,
            reason,
            summary,
            evidence_refs: BTreeSet::new(),
        })
    }

    #[must_use]
    pub fn with_evidence(mut self, evidence_ref: impl Into<String>) -> Self {
        let evidence_ref = evidence_ref.into();
        if !evidence_ref.trim().is_empty() {
            self.evidence_refs.insert(evidence_ref);
        }
        self
    }
}

/// Durable confidence history for one todo.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TodoConfidenceHistory {
    pub todo_id: TodoId,
    #[serde(default)]
    pub observations: Vec<ConfidenceObservation>,
}

impl TodoConfidenceHistory {
    #[must_use]
    pub fn new(todo_id: TodoId) -> Self {
        Self {
            todo_id,
            observations: Vec::new(),
        }
    }

    pub fn append(&mut self, observation: ConfidenceObservation) -> Result<(), &'static str> {
        if let Some(previous) = self.observations.last() {
            if observation.sequence != previous.sequence.saturating_add(1) {
                return Err("confidence observation sequence must be contiguous");
            }
            if observation.recorded_at < previous.recorded_at {
                return Err("confidence observation timestamps must be monotonic");
            }
        } else if observation.sequence != 1 {
            return Err("first confidence observation sequence must be one");
        }
        self.observations.push(observation);
        Ok(())
    }

    #[must_use]
    pub fn current(&self) -> Option<Confidence> {
        self.observations.last().map(|item| item.confidence)
    }

    #[must_use]
    pub fn delta_basis_points(&self) -> Option<i32> {
        let first = self.observations.first()?.confidence.basis_points();
        let latest = self.observations.last()?.confidence.basis_points();
        Some(i32::from(latest) - i32::from(first))
    }

    #[must_use]
    pub fn recent_drop_basis_points(&self, window: usize) -> u16 {
        if window < 2 || self.observations.len() < 2 {
            return 0;
        }
        let start = self.observations.len().saturating_sub(window);
        let slice = &self.observations[start..];
        let highest = slice
            .iter()
            .map(|item| item.confidence.basis_points())
            .max()
            .unwrap_or(0);
        let latest = slice.last().map(|item| item.confidence.basis_points()).unwrap_or(0);
        highest.saturating_sub(latest)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        for (index, observation) in self.observations.iter().enumerate() {
            if observation.sequence != (index as u64).saturating_add(1) {
                return Err("confidence history contains a non-contiguous sequence");
            }
            if index > 0 && observation.recorded_at < self.observations[index - 1].recorded_at {
                return Err("confidence history contains a non-monotonic timestamp");
            }
        }
        Ok(())
    }
}

/// Gate policy for deciding whether execution must pause for a bounded spike.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpikeGatePolicy {
    pub minimum_execution_confidence: Confidence,
    pub maximum_recent_drop: u16,
    pub recent_window: usize,
    pub minimum_observations: usize,
}

impl Default for SpikeGatePolicy {
    fn default() -> Self {
        Self {
            minimum_execution_confidence: Confidence(6_500),
            maximum_recent_drop: 2_000,
            recent_window: 4,
            minimum_observations: 1,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpikeGateReason {
    MissingConfidence,
    InsufficientHistory,
    BelowExecutionThreshold,
    ConfidenceCollapsed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpikeRequest {
    pub todo_id: TodoId,
    pub reasons: Vec<SpikeGateReason>,
    pub current_confidence: Option<Confidence>,
    pub recent_drop_basis_points: u16,
    pub questions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateDecision {
    Execute,
    Spike(SpikeRequest),
}

impl SpikeGatePolicy {
    #[must_use]
    pub fn evaluate(&self, history: &TodoConfidenceHistory) -> GateDecision {
        let current = history.current();
        let recent_drop = history.recent_drop_basis_points(self.recent_window);
        let mut reasons = Vec::new();

        if current.is_none() {
            reasons.push(SpikeGateReason::MissingConfidence);
        }
        if history.observations.len() < self.minimum_observations {
            reasons.push(SpikeGateReason::InsufficientHistory);
        }
        if current.is_some_and(|value| value < self.minimum_execution_confidence) {
            reasons.push(SpikeGateReason::BelowExecutionThreshold);
        }
        if recent_drop > self.maximum_recent_drop {
            reasons.push(SpikeGateReason::ConfidenceCollapsed);
        }

        if reasons.is_empty() {
            GateDecision::Execute
        } else {
            let mut questions = vec![
                "What evidence is missing before this todo can be executed safely?".to_owned(),
                "Which assumption has the highest chance of being wrong?".to_owned(),
            ];
            if reasons.contains(&SpikeGateReason::ConfidenceCollapsed) {
                questions.push("What changed since the highest recent confidence estimate?".to_owned());
            }
            GateDecision::Spike(SpikeRequest {
                todo_id: history.todo_id.clone(),
                reasons,
                current_confidence: current,
                recent_drop_basis_points: recent_drop,
                questions,
            })
        }
    }
}

/// Durable registry used by planners and checkpoint state.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConfidenceRegistry {
    #[serde(default)]
    histories: BTreeMap<TodoId, TodoConfidenceHistory>,
}

impl ConfidenceRegistry {
    pub fn insert(&mut self, history: TodoConfidenceHistory) -> Result<(), &'static str> {
        history.validate()?;
        self.histories.insert(history.todo_id.clone(), history);
        Ok(())
    }

    #[must_use]
    pub fn get(&self, todo_id: &TodoId) -> Option<&TodoConfidenceHistory> {
        self.histories.get(todo_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn confidence(value: u16) -> Confidence {
        Confidence::from_basis_points(value).expect("valid confidence")
    }

    fn observation(sequence: u64, value: u16) -> ConfidenceObservation {
        ConfidenceObservation::new(
            sequence,
            OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(sequence as i64),
            confidence(value),
            ConfidenceReason::ManualAdjustment,
            format!("estimate {value}"),
        )
        .expect("observation")
    }

    #[test]
    fn high_stable_confidence_allows_execution() {
        let mut history = TodoConfidenceHistory::new(TodoId::parse("implement-api").unwrap());
        history.append(observation(1, 7_500)).unwrap();
        history.append(observation(2, 7_800)).unwrap();
        assert_eq!(SpikeGatePolicy::default().evaluate(&history), GateDecision::Execute);
    }

    #[test]
    fn low_confidence_requires_a_spike() {
        let mut history = TodoConfidenceHistory::new(TodoId::parse("migration").unwrap());
        history.append(observation(1, 5_000)).unwrap();
        let GateDecision::Spike(request) = SpikeGatePolicy::default().evaluate(&history) else {
            panic!("expected spike");
        };
        assert!(request.reasons.contains(&SpikeGateReason::BelowExecutionThreshold));
    }

    #[test]
    fn sharp_recent_drop_requires_a_spike_even_above_threshold() {
        let mut history = TodoConfidenceHistory::new(TodoId::parse("refactor").unwrap());
        history.append(observation(1, 9_500)).unwrap();
        history.append(observation(2, 9_000)).unwrap();
        history.append(observation(3, 7_000)).unwrap();
        let GateDecision::Spike(request) = SpikeGatePolicy::default().evaluate(&history) else {
            panic!("expected spike");
        };
        assert!(request.reasons.contains(&SpikeGateReason::ConfidenceCollapsed));
        assert_eq!(request.recent_drop_basis_points, 2_500);
    }

    #[test]
    fn history_rejects_gaps_and_time_travel() {
        let mut history = TodoConfidenceHistory::new(TodoId::parse("task").unwrap());
        history.append(observation(1, 7_000)).unwrap();
        assert_eq!(
            history.append(observation(3, 7_500)),
            Err("confidence observation sequence must be contiguous")
        );
        let earlier = ConfidenceObservation::new(
            2,
            OffsetDateTime::UNIX_EPOCH,
            confidence(7_500),
            ConfidenceReason::ManualAdjustment,
            "earlier",
        )
        .unwrap();
        assert_eq!(
            history.append(earlier),
            Err("confidence observation timestamps must be monotonic")
        );
    }

    #[test]
    fn missing_history_requires_a_spike() {
        let history = TodoConfidenceHistory::new(TodoId::parse("unknown").unwrap());
        let GateDecision::Spike(request) = SpikeGatePolicy::default().evaluate(&history) else {
            panic!("expected spike");
        };
        assert!(request.reasons.contains(&SpikeGateReason::MissingConfidence));
    }
}
