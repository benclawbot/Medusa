//! Public-host URL validation for the browser sidecar.
//!
//! Mirrors the rules from `crates/medusa-agent/src/tools/web.rs` so the
//! sidecar and the agent tool refuse the same private/loopback addresses.
//! The duplication is intentional per spec §Error handling: both files are
//! small and the rules may diverge over time.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

pub fn validate_public_url(url: &url::Url) -> Result<(), String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err("web URLs must use http or https".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("web URLs must not include credentials".to_owned());
    }
    if url.port().is_some_and(|port| port != 80 && port != 443) {
        return Err("web URLs may only use ports 80 or 443".to_owned());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "web URL must include a host".to_owned())?;
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return Err("web URL must resolve to a public host".to_owned());
    }
    if let Ok(address) = host.parse::<IpAddr>() {
        if !is_public_ip(address) {
            return Err("web URL must use a public IP address".to_owned());
        }
        return Ok(());
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("could not resolve web host {host}: {error}"))?
        .map(|address| address.ip())
        .collect::<Vec<_>>();
    if addresses.is_empty() || addresses.iter().any(|address| !is_public_ip(*address)) {
        return Err("web URL must resolve only to public IP addresses".to_owned());
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            !address.is_private()
                && !address.is_loopback()
                && !address.is_link_local()
                && !address.is_broadcast()
                && !address.is_unspecified()
                && !address.is_multicast()
                && address != Ipv4Addr::new(100, 64, 0, 0)
                && !(address.octets()[0] == 100 && (64..=127).contains(&address.octets()[1]))
                && !(address.octets()[0] == 192 && address.octets()[1] == 0)
                && !(address.octets()[0] == 198 && matches!(address.octets()[1], 18 | 19))
                && !(address.octets()[0] == 198
                    && address.octets()[1] == 51
                    && address.octets()[2] == 100)
                && !(address.octets()[0] == 203
                    && address.octets()[1] == 0
                    && address.octets()[2] == 113)
        }
        IpAddr::V6(address) => {
            !address.is_loopback()
                && !address.is_unspecified()
                && !address.is_multicast()
                && !address.is_unicast_link_local()
                && !address.is_unique_local()
                && address != Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::{is_public_ip, validate_public_url};

    fn parse(input: &str) -> url::Url {
        url::Url::parse(input).unwrap()
    }

    #[test]
    fn public_literal_addresses_are_allowed() {
        assert!(validate_public_url(&parse("https://8.8.8.8/")).is_ok());
        assert!(validate_public_url(&parse("http://1.1.1.1:80/")).is_ok());
        assert!(is_public_ip(IpAddr::V6(Ipv6Addr::new(
            0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111,
        ))));
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
    fn localhost_names_are_rejected_without_dns() {
        for url in ["http://localhost/", "https://service.localhost/"] {
            assert_eq!(
                validate_public_url(&parse(url)),
                Err("web URL must resolve to a public host".to_owned())
            );
        }
    }

    #[test]
    fn private_and_reserved_ipv4_ranges_are_rejected() {
        let addresses = [
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::BROADCAST,
            Ipv4Addr::new(10, 0, 0, 1),
            Ipv4Addr::new(169, 254, 1, 1),
            Ipv4Addr::new(224, 0, 0, 1),
            Ipv4Addr::new(100, 64, 0, 0),
            Ipv4Addr::new(100, 127, 255, 255),
            Ipv4Addr::new(192, 0, 2, 1),
            Ipv4Addr::new(198, 18, 0, 1),
            Ipv4Addr::new(198, 19, 0, 1),
            Ipv4Addr::new(198, 51, 100, 1),
            Ipv4Addr::new(203, 0, 113, 1),
        ];

        for address in addresses {
            assert!(!is_public_ip(IpAddr::V4(address)), "{address}");
            assert_eq!(
                validate_public_url(&parse(&format!("http://{address}/"))),
                Err("web URL must use a public IP address".to_owned())
            );
        }
    }

    #[test]
    fn private_and_reserved_ipv6_ranges_are_rejected() {
        let addresses = [
            Ipv6Addr::LOCALHOST,
            Ipv6Addr::UNSPECIFIED,
            Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1),
            Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1),
            Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 1),
            Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 0),
        ];

        for address in addresses {
            assert!(!is_public_ip(IpAddr::V6(address)), "{address}");
        }
    }

    #[test]
    fn unresolvable_host_returns_resolution_context() {
        let error = validate_public_url(&parse("https://does-not-exist.invalid/"))
            .expect_err("reserved invalid TLD should not resolve");
        assert!(error.contains("could not resolve web host"));
    }
}
