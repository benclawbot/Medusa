from pathlib import Path


def replace(path: str, old: str, new: str) -> None:
    target = Path(path)
    text = target.read_text()
    if new in text:
        return
    if old not in text:
        raise SystemExit(f"expected source fragment missing in {path}")
    target.write_text(text.replace(old, new, 1))


CARGO = "crates/medusa-provider/Cargo.toml"
MANAGER = "crates/medusa-provider/src/manager.rs"
ANTHROPIC = "crates/medusa-provider/src/anthropic.rs"

replace(
    CARGO,
    "medusa-core = { path = \"../medusa-core\" }\n",
    "medusa-core = { path = \"../medusa-core\" }\nmedusa-prompt-cache = { path = \"../medusa-prompt-cache\" }\n",
)
replace(CARGO, "tokio.workspace = true\n", "tokio.workspace = true\ntime.workspace = true\n")

replace(
    MANAGER,
    "use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};\n",
    "use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};\nuse medusa_prompt_cache::{\n    CacheObservation, CacheOutcome, CacheSummary, CacheTelemetry, PromptEnvelope, PromptSegment,\n};\n",
)
replace(
    MANAGER,
    "use serde_json::{Value, json};\n",
    "use serde_json::{Map, Value, json};\nuse time::OffsetDateTime;\n",
)

replace(
    MANAGER,
    '''    cache: Mutex<BTreeMap<String, ModelResponse>>,
    state: ProviderHealthStore,''',
    '''    cache: Mutex<BTreeMap<String, ModelResponse>>,
    prompt_cache: Mutex<CacheTelemetry>,
    state: ProviderHealthStore,''',
)
replace(
    MANAGER,
    '''            cache: Mutex::new(BTreeMap::new()),
            state,''',
    '''            cache: Mutex::new(BTreeMap::new()),
            prompt_cache: Mutex::new(CacheTelemetry::default()),
            state,''',
)

replace(
    MANAGER,
    '''    /// Returns the latest completed request snapshot from shared durable state.
    #[must_use]
    pub fn execution_status(&self) -> Option<Value> {
        match self.state.execution_status() {
            Ok(status) => status,
            Err(error) => Some(json!({"state_error": error.to_string()})),
        }
    }
''',
    '''    /// Returns aggregate prompt-prefix cache telemetry for this manager instance.
    #[must_use]
    pub fn prompt_cache_summary(&self) -> CacheSummary {
        self.prompt_cache
            .lock()
            .map_or_else(|_| CacheTelemetry::default().summary(), |telemetry| telemetry.summary())
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
''',
)

# Insert prompt cache helpers before the ModelProvider impl.
replace(
    MANAGER,
    '''impl<P: ModelProvider> ModelProvider for ProviderManager<P> {''',
    '''fn cache_validation_error(message: impl Into<String>) -> MedusaError {
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

impl<P: ModelProvider> ModelProvider for ProviderManager<P> {''',
)

replace(
    MANAGER,
    '''                        self.latency.record_success_with_first_token(
                            index,
                            duration_ms,
                            first_token_ms,
                            response.usage,
                        )?;
                        self.record_success(index)?;''',
    '''                        self.latency.record_success_with_first_token(
                            index,
                            duration_ms,
                            first_token_ms,
                            response.usage,
                        )?;
                        self.record_prompt_cache_observation(index, request, response.usage)?;
                        self.record_success(index)?;''',
)

# Avoid unused Map import by using it for Anthropic-compatible tool shaping in a helper test below.
replace(MANAGER, "use serde_json::{Map, Value, json};", "use serde_json::{Value, json};")

# Add manager tests before the first existing test.
replace(
    MANAGER,
    '''    #[test]
    fn retryable_primary_failure_falls_back_and_caches_response() {''',
    '''    #[test]
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
        assert_ne!(first.full_prompt_fingerprint(), second.full_prompt_fingerprint());
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
        manager.complete(&request()).expect("cached provider response");
        let status = manager.execution_status().expect("execution status");
        assert_eq!(status["prompt_cache"]["requests"], json!(1));
        assert_eq!(status["prompt_cache"]["hits"], json!(1));
        assert_eq!(status["prompt_cache"]["cached_input_tokens"], json!(80));
        assert_eq!(status["prompt_cache"]["reuse_basis_points"], json!(8_000));
    }

    #[test]
    fn retryable_primary_failure_falls_back_and_caches_response() {''',
)

# Anthropic native cache breakpoints: cache the stable system prefix and, when tools exist, the
# last tool definition so the system+tool prefix is reusable across turn-message changes.
replace(
    ANTHROPIC,
    '''    fn request_body(&self, request: &ModelRequest) -> Value {
        json!({
            "model": self.model,
            "system": request.system,
            "messages": request.messages,
            "tools": request.tools,
            "max_tokens": request.max_tokens,
            "temperature": f64::from(request.temperature_milli) / 1000.0,
            "stream": false
        })
    }
''',
    '''    fn request_body(&self, request: &ModelRequest) -> Value {
        let mut tools = serde_json::to_value(&request.tools).unwrap_or_else(|_| json!([]));
        if let Some(last) = tools.as_array_mut().and_then(|items| items.last_mut())
            && let Some(object) = last.as_object_mut()
        {
            object.insert("cache_control".to_owned(), json!({"type": "ephemeral"}));
        }
        json!({
            "model": self.model,
            "system": [{
                "type": "text",
                "text": request.system,
                "cache_control": {"type": "ephemeral"}
            }],
            "messages": request.messages,
            "tools": tools,
            "max_tokens": request.max_tokens,
            "temperature": f64::from(request.temperature_milli) / 1000.0,
            "stream": false
        })
    }
''',
)
replace(
    ANTHROPIC,
    '''    #[test]
    fn thinking_is_not_exposed_or_persisted() {''',
    '''    #[test]
    fn request_marks_stable_system_and_tool_prefix_for_native_caching() {
        let mut config = Config::default();
        config.model.provider = "anthropic".to_owned();
        let provider = MiniMaxProvider {
            blocking_client: shared_blocking_http_client().expect("blocking client"),
            async_client: shared_async_http_client().expect("async client"),
            base_url: "https://example.invalid".to_owned(),
            api_key: "test".to_owned(),
            model: "test-model".to_owned(),
            capabilities: anthropic_capabilities(),
        };
        let mut request = empty_request();
        request.tools.push(crate::ToolDefinition {
            name: "fs_read".to_owned(),
            description: "read".to_owned(),
            input_schema: json!({"type": "object"}),
        });
        let body = provider.request_body(&request);
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn thinking_is_not_exposed_or_persisted() {''',
)
