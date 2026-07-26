//! Shared public-host URL validation for the browser sidecar.

use medusa_browser_client::network_policy::resolve_public_target;

pub fn validate_public_url(url: &url::Url) -> Result<(), String> {
    if std::env::var_os("MEDUSA_BROWSER_ALLOW_LOOPBACK").is_some()
        && validate_loopback_url(url).is_ok()
    {
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

    use super::validate_public_url;

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
}
