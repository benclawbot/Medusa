use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const MAX_REQUEST_BYTES: usize = 8 * 1024;

/// A short-lived, loopback-only server for generated static web artifacts.
///
/// This keeps browser verification self-contained for a generated `index.html` without asking
/// the user to install or configure a separate development server. The server is deliberately
/// scoped to the verified worktree and rejects traversal outside it.
pub(crate) struct StaticVerificationServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl StaticVerificationServer {
    pub(crate) fn start(root: &Path) -> Result<Option<Self>, String> {
        let root = fs::canonicalize(root)
            .map_err(|error| format!("cannot inspect browser verification root: {error}"))?;
        if !root.join("index.html").is_file() {
            return Ok(None);
        }

        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|error| {
            format!("cannot start automatic browser verification server: {error}")
        })?;
        listener.set_nonblocking(true).map_err(|error| {
            format!("cannot configure automatic browser verification server: {error}")
        })?;
        let address = listener.local_addr().map_err(|error| {
            format!("cannot read automatic browser verification address: {error}")
        })?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_root = root.clone();
        let thread = thread::Builder::new()
            .name("medusa-static-verifier".to_owned())
            .spawn(move || run_server(listener, thread_root, thread_stop))
            .map_err(|error| {
                format!("cannot launch automatic browser verification server: {error}")
            })?;

        Ok(Some(Self {
            address,
            stop,
            thread: Some(thread),
        }))
    }

    pub(crate) fn route(&self) -> String {
        format!("http://127.0.0.1:{}/", self.address.port())
    }

    /// Probe the generated artifact without requiring the optional Playwright
    /// sidecar. This is intentionally limited to the loopback server owned by
    /// this value and only establishes that the document is reachable and
    /// non-empty; full browser checks still take precedence when available.
    pub(crate) fn probe(&self) -> Result<(u16, String), String> {
        let mut stream = TcpStream::connect_timeout(&self.address, Duration::from_secs(2))
            .map_err(|error| format!("cannot connect to automatic verification server: {error}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|error| format!("cannot configure automatic verification probe: {error}"))?;
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .map_err(|error| format!("cannot request generated index.html: {error}"))?;
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .map_err(|error| format!("cannot read generated index.html response: {error}"))?;
        let response = String::from_utf8(response)
            .map_err(|error| format!("generated index.html response was not UTF-8: {error}"))?;
        let (header, body) = response.split_once("\r\n\r\n").ok_or_else(|| {
            "generated index.html response had no HTTP header boundary".to_owned()
        })?;
        let status = header
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| "generated index.html response had no HTTP status".to_owned())?
            .parse::<u16>()
            .map_err(|error| {
                format!("generated index.html response had invalid HTTP status: {error}")
            })?;
        Ok((status, body.to_owned()))
    }
}

impl Drop for StaticVerificationServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_server(listener: TcpListener, root: PathBuf, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                if !stop.load(Ordering::Acquire) {
                    serve(stream, &root);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
}

fn serve(mut stream: TcpStream, root: &Path) {
    let mut request = [0_u8; MAX_REQUEST_BYTES];
    let bytes = stream.read(&mut request).unwrap_or(0);
    let request = String::from_utf8_lossy(&request[..bytes]);
    let mut fields = request
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    let method = fields.next().unwrap_or_default();
    let target = fields.next().unwrap_or_default();
    let head = method == "HEAD";
    let file = (method == "GET" || head)
        .then(|| requested_file(root, target))
        .flatten();

    let (status, content_type, body) = match file {
        Some(path) => (
            "200 OK",
            content_type(&path),
            fs::read(path).unwrap_or_default(),
        ),
        None if method == "GET" || head => (
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"Not Found".to_vec(),
        ),
        None => (
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            b"Method Not Allowed".to_vec(),
        ),
    };

    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    if !head {
        let _ = stream.write_all(&body);
    }
}

fn requested_file(root: &Path, target: &str) -> Option<PathBuf> {
    let path = target.split('?').next()?.split('#').next()?;
    let decoded = percent_decode(path.as_bytes())?;
    let decoded = std::str::from_utf8(&decoded).ok()?;
    let relative = decoded.trim_start_matches('/');
    if relative.is_empty() {
        return Some(root.join("index.html"));
    }
    if relative.split('/').any(|component| {
        component.is_empty() || component == "." || component == ".." || component.contains('\\')
    }) {
        return None;
    }

    let candidate = root.join(relative);
    let canonical = fs::canonicalize(&candidate).ok()?;
    if !canonical.starts_with(root) || !canonical.is_file() {
        return None;
    }
    Some(canonical)
}

fn percent_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] != b'%' {
            output.push(input[index]);
            index += 1;
            continue;
        }
        let high = input.get(index + 1).copied().and_then(hex_value)?;
        let low = input.get(index + 2).copied().and_then(hex_value)?;
        output.push((high << 4) | low);
        index += 3;
    }
    Some(output)
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
pub(super) fn fetch_for_test(server: &StaticVerificationServer) -> (u16, String) {
    let mut stream = TcpStream::connect(server.address).expect("connect server");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .expect("request");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("response");
    let response = String::from_utf8(response).expect("utf8 response");
    let mut sections = response.split("\r\n\r\n");
    let header = sections.next().expect("headers");
    let body = sections.next().unwrap_or_default().to_owned();
    let status = header
        .split_whitespace()
        .nth(1)
        .expect("status")
        .parse()
        .expect("numeric status");
    (status, body)
}
