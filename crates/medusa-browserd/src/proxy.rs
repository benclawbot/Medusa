use std::{
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    thread,
    time::Duration,
};

use medusa_browser_client::network_policy::{ResolvedTarget, resolve_public_target};

use crate::validation::configured_loopback_url;

const MAX_HEADER_BYTES: usize = 32 * 1024;

pub struct Proxy {
    address: SocketAddr,
}

impl Proxy {
    #[must_use]
    pub fn server(&self) -> String {
        format!("http://{}", self.address)
    }
}

pub fn spawn() -> io::Result<Proxy> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let address = listener.local_addr()?;
    thread::Builder::new()
        .name("medusa-browser-proxy".to_owned())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let _ = thread::Builder::new()
                            .name("medusa-browser-proxy-connection".to_owned())
                            .spawn(move || {
                                let _ = handle_connection(stream);
                            });
                    }
                    Err(_) => break,
                }
            }
        })?;
    Ok(Proxy { address })
}

fn handle_connection(mut client: TcpStream) -> io::Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(15)))?;
    client.set_write_timeout(Some(Duration::from_secs(15)))?;
    let header = read_header(&mut client)?;
    let header_text = std::str::from_utf8(&header)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "proxy request is not UTF-8"))?;
    let first_line = header_text
        .lines()
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "proxy request is empty"))?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    if method.eq_ignore_ascii_case("CONNECT") {
        return handle_connect(client, target);
    }
    handle_http(client, method, target, header_text)
}

fn handle_connect(mut client: TcpStream, authority: &str) -> io::Result<()> {
    let url = url::Url::parse(&format!("https://{authority}/"))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let target = resolve_url(&url).map_err(policy_error)?;
    let upstream = connect_pinned(&target)?;
    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    tunnel(client, upstream)
}

fn handle_http(
    client: TcpStream,
    method: &str,
    absolute_target: &str,
    header_text: &str,
) -> io::Result<()> {
    let url = url::Url::parse(absolute_target)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let target = resolve_url(&url).map_err(policy_error)?;
    let mut upstream = connect_pinned(&target)?;
    let mut path = url.path().to_owned();
    if path.is_empty() {
        path.push('/');
    }
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    let mut lines = header_text.lines();
    let _ = lines.next();
    write!(upstream, "{method} {path} HTTP/1.1\r\n")?;
    for line in lines {
        if line.is_empty() {
            break;
        }
        if line.to_ascii_lowercase().starts_with("proxy-connection:") {
            continue;
        }
        write!(upstream, "{line}\r\n")?;
    }
    upstream.write_all(b"\r\n")?;
    upstream.flush()?;
    tunnel(client, upstream)
}

fn resolve_url(url: &url::Url) -> Result<ResolvedTarget, String> {
    if configured_loopback_url(url)? {
        let host = url
            .host_str()
            .ok_or_else(|| "local browser URL must include a host".to_owned())?;
        let port = url.port_or_known_default().unwrap_or(80);
        let addresses = (host, port)
            .to_socket_addrs()
            .map_err(|error| error.to_string())?
            .filter(|address| address.ip().is_loopback())
            .collect::<Vec<_>>();
        if addresses.is_empty() {
            return Err("local browser URL did not resolve to loopback".to_owned());
        }
        return Ok(ResolvedTarget::new_for_loopback(
            url.scheme(),
            host,
            port,
            addresses,
        ));
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
}

fn read_header(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut header = Vec::new();
    let mut byte = [0_u8; 1];
    while header.len() < MAX_HEADER_BYTES {
        let count = stream.read(&mut byte)?;
        if count == 0 {
            break;
        }
        header.push(byte[0]);
        if header.ends_with(b"\r\n\r\n") {
            return Ok(header);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "proxy request headers are missing or too large",
    ))
}

fn connect_pinned(target: &ResolvedTarget) -> io::Result<TcpStream> {
    let mut last_error = None;
    for address in target.addresses() {
        match TcpStream::connect_timeout(address, Duration::from_secs(10)) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::AddrNotAvailable, "no validated destination")
    }))
}

fn tunnel(client: TcpStream, upstream: TcpStream) -> io::Result<()> {
    let mut client_read = client.try_clone()?;
    let mut upstream_write = upstream.try_clone()?;
    let client_to_upstream = thread::spawn(move || {
        let result = io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(Shutdown::Write);
        result
    });

    let mut upstream_read = upstream;
    let mut client_write = client;
    let upstream_to_client = io::copy(&mut upstream_read, &mut client_write);
    let _ = client_write.shutdown(Shutdown::Write);
    let client_to_upstream = client_to_upstream
        .join()
        .map_err(|_| io::Error::other("proxy tunnel thread panicked"))?;
    client_to_upstream?;
    upstream_to_client?;
    Ok(())
}

fn policy_error(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_starts_on_loopback() {
        let proxy = spawn().expect("proxy");
        assert!(proxy.server().starts_with("http://127.0.0.1:"));
    }

    #[test]
    fn private_connect_targets_are_rejected() {
        let url = url::Url::parse("https://[::ffff:127.0.0.1]/").expect("URL");
        assert!(resolve_url(&url).is_err());
    }
}
