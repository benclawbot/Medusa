use std::{env, time::Duration};

use medusa_config::{
    Config, DiscoveredModel, DiscoveryFailure, credential_environment, provider_catalog_entry,
};
use reqwest::{StatusCode, blocking::Client};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelDiscoveryError {
    NotAuthorized,
    Unsupported,
    TemporarilyUnavailable,
    Offline,
    InvalidResponse,
}

impl ModelDiscoveryError {
    #[must_use]
    pub const fn fallback_kind(self) -> DiscoveryFailure {
        match self {
            Self::NotAuthorized => DiscoveryFailure::NotAuthorized,
            Self::Unsupported => DiscoveryFailure::Unsupported,
            Self::TemporarilyUnavailable | Self::InvalidResponse => {
                DiscoveryFailure::TemporarilyUnavailable
            }
            Self::Offline => DiscoveryFailure::Offline,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ProviderModel>,
}

#[derive(Debug, Deserialize)]
struct ProviderModel {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
}

/// Performs provider-native `/models` discovery without issuing a billable completion request.
///
/// The caller may provide a session-only credential (for example from the Desktop credential
/// store). When it is absent, this falls back to the same provider credential environment used by
/// normal configuration. Credentials are never returned or cached by this API.
pub fn discover_models(
    config: &Config,
    session_api_key: Option<&str>,
) -> Result<Vec<DiscoveredModel>, ModelDiscoveryError> {
    let provider = config.model.provider.as_str();
    let catalog = provider_catalog_entry(provider).ok_or(ModelDiscoveryError::Unsupported)?;
    if !catalog.discover_models && !matches!(catalog.id, "openai" | "anthropic" | "minimax") {
        return Err(ModelDiscoveryError::Unsupported);
    }

    let base_url = config
        .model
        .base_url
        .as_deref()
        .or(catalog.base_url)
        .or_else(|| default_base_url(catalog.id))
        .ok_or(ModelDiscoveryError::Unsupported)?
        .trim_end_matches('/');
    let endpoint = format!("{base_url}/models");

    let environment_key = credential_environment(catalog.profile_provider)
        .and_then(|name| env::var(name).ok());
    let api_key = session_api_key
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or(environment_key);
    if catalog.default_auth == "api-key" && api_key.is_none() {
        return Err(ModelDiscoveryError::NotAuthorized);
    }

    let client = Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|_| ModelDiscoveryError::Offline)?;
    let mut request = client.get(endpoint);
    if let Some(api_key) = api_key {
        if catalog.id == "anthropic" || catalog.id == "anthropic-compatible" {
            request = request
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01");
        } else {
            request = request.bearer_auth(api_key);
        }
    }

    let response = request.send().map_err(classify_transport_error)?;
    if !response.status().is_success() {
        return Err(classify_status(response.status()));
    }
    let body = response
        .json::<ModelsResponse>()
        .map_err(|_| ModelDiscoveryError::InvalidResponse)?;
    let mut models = body
        .data
        .into_iter()
        .filter(|model| !model.id.trim().is_empty())
        .map(|model| DiscoveredModel {
            id: model.id,
            display_name: model.display_name,
        })
        .collect::<Vec<_>>();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    Ok(models)
}

fn default_base_url(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "openai" => Some("https://api.openai.com/v1"),
        "anthropic" => Some("https://api.anthropic.com/v1"),
        "minimax" => Some("https://api.minimax.io/v1"),
        _ => None,
    }
}

fn classify_status(status: StatusCode) -> ModelDiscoveryError {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ModelDiscoveryError::NotAuthorized,
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED => ModelDiscoveryError::Unsupported,
        status if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS => {
            ModelDiscoveryError::TemporarilyUnavailable
        }
        _ => ModelDiscoveryError::InvalidResponse,
    }
}

fn classify_transport_error(error: reqwest::Error) -> ModelDiscoveryError {
    if error.is_connect() || error.is_timeout() {
        ModelDiscoveryError::Offline
    } else {
        ModelDiscoveryError::TemporarilyUnavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_classification_distinguishes_auth_route_and_temporary_failures() {
        assert_eq!(classify_status(StatusCode::UNAUTHORIZED), ModelDiscoveryError::NotAuthorized);
        assert_eq!(classify_status(StatusCode::FORBIDDEN), ModelDiscoveryError::NotAuthorized);
        assert_eq!(classify_status(StatusCode::NOT_FOUND), ModelDiscoveryError::Unsupported);
        assert_eq!(
            classify_status(StatusCode::TOO_MANY_REQUESTS),
            ModelDiscoveryError::TemporarilyUnavailable
        );
        assert_eq!(
            classify_status(StatusCode::SERVICE_UNAVAILABLE),
            ModelDiscoveryError::TemporarilyUnavailable
        );
    }

    #[test]
    fn fallback_mapping_preserves_failure_semantics() {
        assert_eq!(
            ModelDiscoveryError::NotAuthorized.fallback_kind(),
            DiscoveryFailure::NotAuthorized
        );
        assert_eq!(
            ModelDiscoveryError::Offline.fallback_kind(),
            DiscoveryFailure::Offline
        );
        assert_eq!(
            ModelDiscoveryError::Unsupported.fallback_kind(),
            DiscoveryFailure::Unsupported
        );
    }
}
