//! Provider routing with bounded retry, failover, response caching, and durable health snapshots.

use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_prompt_cache::{
    CacheObservation, CacheOutcome, CacheSummary, CacheTelemetry, PromptEnvelope, PromptSegment,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::{
    ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities, ProviderHealthStore,
    ProviderRouteLatencyStore, ProviderStreamEvent, RouteLatencyPolicy, RouteLatencyStats,
    latency_aware_route_order,
};

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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RouteRetryPolicy {
    pub max_retries: u8,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub jitter_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderRouteProfile {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub protocol: String,
    pub endpoint: Option<String>,
    pub auth_source: String,
    pub tool_calling: bool,
    pub streaming: bool,
    pub retry: RouteRetryPolicy,
}

impl Default for RouteRetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 1,
            base_delay_ms: 250,
            max_delay_ms: 8_000,
            jitter_ms: 100,
        }
    }
}

impl RouteRetryPolicy {
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
        exponential.saturating_add(jitter).min(self.max_delay_ms)
    }
}

/// Routes requests through configured providers using durable latency and health evidence.
pub struct ProviderManager<P> {
    providers: Vec<P>,
    profiles: Vec<ProviderRouteProfile>,
    cache: Mutex<BTreeMap<String, ModelResponse>>,
    prompt_cache: Mutex<CacheTelemetry>,
    state: ProviderHealthStore,
    latency: ProviderRouteLatencyStore,
    latency_policy: RouteLatencyPolicy,
    sleeper: fn(Duration),
}

impl<P> ProviderManager<P> {
    /// Builds a manager with isolated in-memory state authorities for tests and embedding.
    #[must_use]
    pub fn new(providers: Vec<P>) -> Self {
        Self::new_with_profiles(providers, Vec::new())
    }

    #[must_use]
    pub fn new_with_profiles(providers: Vec<P>, profiles: Vec<ProviderRouteProfile>) -> Self {
        let profiles = normalized_profiles(providers.len(), profiles);
        let state = ProviderHealthStore::in_memory(&profiles);
        let latency = ProviderRouteLatencyStore::in_memory(&profiles);
        Self::new_with_profiles_and_store(providers, profiles, state, latency)
    }

    pub fn new_with_profiles_and_user_state(
        providers: Vec<P>,
        profiles: Vec<ProviderRouteProfile>,
    ) -> MedusaResult<Self> {
        let profiles = normalized_profiles(providers.len(), profiles);
        let state = ProviderHealthStore::for_user(&profiles)?;
        let latency = ProviderRouteLatencyStore::for_user(&profiles)?;
        Ok(Self::new_with_profiles_and_store(
            providers, profiles, state, latency,
        ))
    }

    fn new_with_profiles_and_store(
        providers: Vec<P>,
        profiles: Vec<ProviderRouteProfile>,
        state: ProviderHealthStore,
        latency: ProviderRouteLatencyStore,
    ) -> Self {
        Self {
            providers,
            profiles,
            cache: Mutex::new(BTreeMap::new()),
            prompt_cache: Mutex::new(CacheTelemetry::default()),
            state,
            latency,
            latency_policy: RouteLatencyPolicy::default(),
            sleeper: thread::sleep,
        }
    }

    #[must_use]
    pub fn with_retries(mut self, retries_per_provider: u8) -> Self {
        for profile in &mut self.profiles {
            profile.retry.max_retries = retries_per_provider;
        }
        self
    }

    #[cfg(test)]
    fn with_policy(mut self, policy: RouteRetryPolicy) -> Self {
        for profile in &mut self.profiles {
            profile.retry = policy;
        }
        self
    }

    #[cfg(test)]
    fn with_latency_policy(mut self, policy: RouteLatencyPolicy) -> Self {
        self.latency_policy = policy;
        self
    }

    #[cfg(test)]
    fn without_sleep(mut self) -> Self {
        self.sleeper = |_| {};
        self
    }

