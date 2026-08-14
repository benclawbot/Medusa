//! Shared public-host URL validation for the browser sidecar.

use medusa_browser_client::network_policy::resolve_public_target;

pub(crate) const VERIFY_URL_ENV: &str = "MEDUSA_BROWSER_VERIFY_URL";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VerificationRoute {
    url: url::Url,
}

impl VerificationRoute {
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(format!(
                "{VERIFY_URL_ENV} must contain a Medusa-owned HTTP(S) verification route"
            ));
        }
        let url = url::Url::parse(raw)
            .map_err(|error| format!("{VERIFY_URL_ENV} is not a valid absolute URL: {error}"))?;
        if url.fragment().is_some() {
            return Err(format!("{VERIFY_URL_ENV} must not include a fragment"));
        }
        if validate_loopback_url(&url).is_err() {
            let host = url
                .host_str()
                .ok_or_else(|| format!("{VERIFY_URL_ENV} must include a host"))?;
            resolve_public_target(
                url.scheme(),
                url.username(),
                url.password().is_some(),
                url.port(),
                host,
                url.port_or_known_default().unwrap_or(443),
            )
            .map_err(|error| format!("{VERIFY_URL_ENV} targets a disallowed origin: {error}"))?;
        }
        Ok(Self { url })
    }

    pub(crate) fn from_env() -> Result<Self, String> {
        let raw = std::env::var_os(VERIFY_URL_ENV).ok_or_else(|| {
            format!("{VERIFY_URL_ENV} is required for browser readiness")
        })?;
        let raw = raw
            .to_str()
            .ok_or_else(|| format!("{VERIFY_URL_ENV} must be valid UTF-8"))?;
        Self::parse(raw)
    }

    #[must_use]
    pub(crate) fn normalized(&self) -> &str {
        self.url.as_str()
    }

    #[must_use]
    pub(crate) fn origin(&self) -> String {
        self.url.origin().ascii_serialization()
    }
}

pub fn validate_public_url(url: &url::Url) -> Result<(), String> {
    if configured_loopback_url(url)? {
        return Ok(());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "web URL must include a host".to_owned())?;
    resolve_public_target(
        url.scheme(),
        url.username(),
        url.password().is_some(),
        url.port(),
        host,
        url.port_or_known_default().unwrap_or(443),
    )
    .map(|_| ())
}

/// Returns true only when `url` is loopback and matches the exact origin of the
/// admitted Medusa-owned verification route. The verification route is the
/// single authority; a second origin environment variable cannot widen it.
pub(crate) fn configured_loopback_url(url: &url::Url) -> Result<bool, String> {
    if validate_loopback_url(url).is_err() {
        return Ok(false);
    }
    let allowed = VerificationRoute::from_env()?;
    if validate_loopback_url(&allowed.url).is_err() {
        return Ok(false);
    }
    if same_origin(url, &allowed.url) {
        Ok(true)
    } else {
        Err("local browser URL is outside the configured verification origin".to_owned())
    }
}

fn same_origin(left: &url::Url, right: &url::Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str().map(str::to_ascii_lowercase)
            == right.host_str().map(str::to_ascii_lowercase)
        && left.port_or_known_default() == right.port_or_known_default()
}

pub(crate) fn validate_loopback_url(url: &url::Url) -> Result<(), String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err("local browser URLs must use http or https".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("local browser URLs must not include credentials".to_owned());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "local browser URL must include a host".to_owned())?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    loopback
        .then_some(())
        .ok_or_else(|| "local browser URL must target loopback".to_owned())
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use medusa_browser_client::network_policy::is_public_ip;

    use super::{VerificationRoute, same_origin, validate_public_url};

    fn parse(input: &str) -> url::Url {
        url::Url::parse(input).expect("test URL")
    }

    #[test]
    fn verification_route_rejects_invalid_route_classes() {
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
    fn verification_route_normalizes_valid_loopback_and_public_urls() {
        let loopback = VerificationRoute::parse(" HTTP://LOCALHOST:4173/app?mode=verify ")
            .expect("loopback route");
        assert_eq!(loopback.normalized(), "http://localhost:4173/app?mode=verify");
        assert_eq!(loopback.origin(), "http://localhost:4173");
        assert!(VerificationRoute::parse("https://8.8.8.8/verify").is_ok());
        assert!(VerificationRoute::parse("http://[::1]:4173/app").is_ok());
    }

    #[test]
    fn public_literal_addresses_are_allowed() {
        assert!(validate_public_url(&parse("https://8.8.8.8/")).is_ok());
        assert!(validate_public_url(&parse("http://1.1.1.1:80/")).is_ok());
    }

    #[test]
    fn non_http_schemes_credentials_and_custom_ports_are_rejected() {
        assert_eq!(
            validate_public_url(&parse("ftp://8.8.8.8/")),
            Err("web URLs must use http or https".to_owned())
        );
        assert_eq!(
            validate_public_url(&parse("https://user:secret@8.8.8.8/")),
            Err("web URLs must not include credentials".to_owned())
        );
        assert_eq!(
            validate_public_url(&parse("https://8.8.8.8:8443/")),
            Err("web URLs may only use ports 80 or 443".to_owned())
        );
    }

    #[test]
    fn browser_uses_shared_mapped_address_policy() {
        for address in [
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.1.1",
            "::1",
            "fc00::1",
            "fe80::1",
        ] {
            assert!(
                !is_public_ip(address.parse::<IpAddr>().expect("address")),
                "{address}"
            );
            assert!(validate_public_url(&parse(&format!("http://[{address}]/"))).is_err());
        }
    }

    #[test]
    fn localhost_names_are_rejected_without_dns() {
        for url in ["http://localhost/", "https://service.localhost/"] {
            assert!(validate_public_url(&parse(url)).is_err());
        }
    }

    #[test]
    fn verification_origin_matching_is_scheme_host_and_port_exact() {
        let origin = parse("http://localhost:4173/app");
        assert!(same_origin(&origin, &parse("http://LOCALHOST:4173/other")));
        assert!(!same_origin(&origin, &parse("http://localhost:5173/")));
        assert!(!same_origin(&origin, &parse("https://localhost:4173/")));
        assert!(!same_origin(&origin, &parse("http://127.0.0.1:4173/")));
    }
}
