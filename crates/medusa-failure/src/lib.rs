//! Deterministic failure classification and bounded retry policy.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureDomain {
    Network,
    Provider,
    Tool,
    Filesystem,
    Validation,
    Permission,
    User,
    Policy,
    Internal,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureDisposition {
    RetryImmediately,
    RetryWithBackoff,
    Replan,
    Terminal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FailureSignal {
    pub domain: FailureDomain,
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub transient: bool,
    #[serde(default)]
    pub strategy_invalidated: bool,
}

impl FailureSignal {
    pub fn new(
        domain: FailureDomain,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self, &'static str> {
        let code = code.into();
        let message = message.into();
        if code.trim().is_empty() {
            return Err("failure code cannot be empty");
        }
        if message.trim().is_empty() {
            return Err("failure message cannot be empty");
        }
        Ok(Self {
            domain,
            code,
            message,
            transient: false,
            strategy_invalidated: false,
        })
    }

    #[must_use]
    pub fn transient(mut self) -> Self {
        self.transient = true;
        self
    }

    #[must_use]
    pub fn invalidates_strategy(mut self) -> Self {
        self.strategy_invalidated = true;
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FailureRecord {
    pub sequence: u32,
    pub occurred_at: OffsetDateTime,
    pub signal: FailureSignal,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_backoff_ms: u64,
    pub max_backoff_ms: u64,
    pub repeated_code_replan_threshold: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_backoff_ms: 500,
            max_backoff_ms: 30_000,
            repeated_code_replan_threshold: 2,
        }
    }
}

impl RetryPolicy {
    pub fn validate(self) -> Result<Self, &'static str> {
        if self.max_attempts == 0 {
            return Err("max_attempts must be greater than zero");
        }
        if self.base_backoff_ms == 0 {
            return Err("base_backoff_ms must be greater than zero");
        }
        if self.max_backoff_ms < self.base_backoff_ms {
            return Err("max_backoff_ms must be at least base_backoff_ms");
        }
        if self.repeated_code_replan_threshold == 0 {
            return Err("repeated_code_replan_threshold must be greater than zero");
        }
        Ok(self)
    }

    #[must_use]
    pub fn backoff_ms(self, attempt: u32) -> u64 {
        let exponent = attempt.saturating_sub(1).min(31);
        self.base_backoff_ms
            .saturating_mul(1_u64 << exponent)
            .min(self.max_backoff_ms)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FailureDecision {
    pub disposition: FailureDisposition,
    pub reason: String,
    pub attempt: u32,
    pub remaining_attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backoff_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FailureHistory {
    #[serde(default)]
    records: Vec<FailureRecord>,
}

impl FailureHistory {
    pub fn append(&mut self, record: FailureRecord) -> Result<(), &'static str> {
        let expected = self.records.last().map_or(0, |item| item.sequence + 1);
        if record.sequence != expected {
            return Err("failure record sequence is not contiguous");
        }
        if self
            .records
            .last()
            .is_some_and(|item| record.occurred_at < item.occurred_at)
        {
            return Err("failure record timestamp regressed");
        }
        self.records.push(record);
        Ok(())
    }

    #[must_use]
    pub fn records(&self) -> &[FailureRecord] {
        &self.records
    }

    #[must_use]
    pub fn attempts_for(&self, code: &str) -> u32 {
        self.records
            .iter()
            .filter(|record| record.signal.code == code)
            .count() as u32
    }

    #[must_use]
    pub fn classify(&self, signal: &FailureSignal, policy: RetryPolicy) -> FailureDecision {
        let prior_attempts = self.attempts_for(&signal.code);
        let attempt = prior_attempts + 1;
        let remaining_attempts = policy.max_attempts.saturating_sub(attempt);

        if matches!(
            signal.domain,
            FailureDomain::User | FailureDomain::Policy | FailureDomain::Permission
        ) {
            return FailureDecision {
                disposition: FailureDisposition::Terminal,
                reason: "failure requires external authorization or user action".to_owned(),
                attempt,
                remaining_attempts: 0,
                backoff_ms: None,
            };
        }

        if matches!(signal.domain, FailureDomain::Validation) || signal.strategy_invalidated {
            return FailureDecision {
                disposition: FailureDisposition::Replan,
                reason: "current strategy is invalid and must be revised".to_owned(),
                attempt,
                remaining_attempts,
                backoff_ms: None,
            };
        }

        if attempt > policy.max_attempts {
            return FailureDecision {
                disposition: FailureDisposition::Terminal,
                reason: "retry budget exhausted".to_owned(),
                attempt,
                remaining_attempts: 0,
                backoff_ms: None,
            };
        }

        if attempt >= policy.repeated_code_replan_threshold && !signal.transient {
            return FailureDecision {
                disposition: FailureDisposition::Replan,
                reason: "repeated non-transient failure indicates the strategy should change"
                    .to_owned(),
                attempt,
                remaining_attempts,
                backoff_ms: None,
            };
        }

        if signal.transient
            || matches!(
                signal.domain,
                FailureDomain::Network | FailureDomain::Provider
            )
        {
            return FailureDecision {
                disposition: FailureDisposition::RetryWithBackoff,
                reason: "failure appears transient".to_owned(),
                attempt,
                remaining_attempts,
                backoff_ms: Some(policy.backoff_ms(attempt)),
            };
        }

        if matches!(
            signal.domain,
            FailureDomain::Tool | FailureDomain::Filesystem | FailureDomain::Internal
        ) {
            return FailureDecision {
                disposition: FailureDisposition::RetryImmediately,
                reason: "failure may succeed on a bounded immediate retry".to_owned(),
                attempt,
                remaining_attempts,
                backoff_ms: None,
            };
        }

        FailureDecision {
            disposition: FailureDisposition::Terminal,
            reason: "failure is not safely retryable".to_owned(),
            attempt,
            remaining_attempts: 0,
            backoff_ms: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn signal(domain: FailureDomain, code: &str) -> FailureSignal {
        FailureSignal::new(domain, code, "failure").expect("signal")
    }

    #[test]
    fn transient_network_failure_uses_backoff() {
        let decision = FailureHistory::default().classify(
            &signal(FailureDomain::Network, "timeout").transient(),
            RetryPolicy::default(),
        );
        assert_eq!(
            decision.disposition,
            FailureDisposition::RetryWithBackoff
        );
        assert_eq!(decision.backoff_ms, Some(500));
    }

    #[test]
    fn permission_failure_is_terminal() {
        let decision = FailureHistory::default().classify(
            &signal(FailureDomain::Permission, "denied"),
            RetryPolicy::default(),
        );
        assert_eq!(decision.disposition, FailureDisposition::Terminal);
    }

    #[test]
    fn repeated_non_transient_failure_replans() {
        let mut history = FailureHistory::default();
        history
            .append(FailureRecord {
                sequence: 0,
                occurred_at: datetime!(2026-07-24 10:00 UTC),
                signal: signal(FailureDomain::Tool, "broken-tool"),
            })
            .expect("append");
        let decision = history.classify(
            &signal(FailureDomain::Tool, "broken-tool"),
            RetryPolicy::default(),
        );
        assert_eq!(decision.disposition, FailureDisposition::Replan);
    }

    #[test]
    fn exhausted_budget_is_terminal() {
        let mut history = FailureHistory::default();
        for sequence in 0..3 {
            history
                .append(FailureRecord {
                    sequence,
                    occurred_at: datetime!(2026-07-24 10:00 UTC),
                    signal: signal(FailureDomain::Network, "timeout").transient(),
                })
                .expect("append");
        }
        let decision = history.classify(
            &signal(FailureDomain::Network, "timeout").transient(),
            RetryPolicy::default(),
        );
        assert_eq!(decision.disposition, FailureDisposition::Terminal);
        assert_eq!(decision.reason, "retry budget exhausted");
    }

    #[test]
    fn invalid_history_order_is_rejected() {
        let mut history = FailureHistory::default();
        history
            .append(FailureRecord {
                sequence: 0,
                occurred_at: datetime!(2026-07-24 10:01 UTC),
                signal: signal(FailureDomain::Tool, "one"),
            })
            .expect("append");
        assert_eq!(
            history.append(FailureRecord {
                sequence: 2,
                occurred_at: datetime!(2026-07-24 10:02 UTC),
                signal: signal(FailureDomain::Tool, "two"),
            }),
            Err("failure record sequence is not contiguous")
        );
    }
}