    /// Returns a copy from the shared route-health authority.
    #[must_use]
    pub fn health(&self) -> Vec<ProviderHealth> {
        self.state.health().unwrap_or_default()
    }

    /// Returns durable route latency measurements in configured-route order.
    #[must_use]
    pub fn route_latency(&self) -> Vec<RouteLatencyStats> {
        self.latency.stats().unwrap_or_default()
    }

    /// Returns the configured provider position that completed the latest uncached request.
    #[must_use]
    pub fn last_completed_provider(&self) -> Option<usize> {
        self.state.last_completed_provider().unwrap_or_default()
    }

    /// Returns the total number of responses served from all managers sharing this state.
    #[must_use]
    pub fn cache_hits(&self) -> u64 {
        self.state.cache_hits().unwrap_or_default()
    }

    /// Returns aggregate prompt-prefix cache telemetry for this manager instance.
    #[must_use]
    pub fn prompt_cache_summary(&self) -> CacheSummary {
        self.prompt_cache.lock().map_or_else(
            |_| CacheTelemetry::default().summary(),
            |telemetry| telemetry.summary(),
        )
    }

    /// Returns the latest completed request snapshot from shared durable state.
    #[must_use]
    pub fn execution_status(&self) -> Option<Value> {
        let summary = self.prompt_cache_summary();
        match self.state.execution_status() {
            Ok(Some(mut status)) => {
                if let Some(object) = status.as_object_mut() {
                    object.insert(
                        "prompt_cache".to_owned(),
                        json!({
                            "requests": summary.requests,
                            "hits": summary.hits,
                            "partial_hits": summary.partial_hits,
                            "input_tokens": summary.input_tokens,
                            "cached_input_tokens": summary.cached_input_tokens,
                            "reuse_basis_points": summary.reuse_basis_points(),
                            "prefix_changes": summary.prefix_changes,
                        }),
                    );
                }
                Some(status)
            }
            Ok(None) => None,
            Err(error) => Some(json!({"state_error": error.to_string()})),
        }
    }

    fn record_attempt(&self, index: usize) -> MedusaResult<()> {
        self.state.record_attempt(index)
    }

    fn record_success(&self, index: usize) -> MedusaResult<()> {
        self.state.record_success(index)
    }

    fn record_cache_hit(&self) -> MedusaResult<()> {
        self.state.record_cache_hit()
    }

    fn record_error(&self, index: usize, error: &MedusaError) -> MedusaResult<()> {
        self.state.record_error(index, error)
    }

    fn record_retry(&self, index: usize, delay_ms: u64) -> MedusaResult<()> {
        self.state.record_retry(index, delay_ms)
    }

    fn record_failover(&self, index: usize) -> MedusaResult<()> {
        self.state.record_failover(index)
    }
}

fn normalized_profiles(
    provider_count: usize,
    mut profiles: Vec<ProviderRouteProfile>,
) -> Vec<ProviderRouteProfile> {
    profiles.truncate(provider_count);
    while profiles.len() < provider_count {
        let index = profiles.len();
        profiles.push(ProviderRouteProfile {
            id: format!("provider[{index}]"),
            provider: format!("provider-{index}"),
            model: "unspecified".to_owned(),
            protocol: "unspecified".to_owned(),
            endpoint: None,
            auth_source: "unspecified".to_owned(),
            tool_calling: true,
            streaming: false,
            retry: RouteRetryPolicy::default(),
        });
    }
    profiles
}

