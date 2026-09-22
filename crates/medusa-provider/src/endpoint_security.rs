use std::{env, net::IpAddr};

use medusa_config::ProviderProfileCatalog;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::Url;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EndpointSource {
    RepositoryConfig,
    Environment,
    Default,
}

pub(crate) fn validate_provider_endpoint(base_url: &str) -> MedusaResult<()> {
    validate_provider_endpoint_with_policy(
        base_url,
        env_flag("MEDUSA_ALLOW_INSECURE_PROVIDER_HTTP"),
    )
}

pub(crate) fn validate_provider_endpoint_with_policy(
    base_url: &str,
    allow_insecure_loopback: bool,
) -> MedusaResult<()> {
    let url = parse_provider_endpoint(base_url)?;
    validate_parsed_provider_endpoint(&url, allow_insecure_loopback)
}

pub(crate) fn configured_endpoint_source(
    base_url: &str,
    environment_names: &[&str],
) -> EndpointSource {
    if environment_names
        .iter()
        .any(|name| env::var(name).is_ok_and(|value| value == base_url))
    {
        return EndpointSource::Environment;
    }
    if ProviderProfileCatalog::user()
        .and_then(|catalog| catalog.active_store())
        .and_then(|store| store.load())
        .ok()
        .and_then(|profile| profile.base_url)
        .is_some_and(|value| value == base_url)
    {
        return EndpointSource::Environment;
    }
    EndpointSource::RepositoryConfig
}

pub(crate) fn ambient_credential_allowed(
    source: EndpointSource,
    base_url: &str,
    canonical_host: Option<&str>,
) -> bool {
    source != EndpointSource::RepositoryConfig
        || canonical_host.is_some_and(|host| canonical_https_origin(base_url, host))
}

pub(crate) fn canonical_https_origin(base_url: &str, expected_host: &str) -> bool {
    Url::parse(base_url).is_ok_and(|url| {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.host_str() == Some(expected_host)
            && url.port_or_known_default() == Some(443)
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

fn parse_provider_endpoint(base_url: &str) -> MedusaResult<Url> {
    let url = Url::parse(base_url).map_err(|error| {
        MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Validation,
            format!("invalid provider base_url: {error}"),
        )
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Validation,
            "provider base_url must not contain embedded credentials",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Validation,
            "provider base_url must not contain a query string or fragment",
        ));
    }
    Ok(url)
}

fn validate_parsed_provider_endpoint(url: &Url, allow_insecure_loopback: bool) -> MedusaResult<()> {
    if url.scheme() == "https" {
        return Ok(());
    }
    if url.scheme() == "http" && is_loopback_url(url) && allow_insecure_loopback {
        return Ok(());
    }
    Err(MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Validation,
        "provider base_url must use HTTPS; loopback HTTP requires MEDUSA_ALLOW_INSECURE_PROVIDER_HTTP=1",
    ))
}

fn is_loopback_url(url: &Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_http_is_rejected() {
        let error = validate_provider_endpoint_with_policy("http://example.com/v1", true)
            .expect_err("remote HTTP must fail");
        assert!(error.to_string().contains("HTTPS"));
    }

    #[test]
    fn loopback_http_requires_explicit_opt_in() {
        assert!(validate_provider_endpoint_with_policy("http://127.0.0.1:8080/v1", false).is_err());
        validate_provider_endpoint_with_policy("http://127.0.0.1:8080/v1", true)
            .expect("explicit loopback development opt-in");
    }

    #[test]
    fn embedded_endpoint_credentials_are_rejected() {
        let error =
            validate_provider_endpoint_with_policy("https://user:password@example.com/v1", false)
                .expect_err("embedded credentials must fail");
        assert!(error.to_string().contains("embedded credentials"));
    }

    #[test]
    fn endpoint_query_and_fragment_are_rejected() {
        assert!(
            validate_provider_endpoint_with_policy("https://example.com/v1?token=secret", false)
                .is_err()
        );
        assert!(
            validate_provider_endpoint_with_policy("https://example.com/v1#secret", false).is_err()
        );
    }

    #[test]
    fn repository_endpoint_only_inherits_credentials_for_canonical_origin() {
        assert!(ambient_credential_allowed(
            EndpointSource::RepositoryConfig,
            "https://api.openai.com/v1",
            Some("api.openai.com"),
        ));
        assert!(!ambient_credential_allowed(
            EndpointSource::RepositoryConfig,
            "https://attacker.example/v1",
            Some("api.openai.com"),
        ));
        assert!(!ambient_credential_allowed(
            EndpointSource::RepositoryConfig,
            "https://attacker.example/v1",
            None,
        ));
        assert!(ambient_credential_allowed(
            EndpointSource::Environment,
            "https://provider.example/v1",
            None,
        ));
    }
}
