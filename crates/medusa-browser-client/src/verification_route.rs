//! Shared admission contract for the Medusa-owned browser verification route.

use std::{fmt, net::IpAddr};

use sha2::{Digest, Sha256};
use url::Url;

use crate::network_policy::resolve_public_target;

pub const VERIFY_URL_ENV: &str = "MEDUSA_BROWSER_VERIFY_URL";
pub const VERIFICATION_ORIGIN_ENV: &str = "MEDUSA_BROWSER_VERIFICATION_ORIGIN";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationRoute {
    url: Url,
}

impl VerificationRoute {
    pub fn parse(raw: &str) -> Result<Self, VerificationRouteError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(VerificationRouteError::Missing);
        }
        let url = Url::parse(raw)
            .map_err(|error| VerificationRouteError::Malformed(error.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(VerificationRouteError::UnsupportedScheme(
                url.scheme().to_owned(),
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(VerificationRouteError::Credentials);
        }
        if url.fragment().is_some() {
            return Err(VerificationRouteError::Fragment);
        }
        let host = url
            .host_str()
            .ok_or(VerificationRouteError::MissingHost)?;
        let loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        if !loopback {
            resolve_public_target(
                url.scheme(),
                url.username(),
                url.password().is_some(),
                url.port(),
                host,
                url.port_or_known_default().unwrap_or(443),
            )
            .map_err(VerificationRouteError::DisallowedOrigin)?;
        }
        Ok(Self { url })
    }

    pub fn from_env() -> Result<Self, VerificationRouteError> {
        let raw = std::env::var_os(VERIFY_URL_ENV).ok_or(VerificationRouteError::Missing)?;
        let raw = raw
            .to_str()
            .ok_or(VerificationRouteError::NonUnicode)?;
        Self::parse(raw)
    }

    #[must_use]
    pub fn normalized(&self) -> &str {
        self.url.as_str()
    }

    #[must_use]
    pub fn origin(&self) -> String {
        self.url.origin().ascii_serialization()
    }

    #[must_use]
    pub fn safe_fingerprint(&self) -> String {
        let digest = Sha256::digest(self.normalized().as_bytes());
        format!("sha256:{}", hex::encode(digest))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VerificationRouteError {
    Missing,
    NonUnicode,
    Malformed(String),
    UnsupportedScheme(String),
    Credentials,
    Fragment,
    MissingHost,
    DisallowedOrigin(String),
}

impl fmt::Display for VerificationRouteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(
                formatter,
                "{VERIFY_URL_ENV} must contain a Medusa-owned HTTP(S) verification route"
            ),
            Self::NonUnicode => write!(formatter, "{VERIFY_URL_ENV} must be valid UTF-8"),
            Self::Malformed(error) => write!(
                formatter,
                "{VERIFY_URL_ENV} is not a valid absolute URL: {error}"
            ),
            Self::UnsupportedScheme(scheme) => write!(
                formatter,
                "{VERIFY_URL_ENV} must use http or https, not {scheme}"
            ),
            Self::Credentials => write!(
                formatter,
                "{VERIFY_URL_ENV} must not include username or password credentials"
            ),
            Self::Fragment => write!(formatter, "{VERIFY_URL_ENV} must not include a fragment"),
            Self::MissingHost => write!(formatter, "{VERIFY_URL_ENV} must include a host"),
            Self::DisallowedOrigin(error) => write!(
                formatter,
                "{VERIFY_URL_ENV} targets a disallowed verification origin: {error}"
            ),
        }
    }
}

impl std::error::Error for VerificationRouteError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_malformed_unsupported_credentials_and_fragments() {
        assert_eq!(
            VerificationRoute::parse("   ").unwrap_err(),
            VerificationRouteError::Missing
        );
        assert!(matches!(
            VerificationRoute::parse("not a url"),
            Err(VerificationRouteError::Malformed(_))
        ));
        assert_eq!(
            VerificationRoute::parse("file:///tmp/index.html").unwrap_err(),
            VerificationRouteError::UnsupportedScheme("file".to_owned())
        );
        assert_eq!(
            VerificationRoute::parse("http://user:secret@localhost:4173/app").unwrap_err(),
            VerificationRouteError::Credentials
        );
        assert_eq!(
            VerificationRoute::parse("http://localhost:4173/app#secret").unwrap_err(),
            VerificationRouteError::Fragment
        );
    }

    #[test]
    fn admits_loopback_and_public_routes_but_rejects_private_networks() {
        for route in [
            "http://localhost:4173/app",
            "http://127.0.0.1:4173/app?mode=verify",
            "http://[::1]:4173/app",
            "https://8.8.8.8/verify",
        ] {
            assert!(VerificationRoute::parse(route).is_ok(), "{route}");
        }
        for route in [
            "http://10.0.0.1/verify",
            "http://169.254.1.1/verify",
            "http://[fc00::1]/verify",
            "http://service.localhost/verify",
        ] {
            assert!(VerificationRoute::parse(route).is_err(), "{route}");
        }
    }

    #[test]
    fn normalized_route_origin_and_fingerprint_are_stable() {
        let route = VerificationRoute::parse(" HTTP://LOCALHOST:4173/app?mode=verify ")
            .expect("verification route");
        assert_eq!(route.normalized(), "http://localhost:4173/app?mode=verify");
        assert_eq!(route.origin(), "http://localhost:4173");
        assert_eq!(route.safe_fingerprint(), route.safe_fingerprint());
        assert!(!route.safe_fingerprint().contains("mode=verify"));
    }

    #[test]
    fn filesystem_paths_are_not_accepted_as_verification_routes() {
        for path in ["C:\\work\\app\\index.html", "/tmp/app/index.html"] {
            assert!(VerificationRoute::parse(path).is_err(), "{path}");
        }
    }
}