fn cache_validation_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn prompt_envelope(
    profile: &ProviderRouteProfile,
    request: &ModelRequest,
) -> MedusaResult<PromptEnvelope> {
    let mut segments = Vec::new();
    if !request.system.trim().is_empty() {
        segments.push(
            PromptSegment::new("system", request.system.clone(), true)
                .map_err(cache_validation_error)?,
        );
    }
    if !request.tools.is_empty() {
        segments.push(
            PromptSegment::new(
                "tools",
                serde_json::to_string(&request.tools).map_err(|error| {
                    cache_validation_error(format!("could not serialize prompt tools: {error}"))
                })?,
                true,
            )
            .map_err(cache_validation_error)?,
        );
    }
    if !request.messages.is_empty() {
        segments.push(
            PromptSegment::new(
                "messages",
                serde_json::to_string(&request.messages).map_err(|error| {
                    cache_validation_error(format!("could not serialize prompt messages: {error}"))
                })?,
                false,
            )
            .map_err(cache_validation_error)?,
        );
    }
    if segments.is_empty() {
        segments.push(
            PromptSegment::new("empty", "empty provider request", false)
                .map_err(cache_validation_error)?,
        );
    }
    let envelope = PromptEnvelope {
        schema_version: 1,
        provider: profile.provider.clone(),
        model: profile.model.clone(),
        segments,
    };
    envelope.validate().map_err(cache_validation_error)?;
    Ok(envelope)
}

fn provider_cache_outcome(usage: crate::Usage) -> CacheOutcome {
    if usage.cache_read_input_tokens > 0 {
        CacheOutcome::Hit
    } else if usage.cache_creation_input_tokens > 0 {
        CacheOutcome::Miss
    } else {
        CacheOutcome::Unknown
    }
}

fn prompt_cache_metadata(usage: crate::Usage) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "cache_creation_input_tokens".to_owned(),
            usage.cache_creation_input_tokens.to_string(),
        ),
        (
            "cache_read_input_tokens".to_owned(),
            usage.cache_read_input_tokens.to_string(),
        ),
    ])
}

impl<P: ModelProvider> ProviderManager<P> {
    fn record_prompt_cache_observation(
        &self,
        index: usize,
        request: &ModelRequest,
        usage: crate::Usage,
    ) -> MedusaResult<()> {
        let profile = self.profiles.get(index).ok_or_else(|| {
            cache_validation_error(format!("provider profile {index} is missing"))
        })?;
        let envelope = prompt_envelope(profile, request)?;
        let stable_prefix = envelope.stable_prefix();
        let rendered = envelope.rendered();
        let total_input_tokens = usage
            .input_tokens
            .saturating_add(usage.cache_read_input_tokens)
            .saturating_add(usage.cache_creation_input_tokens);
        let mut telemetry = self.prompt_cache.lock().map_err(|_| {
            MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                "prompt cache telemetry lock was poisoned",
            )
        })?;
        let sequence = telemetry.observations().len() as u64 + 1;
        telemetry
            .append(CacheObservation {
                sequence,
                recorded_at: OffsetDateTime::now_utc(),
                provider: profile.provider.clone(),
                model: profile.model.clone(),
                prefix_fingerprint: envelope.stable_prefix_fingerprint(),
                prompt_fingerprint: envelope.full_prompt_fingerprint(),
                stable_prefix_bytes: stable_prefix.len() as u64,
                prompt_bytes: rendered.len() as u64,
                input_tokens: total_input_tokens,
                cached_input_tokens: usage.cache_read_input_tokens,
                outcome: provider_cache_outcome(usage),
                provider_metadata: prompt_cache_metadata(usage),
            })
            .map_err(cache_validation_error)
    }
}

impl<P: ModelProvider> ModelProvider for ProviderManager<P> {
    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.complete_with_cancel_and_sink(request, None, None)
    }

    fn complete_streaming(
        &self,
        request: &ModelRequest,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        self.complete_with_cancel_and_sink(request, None, Some(sink))
    }

    fn complete_streaming_cancellable(
        &self,
        request: &ModelRequest,
        cancel: &AtomicBool,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        self.complete_with_cancel_and_sink(request, Some(cancel), Some(sink))
    }

    fn complete_cancellable(
        &self,
        request: &ModelRequest,
        cancel: &AtomicBool,
    ) -> MedusaResult<ModelResponse> {
        self.complete_with_cancel_and_sink(request, Some(cancel), None)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        let mut capabilities = self
            .providers
            .first()
            .map_or_else(ProviderCapabilities::default, ModelProvider::capabilities);
        capabilities.streaming = self.providers.iter().enumerate().any(|(index, provider)| {
            self.profiles
                .get(index)
                .is_some_and(|profile| profile.streaming)
                && provider.capabilities().streaming
        });
        capabilities
    }

    fn execution_status(&self) -> Option<Value> {
        ProviderManager::execution_status(self)
    }
}

