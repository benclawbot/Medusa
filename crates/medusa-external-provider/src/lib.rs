//! Truthful provider route authority layered over the existing model adapters.
//!
//! Routes are represented without constructing credentials or transports. The primary and each
//! fallback are initialized only when selected by [`ProviderManager`]. Capability projection is
//! conservative: the current blocking adapters never advertise streaming or transport-level
//! cancellation, regardless of configuration wishes.

use std::{
    env,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use medusa_config::{Config, FallbackProviderConfig};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_external_contracts::{
    ProviderCapabilitySet, ReadinessCheck, ReadinessStage, RouteIdentity, RouteReadiness,
};
use medusa_provider::{
    ConfiguredProvider, ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities,
    ProviderManager, ProviderRouteProfile, RouteRetryPolicy,
};
use serde_json::Value;

#[derive(Clone)]
struct LazyRoute {
    state: Arc<RouteState>,
}

struct RouteState {
    route_id: String,
    config: Config,
    session_api_key: Option<String>,
    provider: OnceLock<MedusaResult<ConfiguredProvider>>,
    live_verified: AtomicBool,
}

impl RouteState {
    fn initialize(&self) -> MedusaResult<&ConfiguredProvider> {
        match self.provider.get_or_init(|| {
            ConfiguredProvider::from_config_with_api_key(
                &self.config,
                self.session_api_key.clone(),
            )
        }) {
            Ok(provider) => Ok(provider),
            Err(error) => Err(error.clone()),
        }
    }

    fn legacy_capabilities(&self) -> ProviderCapabilities {
        let capabilities = truthful_capabilities(&self.config);
        ProviderCapabilities {
            image_input: capabilities.image_input,
            supported_image_media_types: capabilities.supported_image_media_types,
            max_image_bytes: capabilities.max_image_bytes,
            max_images_per_request: capabilities.max_images_per_request,
            tool_calling: capabilities.tool_calling,
            streaming: false,
        }
    }

    fn readiness(&self) -> MedusaResult<RouteReadiness> {
        let capabilities = truthful_capabilities(&self.config);
        let mut checks = vec![ReadinessCheck::ready(ReadinessStage::ProfileSaved)];
        if credential_present(&self.config, self.session_api_key.as_deref()) {
            checks.push(ReadinessCheck::ready(ReadinessStage::SecretPresent));
        } else {
            checks.push(ReadinessCheck::unavailable(
                ReadinessStage::SecretPresent,
                missing_credential_reason(&self.config),
            ));
            return RouteReadiness::new(self.identity(), capabilities, checks)
                .map_err(contract_error);
        }
        if self.live_verified.load(Ordering::SeqCst) {
            checks.extend([
                ReadinessCheck::ready(ReadinessStage::EndpointReachable),
                ReadinessCheck::ready(ReadinessStage::Authenticated),
                ReadinessCheck::ready(ReadinessStage::CapabilityAvailable),
                ReadinessCheck::ready(ReadinessStage::LiveRequestVerified),
            ]);
        } else {
            checks.push(ReadinessCheck::unavailable(
                ReadinessStage::EndpointReachable,
                "route has not completed a live request",
            ));
        }
        RouteReadiness::new(self.identity(), capabilities, checks).map_err(contract_error)
    }

    fn identity(&self) -> RouteIdentity {
        RouteIdentity {
            route_id: self.route_id.clone(),
            provider: self.config.model.provider.clone(),
            model: self.config.model.name.clone(),
            protocol: self.config.model.protocol.clone(),
            endpoint_origin: route_endpoint(&self.config),
            auth_source: self.config.model.auth.clone(),
        }
    }

    fn profile(&self) -> ProviderRouteProfile {
        let capabilities = truthful_capabilities(&self.config);
        ProviderRouteProfile {
            id: self.route_id.clone(),
            provider: self.config.model.provider.clone(),
            model: self.config.model.name.clone(),
            protocol: self.config.model.protocol.clone(),
            endpoint: Some(route_endpoint(&self.config)),
            auth_source: self.config.model.auth.clone(),
            tool_calling: capabilities.tool_calling,
            streaming: false,
            retry: RouteRetryPolicy {
                max_retries: self.config.model.max_retries,
                base_delay_ms: self.config.model.retry_base_delay_ms,
                max_delay_ms: self.config.model.retry_max_delay_ms,
                jitter_ms: self.config.model.retry_jitter_ms,
            },
        }
    }
}

impl ModelProvider for LazyRoute {
    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        let response = self.state.initialize()?.complete(request)?;
        self.state.live_verified.store(true, Ordering::SeqCst);
        Ok(response)
    }

    fn complete_cancellable(
        &self,
        request: &ModelRequest,
        cancel: &AtomicBool,
    ) -> MedusaResult<ModelResponse> {
        let response = self
            .state
            .initialize()?
            .complete_cancellable(request, cancel)?;
        self.state.live_verified.store(true, Ordering::SeqCst);
        Ok(response)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.state.legacy_capabilities()
    }

    fn execution_status(&self) -> Option<Value> {
        self.state
            .provider
            .get()
            .and_then(|provider| provider.as_ref().ok())
            .and_then(ModelProvider::execution_status)
    }
}

