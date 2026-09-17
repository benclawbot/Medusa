//! Shared public-network boundary for outbound HTTP research.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTarget {
    scheme: String,
    host: String,
    port: u16,
    addresses: Vec<SocketAddr>,
}

impl ResolvedTarget {
    #[must_use]
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    #[must_use]
    pub fn addresses(&self) -> &[SocketAddr] {
        &self.addresses
    }
}

pub fn resolve_public_target(
    scheme: &str,
    username: &str,
    has_password: bool,
    explicit_port: Option<u16>,
    host: &str,
    port: u16,
) -> Result<ResolvedTarget, String> {
    if !matches!(scheme, "http" | "https") {
        return Err("web URLs must use http or https".into());
    }
    if !username.is_empty() || has_password {
        return Err("web URLs must not include credentials".into());
    }
    if explicit_port.is_some_and(|value| value != 80 && value != 443) {
        return Err("web URLs may only use ports 80 or 443".into());
    }
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return Err("web URL must resolve to a public host".into());
    }
    let addresses = if let Ok(address) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(normalize_ip(address), port)]
    } else {
        (host, port)
            .to_socket_addrs()
            .map_err(|error| format!("could not resolve web host {host}: {error}"))?
            .collect()
    };
    if addresses.is_empty() {
        return Err("web URL must resolve to at least one address".into());
    }
    let addresses = addresses
        .into_iter()
        .map(|address| SocketAddr::new(normalize_ip(address.ip()), address.port()))
        .collect::<Vec<_>>();
    if addresses.iter().any(|address| !is_public_ip(address.ip())) {
        return Err("web URL must resolve only to public IP addresses".into());
    }
    Ok(ResolvedTarget {
        scheme: scheme.into(),
        host: host.into(),
        port,
        addresses,
    })
}

fn normalize_ip(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(value) => value
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(value)),
        value => value,
    }
}

#[must_use]
pub fn is_public_ip(address: IpAddr) -> bool {
    match normalize_ip(address) {
        IpAddr::V4(value) => {
            let octets = value.octets();
            !value.is_private()
                && !value.is_loopback()
                && !value.is_link_local()
                && !value.is_broadcast()
                && !value.is_unspecified()
                && !value.is_multicast()
                && octets[0] != 0
                && octets[0] < 240
                && !(octets[0] == 100 && (64..=127).contains(&octets[1]))
                && !(octets[0] == 192 && octets[1] == 0)
                && !(octets[0] == 198 && matches!(octets[1], 18 | 19))
                && !(octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
                && !(octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
        }
        IpAddr::V6(value) => {
            !value.is_loopback()
                && !value.is_unspecified()
                && !value.is_multicast()
                && !value.is_unicast_link_local()
                && !value.is_unique_local()
                && value.segments()[0] & 0xffc0 != 0xfec0
                && !(value.segments()[0] == 0x2001 && value.segments()[1] == 0x0db8)
        }
    }
}
