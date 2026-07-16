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
