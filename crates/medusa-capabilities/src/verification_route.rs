//! Dependency-neutral admission checks for browser verification routes.
//!
//! `medusa-browserd --check` remains authoritative for DNS resolution and
//! production network policy. These checks reject malformed or obviously unsafe
//! routes before capability discovery attempts that readiness probe.
//!
//! DNS re-validation at probe time is defense in depth: an admitted hostname
//! may rebind to private space between admission and use. Call
//! [`VerificationRoute::resolve_probe_targets`] after [`VerificationRoute::parse`]
//! and treat failures as unavailable.
//!
//! Residual TOCTOU: DNS can change again between this re-validation and the
//! actual connection. The browserd proxy narrows the window by re-resolving
//! per connection (`resolve_public_target`), but a hostile DNS operator can
//! still race the check. The authoritative mitigation is the browserd
//! sidecar's per-connection resolution plus its `--check` readiness gate, not
//! this admission-time snapshot.

use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
};

use sha2::{Digest, Sha256};

pub const VERIFY_URL_ENV: &str = "MEDUSA_BROWSER_VERIFY_URL";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationRoute {
    normalized: String,
    host: String,
    port: u16,
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
        let loopback = authority
            .address
            .is_some_and(|address| address.is_loopback())
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
        let default_port = if scheme == "http" { 80 } else { 443 };
        Ok(Self {
            normalized,
            host: authority.host,
            port: port.unwrap_or(default_port),
        })
    }

    #[must_use]
    pub fn normalized(&self) -> &str {
        &self.normalized
    }

    #[must_use]
    pub fn safe_fingerprint(&self) -> String {
        let digest = Sha256::digest(self.normalized().as_bytes());
        format!("sha256:{}", hex::encode(digest))
    }

    /// Admitted host plus the port a probe-time connection would use.
    #[must_use]
    pub fn host_and_port(&self) -> (&str, u16) {
        (&self.host, self.port)
    }

    /// Resolves the admitted route at probe time and re-validates the result.
    ///
    /// Hostnames admitted by [`VerificationRoute::parse`] are not resolved at
    /// admission, so a name that was public then may rebind to private space
    /// by probe time. Loopback admissions and IP literals carry no DNS risk
    /// and resolve trivially. `medusa-browserd --check` remains the
    /// authoritative readiness gate; see the module docs for the residual
    /// TOCTOU.
    pub fn resolve_probe_targets(&self) -> Result<Vec<SocketAddr>, VerificationRouteError> {
        self.resolve_probe_targets_with(|host, port| {
            (host, port)
                .to_socket_addrs()
                .map(|addresses| addresses.collect::<Vec<_>>())
                .map_err(|error| {
                    format!("could not resolve verification route host {host}: {error}")
                })
        })
    }

    pub(crate) fn resolve_probe_targets_with(
        &self,
        resolver: impl FnOnce(&str, u16) -> Result<Vec<SocketAddr>, String>,
    ) -> Result<Vec<SocketAddr>, VerificationRouteError> {
        if self.host.eq_ignore_ascii_case("localhost") {
            return Ok(Vec::new());
        }
        let addresses = if let Ok(address) = self.host.parse::<IpAddr>() {
            let normalized = normalize_ip(address);
            if normalized.is_loopback() {
                // Loopback literals are pinned to loopback per connection by
                // the browserd proxy; there is no DNS to rebind.
                return Ok(vec![SocketAddr::new(normalized, self.port)]);
            }
            vec![SocketAddr::new(normalized, self.port)]
        } else {
            let (host, port) = self.host_and_port();
            resolver(host, port).map_err(VerificationRouteError::Resolution)?
        };
        validate_probe_time_addresses(&addresses)?;
        Ok(addresses)
    }
}

/// Rejects probe-time resolutions that no longer land in public space.
///
/// Every resolved address must be public; a single private/loopback/link-local
/// address fails the whole route closed because DNS rebinding typically poisons
/// only a subset of answers.
pub fn validate_probe_time_addresses(
    addresses: &[SocketAddr],
) -> Result<(), VerificationRouteError> {
    if addresses.is_empty() {
        return Err(VerificationRouteError::Resolution(
            "verification route resolved to no addresses".to_owned(),
        ));
    }
    if addresses
        .iter()
        .any(|address| !is_public_ip(normalize_ip(address.ip())))
    {
        return Err(VerificationRouteError::Resolution(
            "verification route DNS currently resolves to non-public space; refusing possible rebinding"
                .to_owned(),
        ));
    }
    Ok(())
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
    /// Probe-time DNS resolution failed or no longer lands in public space.
    Resolution(String),
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
            Self::Resolution(reason) => write!(
                formatter,
                "{VERIFY_URL_ENV} failed probe-time DNS re-validation: {reason}"
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

    #[test]
    fn probe_time_revalidation_rejects_private_rebinding() {
        use std::net::SocketAddr;

        let public: SocketAddr = "8.8.8.8:443".parse().expect("public");
        assert!(validate_probe_time_addresses(&[public]).is_ok());
        for bad in [
            "127.0.0.1:443",
            "10.0.0.1:443",
            "169.254.1.1:443",
            "[::1]:443",
            "[fc00::1]:443",
        ] {
            let address: SocketAddr = bad.parse().expect("test address");
            assert!(
                validate_probe_time_addresses(&[public, address]).is_err(),
                "{bad} must fail a mixed resolution closed"
            );
        }
        assert!(validate_probe_time_addresses(&[]).is_err());
    }

    #[test]
    fn admitted_hostnames_are_resolved_and_revalidated_at_probe_time() {
        let route = VerificationRoute::parse("https://example.com/verify").expect("route");
        assert_eq!(route.host_and_port(), ("example.com", 443));
        // Public resolution stays admitted.
        let admitted = route
            .resolve_probe_targets_with(|host, port| {
                assert_eq!(host, "example.com");
                assert_eq!(port, 443);
                Ok(vec!["8.8.8.8:443".parse().expect("public")])
            })
            .expect("public resolution");
        assert_eq!(admitted.len(), 1);
        // Rebinding to private space is refused.
        let error = route
            .resolve_probe_targets_with(|_, port| {
                Ok(vec![
                    "8.8.8.8:443".parse().expect("public"),
                    SocketAddr::new("10.0.0.1".parse().expect("private"), port),
                ])
            })
            .expect_err("private rebinding must be refused");
        assert!(matches!(error, VerificationRouteError::Resolution(_)));
        // Resolution failure fails closed.
        assert!(matches!(
            route.resolve_probe_targets_with(|_, _| Err("dns down".to_owned())),
            Err(VerificationRouteError::Resolution(_))
        ));
        // Loopback admissions carry no DNS risk.
        let loopback = VerificationRoute::parse("http://localhost:4173/app").expect("loopback");
        assert!(
            loopback
                .resolve_probe_targets_with(|_, _| Err("must not resolve".to_owned()))
                .expect("loopback")
                .is_empty()
        );
        let literal = VerificationRoute::parse("http://127.0.0.1:4173/app").expect("literal");
        assert_eq!(literal.resolve_probe_targets().expect("literal").len(), 1);
    }
}
