use std::{env, sync::OnceLock, time::Duration};

use medusa_config::{
    Config, DiscoveredModel, DiscoveryFailure, credential_environment, provider_catalog_entry,
};
use reqwest::{StatusCode, Url, blocking::Client};
use serde::Deserialize;

use crate::blocking_response_json;

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
///
/// Discovery is also attempted for compatible/local routes that expose an explicit base URL. This
/// lets desktop setup verify the actual endpoint instead of treating a syntactically valid profile
/// as a working provider connection.
pub fn discover_models(
    config: &Config,
    session_api_key: Option<&str>,
) -> Result<Vec<DiscoveredModel>, ModelDiscoveryError> {
    let provider = config.model.provider.as_str();
    let catalog = provider_catalog_entry(provider).ok_or(ModelDiscoveryError::Unsupported)?;

    let repository_endpoint = config.model.base_url.is_some();
    let base_url = config
        .model
        .base_url
        .as_deref()
        .or(catalog.base_url)
        .or_else(|| default_base_url(catalog.id))
        .ok_or(ModelDiscoveryError::Unsupported)?
        .trim_end_matches('/');
    validate_discovery_endpoint(base_url)?;
    let endpoint = format!("{base_url}/models");

    let ambient_key_allowed =
        !repository_endpoint || canonical_discovery_origin(catalog.id, base_url);
    let environment_key = ambient_key_allowed
        .then(|| {
            credential_environment(catalog.profile_provider).and_then(|name| env::var(name).ok())
        })
        .flatten();
    let api_key = session_api_key
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or(environment_key);
    if catalog.default_auth == "api-key" && api_key.is_none() {
        return Err(ModelDiscoveryError::NotAuthorized);
    }

    let mut request = discovery_client()?.get(endpoint);
    if let Some(api_key) = api_key {
        if catalog.id == "minimax"
            || catalog.id == "anthropic"
            || catalog.id == "anthropic-compatible"
        {
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
    let body = blocking_response_json::<ModelsResponse>(response)
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

fn validate_discovery_endpoint(base_url: &str) -> Result<(), ModelDiscoveryError> {
    let url = Url::parse(base_url).map_err(|_| ModelDiscoveryError::Unsupported)?;
    if url.username() != "" || url.password().is_some() || url.scheme() != "https" {
        return Err(ModelDiscoveryError::Unsupported);
    }
    Ok(())
}

fn canonical_discovery_origin(provider_id: &str, base_url: &str) -> bool {
    let expected_host = match provider_id {
        "openai" => "api.openai.com",
        "anthropic" => "api.anthropic.com",
        "minimax" => "api.minimax.io",
        _ => return false,
    };
    Url::parse(base_url).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some(expected_host)
            && url.port_or_known_default() == Some(443)
    })
}

fn discovery_client() -> Result<&'static Client, ModelDiscoveryError> {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|_| ModelDiscoveryError::Offline)?;
    let _ = CLIENT.set(client);
    CLIENT.get().ok_or(ModelDiscoveryError::Offline)
}

fn default_base_url(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "openai" => Some("https://api.openai.com/v1"),
        "anthropic" => Some("https://api.anthropic.com/v1"),
        "minimax" => Some("https://api.minimax.io/anthropic"),
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
    fn discovery_rejects_insecure_or_credentialed_endpoints() {
        assert_eq!(
            validate_discovery_endpoint("http://api.openai.com/v1"),
            Err(ModelDiscoveryError::Unsupported)
        );
        assert_eq!(
            validate_discovery_endpoint("https://user:secret@api.openai.com/v1"),
            Err(ModelDiscoveryError::Unsupported)
        );
    }

    #[test]
    fn only_canonical_origins_may_inherit_ambient_credentials() {
        assert!(canonical_discovery_origin(
            "openai",
            "https://api.openai.com/v1"
        ));
        assert!(!canonical_discovery_origin(
            "openai",
            "https://attacker.example/v1"
        ));
    }

    #[test]
    fn status_classification_distinguishes_auth_route_and_temporary_failures() {
        assert_eq!(
            classify_status(StatusCode::UNAUTHORIZED),
            ModelDiscoveryError::NotAuthorized
        );
        assert_eq!(
            classify_status(StatusCode::FORBIDDEN),
            ModelDiscoveryError::NotAuthorized
        );
        assert_eq!(
            classify_status(StatusCode::NOT_FOUND),
            ModelDiscoveryError::Unsupported
        );
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
