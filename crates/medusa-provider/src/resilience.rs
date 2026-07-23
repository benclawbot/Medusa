//! Deterministic provider retry classification, delay policy, and observable events.

use medusa_core::{ErrorCategory, MedusaError};
use serde::{Deserialize, Serialize};

/// Whether a failed provider request may be retried with identical input.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryDisposition {
    Retry,
    Failover,
    Permanent,
}

/// Retry timing configuration shared by provider adapters and the manager.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetryPolicy {
    pub max_retries_per_provider: u8,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub jitter_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries_per_provider: 2,
            base_delay_ms: 250,
            max_delay_ms: 8_000,
            jitter_ms: 100,
        }
    }
}

impl RetryPolicy {
    /// Calculates a deterministic delay. Provider retry-after metadata wins.
    #[must_use]
    pub fn delay_ms(&self, error: &MedusaError, provider_index: usize, attempt: u8) -> u64 {
        if let Some(delay) = retry_after_ms(error) {
            return delay.min(self.max_delay_ms);
        }
        let exponent = u32::from(attempt.min(20));
        let exponential = self.base_delay_ms.saturating_mul(1_u64 << exponent);
        let jitter = if self.jitter_ms == 0 {
            0
        } else {
            stable_jitter(provider_index, attempt) % (self.jitter_ms + 1)
        };
        exponential
            .saturating_add(jitter)
            .min(self.max_delay_ms)
    }
}

/// Durable, frontend-neutral provider routing status.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderRouteEvent {
    Attempt {
        provider_index: usize,
        attempt: u8,
    },
    RetryScheduled {
        provider_index: usize,
        next_attempt: u8,
        delay_ms: u64,
        reason: String,
    },
    Failover {
        from_provider_index: usize,
        to_provider_index: usize,
        reason: String,
    },
    Completed {
        provider_index: usize,
        attempts: u8,
    },
    Failed {
        provider_index: usize,
        reason: String,
    },
}

/// Classifies provider errors without relying on message text.
#[must_use]
pub fn classify_error(error: &MedusaError, has_fallback: bool) -> RetryDisposition {
    if error.retryable || error.category == ErrorCategory::Transient {
        RetryDisposition::Retry
    } else if has_fallback && error.category == ErrorCategory::Environment {
        RetryDisposition::Failover
    } else {
        RetryDisposition::Permanent
    }
}

fn retry_after_ms(error: &MedusaError) -> Option<u64> {
    error
        .context
        .get("retry_after_ms")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            error
                .context
                .get("retry_after_seconds")
                .and_then(serde_json::Value::as_u64)
                .map(|seconds| seconds.saturating_mul(1_000))
        })
}

fn stable_jitter(provider_index: usize, attempt: u8) -> u64 {
    let mut value = (provider_index as u64).wrapping_add(1);
    value ^= u64::from(attempt).wrapping_add(0x9e37_79b9_7f4a_7c15);
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use medusa_core::{ErrorCode, MedusaError};
    use serde_json::json;

    use super::*;

    fn error(category: ErrorCategory, retryable: bool) -> MedusaError {
        MedusaError::new(ErrorCode::DependencyUnavailable, category, "provider failure")
            .with_retryable(retryable)
    }

    #[test]
    fn transient_failures_retry_but_validation_failures_are_permanent() {
        assert_eq!(
            classify_error(&error(ErrorCategory::Transient, false), true),
            RetryDisposition::Retry
        );
        assert_eq!(
            classify_error(&error(ErrorCategory::Validation, false), true),
            RetryDisposition::Permanent
        );
    }

    #[test]
    fn environment_failure_uses_fallback_when_available() {
        assert_eq!(
            classify_error(&error(ErrorCategory::Environment, false), true),
            RetryDisposition::Failover
        );
        assert_eq!(
            classify_error(&error(ErrorCategory::Environment, false), false),
            RetryDisposition::Permanent
        );
    }

    #[test]
    fn retry_after_metadata_overrides_exponential_delay() {
        let mut failure = error(ErrorCategory::Transient, true);
        failure
            .context
            .insert("retry_after_seconds".into(), json!(3));
        assert_eq!(RetryPolicy::default().delay_ms(&failure, 0, 0), 3_000);
    }

    #[test]
    fn backoff_is_bounded_and_deterministic() {
        let policy = RetryPolicy {
            max_retries_per_provider: 4,
            base_delay_ms: 100,
            max_delay_ms: 500,
            jitter_ms: 10,
        };
        let failure = error(ErrorCategory::Transient, true);
        assert_eq!(
            policy.delay_ms(&failure, 1, 2),
            policy.delay_ms(&failure, 1, 2)
        );
        assert!(policy.delay_ms(&failure, 1, 20) <= 500);
    }
}
