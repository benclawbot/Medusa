from pathlib import Path


def replace(path: str, old: str, new: str) -> None:
    target = Path(path)
    text = target.read_text()
    if old not in text:
        raise SystemExit(f"expected source fragment missing in {path}: {old[:80]!r}")
    target.write_text(text.replace(old, new, 1))

MANAGER = "crates/medusa-provider/src/manager.rs"
OPENAI = "crates/medusa-provider/src/openai.rs"

replace(
    MANAGER,
    '''fn provider_cache_outcome(usage: crate::Usage) -> CacheOutcome {
    if usage.cache_read_input_tokens > 0 {
        CacheOutcome::Hit
    } else if usage.cache_creation_input_tokens > 0 {
        CacheOutcome::Miss
    } else {
        CacheOutcome::Unknown
    }
}
''',
    '''fn provider_input_tokens(profile: &ProviderRouteProfile, usage: crate::Usage) -> u64 {
    if profile.protocol.eq_ignore_ascii_case("openai") {
        usage.input_tokens
    } else {
        usage
            .input_tokens
            .saturating_add(usage.cache_read_input_tokens)
            .saturating_add(usage.cache_creation_input_tokens)
    }
}

fn provider_cache_outcome(total_input_tokens: u64, usage: crate::Usage) -> CacheOutcome {
    if usage.cache_read_input_tokens > 0 {
        if usage.cache_read_input_tokens >= total_input_tokens {
            CacheOutcome::Hit
        } else {
            CacheOutcome::PartialHit
        }
    } else if usage.cache_creation_input_tokens > 0 {
        CacheOutcome::Miss
    } else {
        CacheOutcome::Unknown
    }
}
''',
)
replace(
    MANAGER,
    '''        let total_input_tokens = usage
            .input_tokens
            .saturating_add(usage.cache_read_input_tokens)
            .saturating_add(usage.cache_creation_input_tokens);
''',
    '''        let total_input_tokens = provider_input_tokens(profile, usage);
''',
)
replace(
    MANAGER,
    '''                outcome: provider_cache_outcome(usage),
''',
    '''                outcome: provider_cache_outcome(total_input_tokens, usage),
''',
)
replace(
    MANAGER,
    '''                        self.record_prompt_cache_observation(index, request, response.usage)?;
                        self.record_success(index)?;
''',
    '''                        // Prompt-cache telemetry is observational. A clock adjustment,
                        // poisoned telemetry lock, or malformed telemetry must never discard a
                        // provider response that already completed successfully (and may already
                        // have streamed output to the caller).
                        let _ = self.record_prompt_cache_observation(index, request, response.usage);
                        self.record_success(index)?;
''',
)
replace(
    MANAGER,
    '''        assert_eq!(status["prompt_cache"]["hits"], json!(1));
        assert_eq!(status["prompt_cache"]["cached_input_tokens"], json!(80));
''',
    '''        assert_eq!(status["prompt_cache"]["hits"], json!(0));
        assert_eq!(status["prompt_cache"]["partial_hits"], json!(1));
        assert_eq!(status["prompt_cache"]["cached_input_tokens"], json!(80));
''',
)
replace(
    MANAGER,
    '''    #[test]
    fn retryable_primary_failure_falls_back_and_caches_response() {''',
    '''    #[test]
    fn openai_prompt_total_is_not_double_counted_with_cached_tokens() {
        let mut openai = profile("openai-cache");
        openai.protocol = "openai".to_owned();
        let usage = Usage {
            input_tokens: 100,
            cache_read_input_tokens: 90,
            ..Usage::default()
        };
        assert_eq!(provider_input_tokens(&openai, usage), 100);
        assert_eq!(provider_cache_outcome(100, usage), CacheOutcome::PartialHit);
    }

    #[test]
    fn telemetry_failure_does_not_discard_successful_provider_response() {
        let (provider, _) = provider(Ok(success()));
        let manager = ProviderManager::new(vec![provider]);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = manager.prompt_cache.lock().expect("prompt cache lock");
            panic!("poison cache telemetry lock");
        }));
        manager
            .complete(&request())
            .expect("provider success survives telemetry failure");
        assert_eq!(manager.health()[0].successes, 1);
    }

    #[test]
    fn retryable_primary_failure_falls_back_and_caches_response() {''',
)

replace(
    OPENAI,
    '''#[derive(Debug, Default, Deserialize)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}
''',
    '''#[derive(Debug, Default, Deserialize)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: OpenAiPromptTokenDetails,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAiPromptTokenDetails {
    #[serde(default)]
    cached_tokens: u64,
}
''',
)
replace(
    OPENAI,
    '''            usage: Usage {
                input_tokens: self.usage.prompt_tokens,
                output_tokens: self.usage.completion_tokens,
                ..Usage::default()
            },
''',
    '''            usage: Usage {
                input_tokens: self.usage.prompt_tokens,
                output_tokens: self.usage.completion_tokens,
                cache_read_input_tokens: self.usage.prompt_tokens_details.cached_tokens,
                ..Usage::default()
            },
''',
)
replace(
    OPENAI,
    '''    #[test]
    fn configured_streaming_never_exceeds_wire_support() {''',
    '''    #[test]
    fn non_streaming_usage_preserves_openai_cached_prompt_tokens() {
        let wire: OpenAiWireResponse = serde_json::from_value(json!({
            "id": "response-1",
            "choices": [{
                "message": {"content": "ok", "tool_calls": []},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 4,
                "prompt_tokens_details": {"cached_tokens": 90}
            }
        }))
        .expect("wire response");
        let response = wire.into_model_response().expect("model response");
        assert_eq!(response.usage.input_tokens, 100);
        assert_eq!(response.usage.cache_read_input_tokens, 90);
    }

    #[test]
    fn configured_streaming_never_exceeds_wire_support() {''',
)