/// Lazy provider manager with versioned route-readiness projection.
pub struct LazyConfiguredProviderManager {
    manager: ProviderManager<LazyRoute>,
    routes: Vec<Arc<RouteState>>,
}

impl LazyConfiguredProviderManager {
    pub fn from_config(
        config: &Config,
        session_api_key: Option<String>,
    ) -> MedusaResult<Self> {
        let mut route_configs = vec![(
            "primary".to_owned(),
            config.clone(),
            session_api_key,
        )];
        route_configs.extend(
            config
                .model
                .fallback_providers
                .iter()
                .enumerate()
                .map(|(index, fallback)| {
                    (
                        format!("fallback[{index}]"),
                        config_for_fallback(config, fallback),
                        None,
                    )
                }),
        );
        let routes = route_configs
            .into_iter()
            .map(|(route_id, config, session_api_key)| {
                Arc::new(RouteState {
                    route_id,
                    config,
                    session_api_key,
                    provider: OnceLock::new(),
                    live_verified: AtomicBool::new(false),
                })
            })
            .collect::<Vec<_>>();
        let providers = routes
            .iter()
            .cloned()
            .map(|state| LazyRoute { state })
            .collect();
        let profiles = routes.iter().map(|route| route.profile()).collect();
        Ok(Self {
            manager: ProviderManager::new_with_profiles(providers, profiles),
            routes,
        })
    }

    pub fn route_readiness(&self) -> MedusaResult<Vec<RouteReadiness>> {
        self.routes.iter().map(|route| route.readiness()).collect()
    }

    #[must_use]
    pub fn initialized_routes(&self) -> usize {
        self.routes
            .iter()
            .filter(|route| route.provider.get().is_some())
            .count()
    }

    #[must_use]
    pub fn health(&self) -> Vec<medusa_provider::ProviderHealth> {
        self.manager.health()
    }
}

impl ModelProvider for LazyConfiguredProviderManager {
    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.manager.complete(request)
    }

    fn complete_cancellable(
        &self,
        request: &ModelRequest,
        cancel: &AtomicBool,
    ) -> MedusaResult<ModelResponse> {
        self.manager.complete_cancellable(request, cancel)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.manager.capabilities()
    }

    fn execution_status(&self) -> Option<Value> {
        self.manager.execution_status()
    }
}

fn truthful_capabilities(config: &Config) -> ProviderCapabilitySet {
    let provider = config.model.provider.trim().to_ascii_lowercase();
    let protocol = config.model.protocol.trim().to_ascii_lowercase();
    let image_input = provider == "openai"
        || provider == "anthropic"
        || config.model.auth.eq_ignore_ascii_case("chatgpt-oauth")
        || (provider == "minimax" && enabled("MINIMAX_IMAGE_INPUT"));
    ProviderCapabilitySet {
        image_input,
        tool_calling: config.model.tool_calling
            && matches!(
                protocol.as_str(),
                "anthropic" | "openai" | "anthropic-compatible"
            ),
        streaming_text: false,
        streaming_audio: false,
        cancellation: false,
        supported_image_media_types: if image_input {
            vec![
                "image/png".to_owned(),
                "image/jpeg".to_owned(),
                "image/webp".to_owned(),
                "image/gif".to_owned(),
            ]
        } else {
            Vec::new()
        },
        max_image_bytes: image_input.then_some(20 * 1024 * 1024),
        max_images_per_request: image_input.then_some(if provider == "anthropic" { 20 } else { 10 }),
    }
}

