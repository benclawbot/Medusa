//! Dependency-neutral admission checks for browser verification routes.
//!
//! `medusa-browserd --check` remains authoritative for DNS resolution and
//! production network policy. These checks reject malformed or obviously unsafe
//! routes before capability discovery attempts that readiness probe.

use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use sha2::{Digest, Sha256};

pub const VERIFY_URL_ENV: &str = "MEDUSA_BROWSER_VERIFY_URL";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationRoute {
    normalized: String,
}

impl VerificationRoute {
    pub fn parse(raw: &str) -> Result<Self, VerificationRouteError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(VerificationRouteError::Missing);
        }
        if raw.contains('#') {
            return Err(VerificationRouteError::Fragment);
        }
        let (scheme, remainder) = raw
            .split_once("://")
            .ok_or(VerificationRouteError::Malformed)?;
        let scheme = scheme.to_ascii_lowercase();
        if !matches!(scheme.as_str(), "http" | "https") {
            return Err(VerificationRouteError::UnsupportedScheme(scheme));
        }
        let authority_end = remainder.find(['/', '?']).unwrap_or(remainder.len());
        let authority = &remainder[..authority_end];
        if authority.is_empty() {
            return Err(VerificationRouteError::MissingHost);
        }
        if authority.contains('@') {
            return Err(VerificationRouteError::Credentials);
        }
        let suffix = &remainder[authority_end..];
        let authority = parse_authority(authority)?;
        let loopback = authority.address.is_some_and(IpAddr::is_loopback)
            || authority.host.eq_ignore_ascii_case("localhost");
        if !loopback {
            if authority.host.ends_with(".localhost") {
                return Err(VerificationRouteError::DisallowedOrigin);
            }
            if let Some(address) = authority.address
                && !is_public_ip(address)
            {
                return Err(VerificationRouteError::DisallowedOrigin);
            }
            if authority.port.is_some_and(|port| port != 80 && port != 443) {
                return Err(VerificationRouteError::DisallowedPort);
            }
        }
        let port = match (scheme.as_str(), authority.port) {
            ("http", Some(80)) | ("https", Some(443)) => None,
            (_, port) => port,
        };
        let mut normalized = format!("{scheme}://{}", authority.normalized_host);
        if let Some(port) = port {
            normalized.push(':');
            normalized.push_str(&port.to_string());
        }
        if suffix.is_empty() {
            normalized.push('/');
        } else if suffix.starts_with('?') {
            normalized.push('/');
            normalized.push_str(suffix);
        } else {
            normalized.push_str(suffix);
        }
        Ok(Self { normalized })
    }

    #[must_use]
    pub fn normalized(&self) -> &str {
        &self.normalized
    }

    #[must_use]
    pub fn safe_fingerprint(&self) -> String {
        let digest = Sha256::digest(self.normalized.as_bytes());
        format!("sha256:{}", hex::encode(digest))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VerificationRouteError {
    Missing,
    Malformed,
    UnsupportedScheme(String),
    Credentials,
    Fragment,
    MissingHost,
    InvalidPort,
    DisallowedPort,
    DisallowedOrigin,
}

impl fmt::Display for VerificationRouteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(
                formatter,
                "{VERIFY_URL_ENV} must contain a Medusa-owned HTTP(S) verification route"
            ),
            Self::Malformed => write!(
                formatter,
                "{VERIFY_URL_ENV} must be a valid absolute HTTP(S) URL"
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
            Self::InvalidPort => write!(formatter, "{VERIFY_URL_ENV} contains an invalid port"),
            Self::DisallowedPort => write!(
                formatter,
                "{VERIFY_URL_ENV} public routes may only use ports 80 or 443"
            ),
            Self::DisallowedOrigin => write!(
                formatter,
                "{VERIFY_URL_ENV} targets a disallowed local or private origin"
            ),
        }
    }
}

impl std::error::Error for VerificationRouteError {}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedAuthority {
    host: String,
    normalized_host: String,
    address: Option<IpAddr>,
    port: Option<u16>,
}

