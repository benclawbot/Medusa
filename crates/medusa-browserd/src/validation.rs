//! Shared public-host URL validation for the browser sidecar.

use medusa_browser_client::network_policy::resolve_public_target;

const VERIFICATION_ORIGIN_ENV: &str = "MEDUSA_BROWSER_VERIFICATION_ORIGIN";

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
/// Medusa-owned verification route. A configured origin never grants access to
/// a different localhost host/port/scheme.
pub(crate) fn configured_loopback_url(url: &url::Url) -> Result<bool, String> {
    if validate_loopback_url(url).is_err() {
        return Ok(false);
    }
    let Some(allowed) = configured_loopback_origin()? else {
        return Ok(false);
    };
    if same_origin(url, &allowed) {
        Ok(true)
    } else {
        Err("local browser URL is outside the configured verification origin".to_owned())
    }
}

fn configured_loopback_origin() -> Result<Option<url::Url>, String> {
    let Some(raw) = std::env::var_os(VERIFICATION_ORIGIN_ENV) else {
        return Ok(None);
    };
    let raw = raw.to_string_lossy();
    let origin = url::Url::parse(raw.trim())
        .map_err(|error| format!("invalid browser verification origin: {error}"))?;
    if validate_loopback_url(&origin).is_err() {
        return Ok(None);
    }
    Ok(Some(origin))
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

    use super::{same_origin, validate_public_url};

    fn parse(input: &str) -> url::Url {
        url::Url::parse(input).expect("test URL")
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
            assert_eq!(
                validate_public_url(&parse(url)),
                Err("web URL must resolve to a public host".to_owned())
            );
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