fn credential_present(config: &Config, session_api_key: Option<&str>) -> bool {
    if config.model.auth.eq_ignore_ascii_case("none") {
        return true;
    }
    if session_api_key.is_some_and(|value| !value.is_empty()) {
        return true;
    }
    credential_variables(config)
        .into_iter()
        .any(|name| env::var(name).is_ok_and(|value| !value.is_empty()))
}

fn credential_variables(config: &Config) -> Vec<String> {
    let provider = config
        .model
        .provider
        .trim()
        .to_ascii_uppercase()
        .replace('-', "_");
    if config.model.protocol.eq_ignore_ascii_case("openai") {
        vec![
            format!("{provider}_API_KEY"),
            "OPENAI_API_KEY".to_owned(),
            "MEDUSA_API_KEY".to_owned(),
        ]
    } else {
        vec![format!("{provider}_API_KEY")]
    }
}

fn missing_credential_reason(config: &Config) -> String {
    format!(
        "no session credential or environment credential was found in {}",
        credential_variables(config).join(", ")
    )
}

fn route_endpoint(config: &Config) -> String {
    config.model.base_url.clone().unwrap_or_else(|| {
        if config.model.protocol.eq_ignore_ascii_case("openai") {
            if config.model.provider.eq_ignore_ascii_case("minimax") {
                "https://api.minimax.io/v1".to_owned()
            } else {
                "https://api.openai.com/v1".to_owned()
            }
        } else if config.model.provider.eq_ignore_ascii_case("anthropic") {
            "https://api.anthropic.com".to_owned()
        } else {
            "https://api.minimax.io/anthropic".to_owned()
        }
    })
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

fn enabled(name: &str) -> bool {
    env::var(name)
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes"))
}

fn contract_error(error: medusa_external_contracts::ContractError) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construction_is_lazy_for_primary_and_fallbacks() {
        let mut config = Config::default();
        config.model.provider = "unsupported-primary".to_owned();
        config.model.fallback_providers = vec![FallbackProviderConfig {
            provider: "unsupported-fallback".to_owned(),
            name: "fallback-model".to_owned(),
            protocol: "openai".to_owned(),
            base_url: Some("https://fallback.example/v1".to_owned()),
            auth: "api-key".to_owned(),
            tool_calling: true,
            streaming: true,
            max_retries: 1,
            retry_base_delay_ms: 10,
            retry_max_delay_ms: 100,
            retry_jitter_ms: 5,
        }];
        let manager = LazyConfiguredProviderManager::from_config(&config, None).unwrap();
        assert_eq!(manager.initialized_routes(), 0);
        assert_eq!(manager.route_readiness().unwrap().len(), 2);
    }

    #[test]
    fn configuration_cannot_invent_streaming_or_cancellation() {
        let mut config = Config::default();
        config.model.streaming = true;
        let manager = LazyConfiguredProviderManager::from_config(
            &config,
            Some("session-key".to_owned()),
        )
        .unwrap();
        let readiness = manager.route_readiness().unwrap();
        assert!(!readiness[0].capabilities.streaming_text);
        assert!(!readiness[0].capabilities.cancellation);
        assert!(!manager.capabilities().streaming);
    }

    #[test]
    fn saved_profile_and_secret_are_not_live_readiness() {
        let config = Config::default();
        let manager = LazyConfiguredProviderManager::from_config(
            &config,
            Some("session-key".to_owned()),
        )
        .unwrap();
        let readiness = manager.route_readiness().unwrap();
        assert!(readiness[0].stage_ready(ReadinessStage::ProfileSaved));
        assert!(readiness[0].stage_ready(ReadinessStage::SecretPresent));
        assert!(!readiness[0].stage_ready(ReadinessStage::EndpointReachable));
        assert!(!readiness[0].ready_for_requests());
    }
}
