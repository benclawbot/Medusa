use std::sync::atomic::AtomicBool;

use medusa_config::{Config, FallbackProviderConfig};
use medusa_core::MedusaResult;
use serde_json::Value;

use crate::{
    MiniMaxProvider, ModelProvider, ModelRequest, ModelResponse, OpenAiProvider,
    ProviderCapabilities, ProviderManager, ProviderRouteProfile, ProviderStreamEvent,
    RouteRetryPolicy,
};

/// Runtime-selected provider supporting Anthropic and OpenAI-compatible APIs.
pub enum ConfiguredProvider {
    Anthropic(MiniMaxProvider),
    OpenAi(OpenAiProvider),
}

impl ConfiguredProvider {
    pub fn from_config(config: &Config) -> MedusaResult<Self> {
        Self::from_config_with_api_key(config, None)
    }

    pub fn from_config_with_api_key(
        config: &Config,
        session_api_key: Option<String>,
    ) -> MedusaResult<Self> {
        if config.model.protocol.eq_ignore_ascii_case("openai") {
            Ok(Self::OpenAi(OpenAiProvider::from_config_with_api_key(
                config,
                session_api_key,
            )?))
        } else {
            Ok(Self::Anthropic(MiniMaxProvider::from_config_with_api_key(
                config,
                session_api_key,
            )?))
        }
    }

    /// Builds the configured primary provider plus ordered, self-contained fallback routes.
    pub fn manager_from_config(
        config: &Config,
        session_api_key: Option<String>,
    ) -> MedusaResult<ProviderManager<Self>> {
        let primary = Self::from_config_with_api_key(config, session_api_key)?;
        let primary_capabilities = primary.capabilities();
        let mut providers = vec![primary];
        let mut profiles = vec![route_profile(
            "primary",
            &config.model.provider,
            &config.model.name,
            &config.model.protocol,
            config.model.base_url.as_deref(),
            &config.model.auth,
            &primary_capabilities,
            config.model.max_retries,
            config.model.retry_base_delay_ms,
            config.model.retry_max_delay_ms,
            config.model.retry_jitter_ms,
        )];

        for (index, fallback) in config.model.fallback_providers.iter().enumerate() {
            let fallback_config = config_for_fallback(config, fallback);
            let provider =
                Self::from_config_with_api_key(&fallback_config, None).map_err(|mut error| {
                    error
                        .context
                        .insert("fallback_index".to_owned(), Value::from(index as u64));
                    error.context.insert(
                        "provider".to_owned(),
                        Value::from(fallback.provider.clone()),
                    );
                    error
                        .context
                        .insert("model".to_owned(), Value::from(fallback.name.clone()));
                    error
                })?;
            let capabilities = provider.capabilities();
            providers.push(provider);
            profiles.push(route_profile(
                &format!("fallback[{index}]"),
                &fallback.provider,
                &fallback.name,
                &fallback.protocol,
                fallback.base_url.as_deref(),
                &fallback.auth,
                &capabilities,
                fallback.max_retries,
                fallback.retry_base_delay_ms,
                fallback.retry_max_delay_ms,
                fallback.retry_jitter_ms,
            ));
        }

        ProviderManager::new_with_profiles_and_user_state(providers, profiles)
    }
}

impl ModelProvider for ConfiguredProvider {
    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        match self {
            Self::Anthropic(provider) => provider.complete(request),
            Self::OpenAi(provider) => provider.complete(request),
        }
    }

    fn complete_streaming(
        &self,
        request: &ModelRequest,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        match self {
            Self::Anthropic(provider) => provider.complete_streaming(request, sink),
            Self::OpenAi(provider) => provider.complete_streaming(request, sink),
        }
    }

    fn complete_streaming_cancellable(
        &self,
        request: &ModelRequest,
        cancel: &AtomicBool,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        match self {
            Self::Anthropic(provider) => {
                provider.complete_streaming_cancellable(request, cancel, sink)
            }
            Self::OpenAi(provider) => {
                provider.complete_streaming_cancellable(request, cancel, sink)
            }
        }
    }

    fn complete_cancellable(
        &self,
        request: &ModelRequest,
        cancel: &AtomicBool,
    ) -> MedusaResult<ModelResponse> {
        match self {
            Self::Anthropic(provider) => provider.complete_cancellable(request, cancel),
            Self::OpenAi(provider) => provider.complete_cancellable(request, cancel),
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        match self {
            Self::Anthropic(provider) => provider.capabilities(),
            Self::OpenAi(provider) => provider.capabilities(),
        }
    }
}

fn config_for_fallback(config: &Config, fallback: &FallbackProviderConfig) -> Config {
    let mut route = config.clone();
    route.model.provider = fallback.provider.clone();
    route.model.name = fallback.name.clone();
    route.model.protocol = fallback.protocol.clone();
    route.model.base_url = fallback.base_url.clone();
    route.model.auth = fallback.auth.clone();
    route.model.tool_calling = fallback.tool_calling;
    route.model.streaming = fallback.streaming;
    route.model.max_retries = fallback.max_retries;
    route.model.retry_base_delay_ms = fallback.retry_base_delay_ms;
    route.model.retry_max_delay_ms = fallback.retry_max_delay_ms;
    route.model.retry_jitter_ms = fallback.retry_jitter_ms;
    route.model.fallback_providers.clear();
    route
}

#[allow(clippy::too_many_arguments)]
fn route_profile(
    id: &str,
    provider: &str,
    model: &str,
    protocol: &str,
    endpoint: Option<&str>,
    auth: &str,
    capabilities: &ProviderCapabilities,
    max_retries: u8,
    base_delay_ms: u64,
    max_delay_ms: u64,
    jitter_ms: u64,
) -> ProviderRouteProfile {
    ProviderRouteProfile {
        id: id.to_owned(),
        provider: provider.to_owned(),
        model: model.to_owned(),
        protocol: protocol.to_owned(),
        endpoint: endpoint.map(str::to_owned),
        auth_source: auth.to_owned(),
        tool_calling: capabilities.tool_calling,
        streaming: capabilities.streaming,
        retry: RouteRetryPolicy {
            max_retries,
            base_delay_ms,
            max_delay_ms,
            jitter_ms,
        },
    }
}
