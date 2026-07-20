//! Provider routing with bounded retry, failover, response caching, and health snapshots.

use std::{collections::BTreeMap, sync::Mutex};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};

use crate::{ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities};

/// Observable health state for a configured provider position.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderHealth {
    pub attempts: u64,
    pub successes: u64,
    pub last_error: Option<String>,
}

/// Routes requests through a primary provider followed by optional fallbacks.
pub struct ProviderManager<P> {
    providers: Vec<P>,
    retries_per_provider: u8,
    cache: Mutex<BTreeMap<String, ModelResponse>>,
    health: Mutex<Vec<ProviderHealth>>,
}

impl<P> ProviderManager<P> {
    /// Builds a manager with a primary provider and zero or more ordered fallbacks.
    #[must_use]
    pub fn new(providers: Vec<P>) -> Self {
        let health = vec![ProviderHealth::default(); providers.len()];
        Self {
            providers,
            retries_per_provider: 1,
            cache: Mutex::new(BTreeMap::new()),
            health: Mutex::new(health),
        }
    }

    #[must_use]
    pub fn with_retries(mut self, retries_per_provider: u8) -> Self {
        self.retries_per_provider = retries_per_provider;
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
            for attempt in 0..=self.retries_per_provider {
                if let Ok(mut health) = self.health.lock()
                    && let Some(entry) = health.get_mut(index)
                {
                    entry.attempts = entry.attempts.saturating_add(1);
                }
                match provider.complete(request) {
                    Ok(response) => {
                        if let Ok(mut health) = self.health.lock()
                            && let Some(entry) = health.get_mut(index)
                        {
                            entry.successes = entry.successes.saturating_add(1);
                            entry.last_error = None;
                        }
                        if let Ok(mut cache) = self.cache.lock() {
                            cache.insert(key, response.clone());
                        }
                        return Ok(response);
                    }
                    Err(error) => {
                        let retryable = error.retryable;
                        let message = error.to_string();
                        if let Ok(mut health) = self.health.lock()
                            && let Some(entry) = health.get_mut(index)
                        {
                            entry.last_error = Some(message);
                        }
                        final_error = Some(error);
                        if !retryable || attempt == self.retries_per_provider {
                            break;
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

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use medusa_core::{ErrorCategory, ErrorCode, MedusaError};

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

    #[test]
    fn retryable_primary_failure_falls_back_and_caches_the_response() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let primary = StubProvider {
            calls: primary_calls.clone(),
            response: Err(MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Transient,
                "offline",
            )
            .with_retryable(true)),
        };
        let fallback = StubProvider {
            calls: fallback_calls.clone(),
            response: Ok(success()),
        };
        let manager = ProviderManager::new(vec![primary, fallback]);
        manager.complete(&request()).expect("fallback response");
        manager.complete(&request()).expect("cached response");
        assert_eq!(primary_calls.load(Ordering::SeqCst), 2);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
        assert_eq!(manager.health()[1].successes, 1);
    }
}