fn parse_authority(authority: &str) -> Result<ParsedAuthority, VerificationRouteError> {
    if let Some(rest) = authority.strip_prefix('[') {
        let close = rest.find(']').ok_or(VerificationRouteError::Malformed)?;
        let host = &rest[..close];
        let address = host
            .parse::<IpAddr>()
            .map_err(|_| VerificationRouteError::Malformed)?;
        if !matches!(address, IpAddr::V6(_)) {
            return Err(VerificationRouteError::Malformed);
        }
        let port = parse_port_suffix(&rest[close + 1..])?;
        return Ok(ParsedAuthority {
            host: host.to_ascii_lowercase(),
            normalized_host: format!("[{address}]"),
            address: Some(address),
            port,
        });
    }
    if authority.contains(['[', ']']) || authority.matches(':').count() > 1 {
        return Err(VerificationRouteError::Malformed);
    }
    let (host, port) = if let Some((host, port)) = authority.rsplit_once(':') {
        if host.is_empty() || port.is_empty() {
            return Err(VerificationRouteError::InvalidPort);
        }
        let port = port
            .parse::<u16>()
            .map_err(|_| VerificationRouteError::InvalidPort)?;
        (host, Some(port))
    } else {
        (authority, None)
    };
    if host.is_empty() {
        return Err(VerificationRouteError::MissingHost);
    }
    let host = host.to_ascii_lowercase();
    Ok(ParsedAuthority {
        normalized_host: host.clone(),
        address: host.parse::<IpAddr>().ok(),
        host,
        port,
    })
}

fn parse_port_suffix(suffix: &str) -> Result<Option<u16>, VerificationRouteError> {
    if suffix.is_empty() {
        return Ok(None);
    }
    let port = suffix
        .strip_prefix(':')
        .ok_or(VerificationRouteError::Malformed)?;
    if port.is_empty() {
        return Err(VerificationRouteError::InvalidPort);
    }
    port.parse::<u16>()
        .map(Some)
        .map_err(|_| VerificationRouteError::InvalidPort)
}

fn normalize_ip(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(address)),
        address => address,
    }
}

fn is_public_ip(address: IpAddr) -> bool {
    match normalize_ip(address) {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    !address.is_private()
        && !address.is_loopback()
        && !address.is_link_local()
        && !address.is_broadcast()
        && !address.is_unspecified()
        && !address.is_multicast()
        && octets[0] != 0
        && octets[0] < 240
        && !(octets[0] == 100 && (64..=127).contains(&octets[1]))
        && !(octets[0] == 192 && octets[1] == 0)
        && !(octets[0] == 198 && matches!(octets[1], 18 | 19))
        && !(octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        && !(octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    !address.is_loopback()
        && !address.is_unspecified()
        && !address.is_multicast()
        && !address.is_unicast_link_local()
        && !address.is_unique_local()
        && address.segments()[0] & 0xffc0 != 0xfec0
        && !(address.segments()[0] == 0x2001 && address.segments()[1] == 0x0db8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_route_classes_before_sidecar_probe() {
        for route in [
            " ",
            "not a url",
            "file:///tmp/index.html",
            "http://user:secret@localhost:4173/app",
            "http://localhost:4173/app#fragment",
            "http://10.0.0.1/verify",
            "http://169.254.1.1/verify",
            "http://[fc00::1]/verify",
            "http://service.localhost/verify",
            "C:\\work\\app\\index.html",
            "/tmp/app/index.html",
        ] {
            assert!(VerificationRoute::parse(route).is_err(), "{route}");
        }
    }

    #[test]
    fn normalizes_valid_loopback_and_public_literal_routes() {
        let route = VerificationRoute::parse(" HTTP://LOCALHOST:4173/app?mode=verify ")
            .expect("loopback route");
        assert_eq!(route.normalized(), "http://localhost:4173/app?mode=verify");
        assert!(VerificationRoute::parse("https://8.8.8.8/verify").is_ok());
        assert!(VerificationRoute::parse("http://[::1]:4173/app").is_ok());
    }

    #[test]
    fn fingerprint_is_stable_and_does_not_expose_route_text() {
        let route =
            VerificationRoute::parse("http://localhost:4173/app?token=secret").expect("route");
        assert_eq!(route.safe_fingerprint(), route.safe_fingerprint());
        assert!(!route.safe_fingerprint().contains("secret"));
    }
}
