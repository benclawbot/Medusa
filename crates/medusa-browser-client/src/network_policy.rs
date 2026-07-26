//! Shared public-network boundary policy for Medusa HTTP and browser traffic.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTarget {
    scheme: String,
    host: String,
    port: u16,
    addresses: Vec<SocketAddr>,
}

impl ResolvedTarget {
    #[must_use]
    pub fn new_for_loopback(
        scheme: &str,
        host: &str,
        port: u16,
        addresses: Vec<std::net::SocketAddr>,
    ) -> Self {
        Self {
            scheme: scheme.to_owned(),
            host: host.to_owned(),
            port,
            addresses,
        }
    }

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
    resolve_public_target_with(
        scheme,
        username,
        has_password,
        explicit_port,
        host,
        port,
        |host, port| {
            (host, port)
                .to_socket_addrs()
                .map(|addresses| addresses.collect::<Vec<_>>())
                .map_err(|error| format!("could not resolve web host {host}: {error}"))
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub fn resolve_public_target_with<F>(
    scheme: &str,
    username: &str,
    has_password: bool,
    explicit_port: Option<u16>,
    host: &str,
    port: u16,
    resolver: F,
) -> Result<ResolvedTarget, String>
where
    F: FnOnce(&str, u16) -> Result<Vec<SocketAddr>, String>,
{
    if !matches!(scheme, "http" | "https") {
        return Err("web URLs must use http or https".to_owned());
    }
    if !username.is_empty() || has_password {
        return Err("web URLs must not include credentials".to_owned());
    }
    if explicit_port.is_some_and(|port| port != 80 && port != 443) {
        return Err("web URLs may only use ports 80 or 443".to_owned());
    }
    if host.is_empty() {
        return Err("web URL must include a host".to_owned());
    }
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return Err("web URL must resolve to a public host".to_owned());
    }
    let addresses = if let Ok(address) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(normalize_ip(address), port)]
    } else {
        resolver(host, port)?
    };
    if addresses.is_empty() {
        return Err("web URL must resolve to at least one address".to_owned());
    }
    let addresses = addresses
        .into_iter()
        .map(|address| SocketAddr::new(normalize_ip(address.ip()), address.port()))
        .collect::<Vec<_>>();
    if addresses.iter().any(|address| !is_public_ip(address.ip())) {
        return Err("web URL must resolve only to public IP addresses".to_owned());
    }
    Ok(ResolvedTarget {
        scheme: scheme.to_owned(),
        host: host.to_owned(),
        port,
        addresses,
    })
}

#[must_use]
pub fn normalize_ip(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(address)),
        address => address,
    }
}

#[must_use]
pub fn is_public_ip(address: IpAddr) -> bool {
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
    fn shared_address_corpus_rejects_private_and_mapped_ranges() {
        let rejected = [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.10.2",
            "100.64.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "240.0.0.1",
            "::1",
            "fe80::1",
            "fc00::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.1.1",
        ];
        for address in rejected {
            let parsed = address.parse::<IpAddr>().expect("test address");
            assert!(!is_public_ip(parsed), "{address}");
        }
        for address in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(
                is_public_ip(address.parse().expect("public address")),
                "{address}"
            );
        }
    }

    #[test]
    fn resolution_is_captured_once_and_normalized() {
        let target = resolve_public_target_with(
            "https",
            "",
            false,
            None,
            "example.test",
            443,
            |host, port| {
                assert_eq!(host, "example.test");
                assert_eq!(port, 443);
                Ok(vec![SocketAddr::new(
                    "::ffff:8.8.8.8".parse().expect("mapped address"),
                    port,
                )])
            },
        )
        .expect("public target");
        assert_eq!(target.scheme(), "https");
        assert_eq!(
            target.addresses(),
            &["8.8.8.8:443".parse().expect("socket")]
        );
    }

    #[test]
    fn mixed_public_and_private_resolution_fails_closed() {
        let result = resolve_public_target_with(
            "https",
            "",
            false,
            None,
            "example.test",
            443,
            |_host, port| {
                Ok(vec![
                    SocketAddr::new("8.8.8.8".parse().expect("public"), port),
                    SocketAddr::new("127.0.0.1".parse().expect("loopback"), port),
                ])
            },
        );
        assert_eq!(
            result,
            Err("web URL must resolve only to public IP addresses".to_owned())
        );
    }
}
