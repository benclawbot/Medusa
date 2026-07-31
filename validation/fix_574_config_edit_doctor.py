from pathlib import Path


def replace_once(source: str, old: str, new: str, label: str) -> str:
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one target, found {count}")
    return source.replace(old, new, 1)


path = Path("crates/medusa-cli/src/config_command.rs")
source = path.read_text()

source = replace_once(
    source,
    '''    add_check(
        &mut checks,
        "profile_store",
        profile_store_writable(store.path()),
        if profile_store_writable(store.path()) {
            "configuration directory is writable"
        } else {
            "configuration directory is not writable or does not exist"
        },
        Some("fix the configuration-directory ownership or permissions"),
    );
''',
    '''    let profile_store_writable = profile_store_writable(store.path());
    add_check(
        &mut checks,
        "profile_store",
        profile_store_writable,
        if profile_store_writable {
            "configuration directory is writable"
        } else {
            "configuration directory is not writable or does not exist"
        },
        Some("fix the configuration-directory ownership or permissions"),
    );
''',
    "single writability probe",
)

for label, old, new in [
    (
        "schema none type",
        '''                "profile schema and typed invariants are valid",
                None,
''',
        '''                "profile schema and typed invariants are valid",
                None::<String>,
''',
    ),
    (
        "endpoint none type",
        '''            "selected route uses its provider default endpoint",
            None,
''',
        '''            "selected route uses its provider default endpoint",
            None::<String>,
''',
    ),
    (
        "credential none type",
        '''                _ => "credential mode is valid",
            },
            None,
''',
        '''                _ => "credential mode is valid",
            },
            None::<String>,
''',
    ),
]:
    source = replace_once(source, old, new, label)

source = replace_once(
    source,
    '''fn loopback_socket(base_url: &str) -> Option<SocketAddr> {
    let url = reqwest::Url::parse(base_url).ok()?;
    let host = url.host_str()?;
    let address = if host.eq_ignore_ascii_case("localhost") {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        host.parse::<IpAddr>().ok()?
    };
    address
        .is_loopback()
        .then(|| SocketAddr::new(address, url.port_or_known_default()?))
}
''',
    '''fn loopback_socket(base_url: &str) -> Option<SocketAddr> {
    let url = reqwest::Url::parse(base_url).ok()?;
    let host = url.host_str()?;
    let address = if host.eq_ignore_ascii_case("localhost") {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        host.parse::<IpAddr>().ok()?
    };
    if !address.is_loopback() {
        return None;
    }
    Some(SocketAddr::new(address, url.port_or_known_default()?))
}
''',
    "loopback socket resolution",
)

path.write_text(source)
