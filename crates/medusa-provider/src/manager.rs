//! Provider routing with bounded retry, failover, response caching, and health snapshots.

use std::{
    collections::BTreeMap,
    sync::Mutex,
    thread,
    time::Duration,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};

use crate::{ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities};

/// Observable health state for a configured provider position.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderHealth {
    pub attempts: u64,
    pub retries: u64,
    pub failovers: u64,
    pub successes: u64,
    pub last_delay_ms: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetryDisposition {
    Retry,
    Failover,
    Permanent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetryPolicy {
    max_retries_per_provider: u8,
    base_delay_ms: u64,
    max_delay_ms: u64,
    jitter_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries_per_provider: 1,
            base_delay_ms: 250,
            max_delay_ms: 8_000,
            jitter_ms: 100,
        }
    }
}

impl RetryPolicy {
    fn delay_ms(&self, error: &MedusaError, provider_index: usize, attempt: u8) -> u64 {
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

/// Routes requests through a primary provider followed by optional fallbacks.
pub struct ProviderManager<P> {
    providers: Vec<P>,
    policy: RetryPolicy,
    cache: Mutex<BTreeMap<String, ModelResponse>>,
    health: Mutex<Vec<ProviderHealth>>,
    sleeper: fn(Duration),
}

impl<P> ProviderManager<P> {
    /// Builds a manager with a primary provider and zero or more ordered fallbacks.
    #[must_use]
    pub fn new(providers: Vec<P>) -> Self {
        let health = vec![ProviderHealth::default(); providers.len()];
        Self {
            providers,
            policy: RetryPolicy::default(),
            cache: Mutex::new(BTreeMap::new()),
            health: Mutex::new(health),
            sleeper: thread::sleep,
        }
    }

    #[must_use]
    pub fn with_retries(mut self, retries_per_provider: u8) -> Self {
        self.policy.max_retries_per_provider = retries_per_provider;
        self
    }

    #[cfg(test)]
    fn with_policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }

    #[cfg(test)]
    fn without_sleep(mut self) -> Self {
        self.sleeper = |_| {};
        self
    }

    /// Returns a copy so callers never hold the manager's health lock.
    #[must_use]
    pub fn health(&self) -> Vec<ProviderHealth> {
        self.health
            .lock()
            .map(|health| health.clone())
            .unwrap_or_default()
    }

    fn record_attempt(&self, index: usize) {
        if let Ok(mut health) = self.health.lock()
            && let Some(entry) = health.get_mut(index)
        {
            entry.attempts = entry.attempts.saturating_add(1);
        }
    }

    fn record_success(&self, index: usize) {
        if let Ok(mut health) = self.health.lock()
            && let Some(entry) = health.get_mut(index)
        {
            entry.successes = entry.successes.saturating_add(1);
            entry.last_error = None;
            entry.last_delay_ms = None;
        }
    }

    fn record_error(&self, index: usize, error: &MedusaError) {
        if let Ok(mut health) = self.health.lock()
            && let Some(entry) = health.get_mut(index)
        {
            entry.last_error = Some(error.to_string());
        }
    }

    fn record_retry(&self, index: usize, delay_ms: u64) {
        if let Ok(mut health) = self.health.lock()
            && let Some(entry) = health.get_mut(index)
        {
            entry.retries = entry.retries.saturating_add(1);
            entry.last_delay_ms = Some(delay_ms);
        }
    }

    fn record_failover(&self, index: usize) {
        if let Ok(mut health) = self.health.lock()
            && let Some(entry) = health.get_mut(index)
        {
            entry.failovers = entry.failovers.saturating_add(1);
        }
    }
}

impl<P: ModelProvider> ModelProvider for ProviderManager<P> {
    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        let key = serde_json::to_string(request).map_err(|error| {
            MedusaError::new(
                ErrorCode::InvalidConfiguration,
                ErrorCategory::Validation,
                format!("could not serialize provider request for cache: {error}"),
            )
        })?;
        if let Ok(cache) = self.cache.lock()
            && let Some(response) = cache.get(&key)
        {
            return Ok(response.clone());
        }
        let mut final_error = None;
        for (index, provider) in self.providers.iter().enumerate() {
            let has_fallback = index + 1 < self.providers.len();
            for attempt in 0..=self.policy.max_retries_per_provider {
                self.record_attempt(index);
                match provider.complete(request) {
                    Ok(response) => {
                        self.record_success(index);
                        if let Ok(mut cache) = self.cache.lock() {
                            cache.insert(key, response.clone());
                        }
                        return Ok(response);
                    }
                    Err(error) => {
                        self.record_error(index, &error);
                        let disposition = classify_error(&error, has_fallback);
                        final_error = Some(error.clone());
                        match disposition {
                            RetryDisposition::Retry
                                if attempt < self.policy.max_retries_per_provider =>
                            {
                                let delay_ms = self.policy.delay_ms(&error, index, attempt);
                                self.record_retry(index, delay_ms);
                                (self.sleeper)(Duration::from_millis(delay_ms));
                            }
                            RetryDisposition::Retry | RetryDisposition::Failover
                                if has_fallback =>
                            {
                                self.record_failover(index);
                                break;
                            }
                            RetryDisposition::Permanent | RetryDisposition::Failover => {
                                return Err(error);
                            }
                            RetryDisposition::Retry => return Err(error),
                        }
                    }
                }
            }
        }
        Err(final_error.unwrap_or_else(|| {
            MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Environment,
                "no model providers are configured",
            )
        }))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.providers
            .first()
            .map_or_else(ProviderCapabilities::default, ModelProvider::capabilities)
    }
}