impl<P: ModelProvider> ProviderManager<P> {
    fn complete_with_cancel_and_sink(
        &self,
        request: &ModelRequest,
        cancel: Option<&AtomicBool>,
        mut sink: Option<&mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>>,
    ) -> MedusaResult<ModelResponse> {
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
            self.record_cache_hit()?;
            if let Some(sink) = sink.as_deref_mut() {
                sink(ProviderStreamEvent::Completed {
                    response: response.clone(),
                })?;
            }
            return Ok(response.clone());
        }

        let stats = self.latency.stats()?;
        let route_order = latency_aware_route_order(
            &self.profiles,
            &stats,
            !request.tools.is_empty(),
            false,
            self.latency_policy,
        );
        let mut final_error = None;
        for (position, index) in route_order.iter().copied().enumerate() {
            let provider = &self.providers[index];
            let has_fallback = position + 1 < route_order.len();
            let policy = self
                .profiles
                .get(index)
                .map_or_else(RouteRetryPolicy::default, |profile| profile.retry);
            for attempt in 0..=policy.max_retries {
                self.record_attempt(index)?;
                let started = Instant::now();
                let streaming = self
                    .profiles
                    .get(index)
                    .is_some_and(|profile| profile.streaming)
                    && provider.capabilities().streaming;
                let mut first_token_ms = None;
                let mut route_stream_started = false;
                let mut stream_sink = |event: ProviderStreamEvent| {
                    if first_token_ms.is_none()
                        && matches!(event, ProviderStreamEvent::OutputStarted)
                    {
                        first_token_ms = Some(elapsed_ms(started));
                    }
                    route_stream_started = true;
                    if let Some(sink) = sink.as_deref_mut() {
                        sink(event)?;
                    }
                    Ok(())
                };
                let result = if streaming {
                    match cancel {
                        Some(flag) => {
                            provider.complete_streaming_cancellable(request, flag, &mut stream_sink)
                        }
                        None => provider.complete_streaming(request, &mut stream_sink),
                    }
                } else {
                    match cancel {
                        Some(flag) => provider.complete_cancellable(request, flag),
                        None => provider.complete(request),
                    }
                };
                match result {
                    Ok(response) => {
                        let duration_ms = elapsed_ms(started);
                        self.latency.record_success_with_first_token(
                            index,
                            duration_ms,
                            first_token_ms,
                            response.usage,
                        )?;
                        self.record_prompt_cache_observation(index, request, response.usage)?;
                        self.record_success(index)?;
                        if !streaming && let Some(sink) = sink.as_deref_mut() {
                            sink(ProviderStreamEvent::Completed {
                                response: response.clone(),
                            })?;
                        }
                        if let Ok(mut cache) = self.cache.lock() {
                            cache.insert(key.clone(), response.clone());
                        }
                        return Ok(response);
                    }
                    Err(error) => {
                        if route_stream_started {
                            return Err(error);
                        }
                        let duration_ms = elapsed_ms(started);
                        self.latency.record_failure(index, duration_ms)?;
                        self.record_error(index, &error)?;
                        final_error = Some(error.clone());
                        match classify_error(&error, has_fallback) {
                            RetryDisposition::Retry if attempt < policy.max_retries => {
                                let delay_ms = policy.delay_ms(&error, index, attempt);
                                self.record_retry(index, delay_ms)?;
                                if let Some(flag) = cancel {
                                    let deadline = Instant::now() + Duration::from_millis(delay_ms);
                                    while Instant::now() < deadline {
                                        if flag.load(Ordering::SeqCst) {
                                            self.latency.record_cancellation(index, 0)?;
                                            return Err(MedusaError::new(
                                                ErrorCode::DependencyUnavailable,
                                                ErrorCategory::Transient,
                                                "provider request cancelled",
                                            ));
                                        }
                                        thread::sleep(Duration::from_millis(25));
                                    }
                                } else {
                                    (self.sleeper)(Duration::from_millis(delay_ms));
                                }
                            }
                            RetryDisposition::Retry | RetryDisposition::Failover
                                if has_fallback =>
                            {
                                self.record_failover(index)?;
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
                "no compatible model providers are configured",
            )
        }))
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
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

    fn provider(response: MedusaResult<ModelResponse>) -> (StubProvider, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            StubProvider {
                calls: calls.clone(),
                response,
            },
            calls,
        )
    }

    fn profile(id: &str) -> ProviderRouteProfile {
        ProviderRouteProfile {
            id: id.to_owned(),
            provider: id.to_owned(),
            model: "model".to_owned(),
            protocol: "test".to_owned(),
            endpoint: None,
            auth_source: "test".to_owned(),
            tool_calling: true,
            streaming: false,
            retry: RouteRetryPolicy::default(),
        }
    }

    #[test]
    fn dynamic_messages_preserve_the_stable_prefix_fingerprint() {
        let profile = profile("cache-route");
        let first = request();
        let mut second = request();
        second.messages[0].content = vec![MessageBlock::Text {
            text: "different turn".into(),
        }];
        let first = prompt_envelope(&profile, &first).expect("first envelope");
        let second = prompt_envelope(&profile, &second).expect("second envelope");
        assert_eq!(
            first.stable_prefix_fingerprint(),
            second.stable_prefix_fingerprint()
        );
        assert_ne!(
            first.full_prompt_fingerprint(),
            second.full_prompt_fingerprint()
        );
    }

    #[test]
    fn provider_native_cache_usage_is_recorded_in_execution_status() {
        let response = ModelResponse {
            response_id: Some("cached-response".into()),
            stop_reason: Some("end_turn".into()),
            blocks: Vec::new(),
            usage: Usage {
                input_tokens: 20,
                output_tokens: 4,
                cache_read_input_tokens: 80,
                cache_creation_input_tokens: 0,
            },
        };
        let (provider, _) = provider(Ok(response));
        let manager = ProviderManager::new_with_profiles(vec![provider], vec![profile("cached")]);
        manager
            .complete(&request())
            .expect("cached provider response");
        let status = manager.execution_status().expect("execution status");
        assert_eq!(status["prompt_cache"]["requests"], json!(1));
        assert_eq!(status["prompt_cache"]["hits"], json!(1));
        assert_eq!(status["prompt_cache"]["cached_input_tokens"], json!(80));
        assert_eq!(status["prompt_cache"]["reuse_basis_points"], json!(8_000));
    }

    #[test]
    fn retryable_primary_failure_falls_back_and_caches_response() {
        let (primary, primary_calls) = provider(Err(failure(ErrorCategory::Transient, true)));
        let (fallback, fallback_calls) = provider(Ok(success()));
        let manager = ProviderManager::new(vec![primary, fallback]).without_sleep();

        manager.complete(&request()).expect("fallback response");
        let uncached = manager.execution_status().expect("uncached status");
        manager.complete(&request()).expect("cached response");
        let cached = manager.execution_status().expect("cached status");

        assert_eq!(primary_calls.load(Ordering::SeqCst), 2);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
        assert_eq!(manager.health()[0].retries, 1);
        assert_eq!(manager.health()[0].failovers, 1);
        assert_eq!(manager.health()[1].successes, 1);
        assert_eq!(manager.last_completed_provider(), Some(1));
        assert_eq!(manager.cache_hits(), 1);
        assert_eq!(uncached["provider_index"], json!(1));
        assert_eq!(uncached["cache_hit"], json!(false));
        assert_eq!(cached["provider_index"], json!(1));
        assert_eq!(cached["cache_hit"], json!(true));
        assert_eq!(cached["cache_hits"], json!(1));
        assert_eq!(manager.route_latency()[0].failures, 2);
        assert_eq!(manager.route_latency()[1].successes, 1);
    }

    #[test]
    fn primary_success_is_attributed_to_primary_provider() {
        let (primary, _) = provider(Ok(success()));
        let manager = ProviderManager::new(vec![primary]).without_sleep();

        manager.complete(&request()).expect("primary response");

        assert_eq!(manager.last_completed_provider(), Some(0));
        assert_eq!(manager.cache_hits(), 0);
        let status = manager.execution_status().expect("execution status");
        assert_eq!(status["provider_index"], json!(0));
        assert_eq!(status["attempts"], json!(1));
        assert_eq!(status["successes"], json!(1));
        assert_eq!(manager.route_latency()[0].successes, 1);
    }

    #[test]
    fn permanent_validation_failure_is_not_retried_or_failed_over() {
        let (primary, primary_calls) = provider(Err(failure(ErrorCategory::Validation, false)));
        let (fallback, fallback_calls) = provider(Ok(success()));
        let manager = ProviderManager::new(vec![primary, fallback]).without_sleep();

        assert!(manager.complete(&request()).is_err());
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
        assert_eq!(manager.last_completed_provider(), None);
        assert_eq!(manager.execution_status(), None);
    }

    #[test]
    fn environment_failure_fails_over_without_retry() {
        let (primary, primary_calls) = provider(Err(failure(ErrorCategory::Environment, false)));
        let (fallback, fallback_calls) = provider(Ok(success()));
        let manager = ProviderManager::new(vec![primary, fallback]).without_sleep();

        manager.complete(&request()).expect("fallback response");
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
        assert_eq!(manager.health()[0].failovers, 1);
        assert_eq!(manager.last_completed_provider(), Some(1));
        let status = manager.execution_status().expect("execution status");
        assert_eq!(status["provider_index"], json!(1));
        assert_eq!(status["cache_hit"], json!(false));
    }

    #[test]
    fn retry_after_metadata_controls_recorded_delay() {
        let mut error = failure(ErrorCategory::Transient, true);
        error.context.insert("retry_after_seconds".into(), json!(3));
        let (provider, _) = provider(Err(error));
        let manager = ProviderManager::new(vec![provider])
            .with_policy(RouteRetryPolicy {
                max_retries: 1,
                base_delay_ms: 1,
                max_delay_ms: 5_000,
                jitter_ms: 0,
            })
            .without_sleep();

        assert!(manager.complete(&request()).is_err());
        assert_eq!(manager.health()[0].last_delay_ms, Some(3_000));
        assert_eq!(manager.execution_status(), None);
    }

    #[test]
    fn persisted_latency_can_promote_a_faster_fallback() {
        let (slow, slow_calls) = provider(Ok(success()));
        let (fast, fast_calls) = provider(Ok(success()));
        let profiles = vec![profile("slow"), profile("fast")];
        let health = ProviderHealthStore::in_memory(&profiles);
        let latency = ProviderRouteLatencyStore::in_memory(&profiles);
        latency
            .record_success(0, 1_000, Usage::default())
            .expect("slow observation");
        latency
            .record_success(1, 10, Usage::default())
            .expect("fast observation");
        let manager = ProviderManager::new_with_profiles_and_store(
            vec![slow, fast],
            profiles,
            health,
            latency,
        )
        .with_latency_policy(RouteLatencyPolicy {
            cold_start_duration_ms: 30_000,
            failure_penalty_ms_per_mille: 10,
            max_cache_credit_ms: 0,
        });

        manager.complete(&request()).expect("fast response");

        assert_eq!(slow_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fast_calls.load(Ordering::SeqCst), 1);
        assert_eq!(manager.last_completed_provider(), Some(1));
    }
}