fn classify_error(error: &MedusaError, has_fallback: bool) -> RetryDisposition {
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
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use medusa_core::{ErrorCategory, ErrorCode, MedusaError};
    use serde_json::json;

    use super::*;
    use crate::{Message, MessageBlock, Role, Usage};

    #[derive(Clone)]
    struct StubProvider {
        calls: Arc<AtomicUsize>,
        response: MedusaResult<ModelResponse>,
    }

    impl ModelProvider for StubProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.response.clone()
        }
    }

    fn request() -> ModelRequest {
        ModelRequest {
            system: "test".into(),
            messages: vec![Message {
                role: Role::User,
                content: vec![MessageBlock::Text {
                    text: "hello".into(),
                }],
            }],
            tools: Vec::new(),
            max_tokens: 1,
            temperature_milli: 0,
        }
    }

    fn success() -> ModelResponse {
        ModelResponse {
            response_id: Some("response".into()),
            stop_reason: Some("end_turn".into()),
            blocks: Vec::new(),
            usage: Usage::default(),
        }
    }

    fn failure(category: ErrorCategory, retryable: bool) -> MedusaError {
        MedusaError::new(ErrorCode::DependencyUnavailable, category, "offline")
            .with_retryable(retryable)
    }

    #[test]
    fn retryable_primary_failure_falls_back_and_caches_the_response() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let primary = StubProvider {
            calls: primary_calls.clone(),
            response: Err(failure(ErrorCategory::Transient, true)),
        };
        let fallback = StubProvider {
            calls: fallback_calls.clone(),
            response: Ok(success()),
        };
        let manager = ProviderManager::new(vec![primary, fallback]).without_sleep();
        manager.complete(&request()).expect("fallback response");
        manager.complete(&request()).expect("cached response");
        assert_eq!(primary_calls.load(Ordering::SeqCst), 2);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
        assert_eq!(manager.health()[0].retries, 1);
        assert_eq!(manager.health()[0].failovers, 1);
        assert_eq!(manager.health()[1].successes, 1);
    }

    #[test]
    fn permanent_validation_failure_is_not_retried_or_failed_over() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let manager = ProviderManager::new(vec![
            StubProvider {
                calls: primary_calls.clone(),
                response: Err(failure(ErrorCategory::Validation, false)),
            },
            StubProvider {
                calls: fallback_calls.clone(),
                response: Ok(success()),
            },
        ])
        .without_sleep();
        assert!(manager.complete(&request()).is_err());
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn environment_failure_fails_over_without_retry() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let manager = ProviderManager::new(vec![
            StubProvider {
                calls: primary_calls.clone(),
                response: Err(failure(ErrorCategory::Environment, false)),
            },
            StubProvider {
                calls: fallback_calls.clone(),
                response: Ok(success()),
            },
        ])
        .without_sleep();
        manager.complete(&request()).expect("fallback response");
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
        assert_eq!(manager.health()[0].failovers, 1);
    }

    #[test]
    fn retry_after_metadata_controls_recorded_delay() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut error = failure(ErrorCategory::Transient, true);
        error.context.insert("retry_after_seconds".into(), json!(3));
        let manager = ProviderManager::new(vec![StubProvider {
            calls,
            response: Err(error),
        }])
        .with_policy(RetryPolicy {
            max_retries_per_provider: 1,
            base_delay_ms: 1,
            max_delay_ms: 5_000,
            jitter_ms: 0,
        })
        .without_sleep();
        assert!(manager.complete(&request()).is_err());
        assert_eq!(manager.health()[0].last_delay_ms, Some(3_000));
    }
}
