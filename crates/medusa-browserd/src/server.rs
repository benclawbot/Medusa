use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use medusa_browser_client::{
    protocol::{
        BrowserRequest, BrowserResponse, BrowserRpcRequest, BrowserRpcResponse,
        MAX_BROWSER_REQUEST_FRAME_BYTES, MAX_BROWSER_RESPONSE_FRAME_BYTES,
    },
    transport::{Transport, read_bounded_frame, send_and_receive},
};

use crate::{
    proxy,
    validation::{VERIFY_URL_ENV, VerificationRoute, validate_public_url},
};

const BROWSER_BRIDGE_PATH_ENV: &str = "MEDUSA_BROWSER_BRIDGE_PATH";
const BROWSER_BRIDGE_RELATIVE_PATH: &str = "browser/playwright_bridge.mjs";
/// How long `run` waits for the loopback proxy to accept connections before serving.
const PROXY_READINESS_TIMEOUT: Duration = Duration::from_secs(5);
/// How long `spawn_bridge` waits for the node bridge to answer Ping before serving.
const BRIDGE_READINESS_TIMEOUT: Duration = Duration::from_secs(10);
/// Request id reserved for the internal bridge readiness Ping.
const BRIDGE_PROBE_REQUEST_ID: u64 = u64::MAX;

pub fn run() -> io::Result<()> {
    let verification_route = configured_verification_route()?;
    let proxy = proxy::spawn()?;
    wait_for_proxy_ready(&proxy, PROXY_READINESS_TIMEOUT)?;
    let mut bridge = spawn_bridge(&proxy).map_err(io::Error::other)?;
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut frame = Vec::with_capacity(4096);
    let mut consecutive_bridge_restarts: u32 = 0;
    loop {
        let count = read_bounded_frame(&mut stdin, &mut frame, MAX_BROWSER_REQUEST_FRAME_BYTES)?;
        if count == 0 {
            break;
        }
        let wire: BrowserRpcRequest = match serde_json::from_slice(&frame) {
            Ok(request) => request,
            Err(error) => {
                write_response(
                    &mut stdout,
                    0,
                    &BrowserResponse::Error {
                        code: "invalid_request".into(),
                        message: error.to_string(),
                    },
                )?;
                continue;
            }
        };
        let request_id = wire.request_id;
        let request = wire.request;
        if request_id == 0 {
            write_response(
                &mut stdout,
                0,
                &BrowserResponse::Error {
                    code: "invalid_request_id".into(),
                    message: "browser request_id must be non-zero".into(),
                },
            )?;
            continue;
        }

        if matches!(request, BrowserRequest::Ping) {
            write_response(&mut stdout, request_id, &BrowserResponse::Ok)?;
            continue;
        }
        // Supervise the bridge: a child that died while idle is restarted
        // with backoff before the next request is forwarded to it.
        if bridge_exited(&mut bridge)? {
            let delay = bridge_restart_delay(consecutive_bridge_restarts);
            std::thread::sleep(delay);
            bridge = spawn_bridge(&proxy).map_err(io::Error::other)?;
            consecutive_bridge_restarts = consecutive_bridge_restarts.saturating_add(1);
        }
        if matches!(request, BrowserRequest::Close) {
            let response =
                forward_to_bridge(&mut bridge.stdin, &mut bridge.stdout, request_id, &request);
            write_response(&mut stdout, request_id, &response)?;
            break;
        }
        let request = match normalize_navigation_request(request, &verification_route) {
            Ok(request) => request,
            Err(response) => {
                write_response(&mut stdout, request_id, &response)?;
                continue;
            }
        };

        let mut response =
            forward_to_bridge(&mut bridge.stdin, &mut bridge.stdout, request_id, &request);
        // A transport failure against a dead child deserves one retry on a
        // freshly restarted bridge instead of surfacing a stale pipe error.
        if is_bridge_transport_failure(&response) && bridge_exited(&mut bridge).unwrap_or(false) {
            let delay = bridge_restart_delay(consecutive_bridge_restarts);
            std::thread::sleep(delay);
            match spawn_bridge(&proxy) {
                Ok(restarted) => {
                    bridge = restarted;
                    consecutive_bridge_restarts = consecutive_bridge_restarts.saturating_add(1);
                    response = forward_to_bridge(
                        &mut bridge.stdin,
                        &mut bridge.stdout,
                        request_id,
                        &request,
                    );
                }
                Err(error) => {
                    response = BrowserResponse::Error {
                        code: "bridge_restart_failed".into(),
                        message: error.to_string(),
                    };
                }
            }
        }
        if !is_bridge_transport_failure(&response) {
            consecutive_bridge_restarts = 0;
        }
        write_response(&mut stdout, request_id, &response)?;
    }
    let _ = bridge.child.kill();
    let _ = bridge.child.wait();
    Ok(())
}

pub(crate) fn check_readiness() -> io::Result<()> {
    resolve_bridge_path()?;
    if std::env::var_os(VERIFY_URL_ENV).is_some() {
        configured_verification_route()?;
    }
    Ok(())
}

fn configured_verification_route() -> io::Result<VerificationRoute> {
    VerificationRoute::from_env()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

#[cfg(test)]
fn admit_verification_route(raw: &str) -> io::Result<VerificationRoute> {
    VerificationRoute::parse(raw)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn normalize_navigation_request(
    request: BrowserRequest,
    verification_route: &VerificationRoute,
) -> Result<BrowserRequest, BrowserResponse> {
    let BrowserRequest::Navigate { url } = request else {
        return Ok(request);
    };
    let parsed = url::Url::parse(&url).map_err(|error| BrowserResponse::Error {
        code: "invalid_url".into(),
        message: error.to_string(),
    })?;
    if parsed.as_str() == verification_route.normalized() {
        return Ok(BrowserRequest::Navigate {
            url: verification_route.normalized().to_owned(),
        });
    }
    validate_public_url(&parsed).map_err(|message| BrowserResponse::Error {
        code: "invalid_url".into(),
        message,
    })?;
    Ok(BrowserRequest::Navigate {
        url: parsed.to_string(),
    })
}

struct Bridge {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

fn spawn_bridge(proxy: &proxy::Proxy) -> io::Result<Bridge> {
    let bridge_path = resolve_bridge_path()?;
    let mut command = Command::new("node");
    command
        .arg(bridge_path)
        .env("MEDUSA_BROWSER_PROXY", proxy.server())
        .env("MEDUSA_BROWSER_PARENT_PID", std::process::id().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    command.creation_flags(0x0800_0000);
    let mut child = command.spawn()?;
    let (stdin, stdout) = take_bridge_stdio(&mut child)?;
    let (pipes, readiness) = probe_bridge_stdio(stdin, stdout, BRIDGE_READINESS_TIMEOUT);
    match (pipes, readiness) {
        (Some((stdin, stdout)), Ok(())) => Ok(Bridge {
            child,
            stdin,
            stdout,
        }),
        (pipes, readiness) => {
            drop(pipes);
            let _ = child.kill();
            let _ = child.wait();
            Err(readiness.err().unwrap_or_else(|| {
                io::Error::other("Playwright bridge readiness probe failed without a cause")
            }))
        }
    }
}

/// Confirms the loopback proxy accepts connections before `run` serves traffic.
fn wait_for_proxy_ready(proxy: &proxy::Proxy, timeout: Duration) -> io::Result<()> {
    let started = Instant::now();
    loop {
        match std::net::TcpStream::connect(proxy.local_addr()) {
            Ok(_) => return Ok(()),
            Err(error) => {
                if started.elapsed() >= timeout {
                    return Err(io::Error::new(
                        error.kind(),
                        format!(
                            "browser proxy did not become ready within {} ms: {error}",
                            timeout.as_millis()
                        ),
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Non-blocking liveness check for the supervised bridge child.
fn bridge_exited(bridge: &mut Bridge) -> io::Result<bool> {
    Ok(bridge.child.try_wait()?.is_some())
}

/// Capped exponential backoff between bridge restarts: 100ms doubling to 5s.
fn bridge_restart_delay(consecutive_restarts: u32) -> Duration {
    Duration::from_millis(100_u64.saturating_mul(1_u64 << consecutive_restarts.min(6)))
        .min(Duration::from_secs(5))
}

/// True when a forward failed because the bridge pipe itself is broken, as
/// opposed to the bridge answering with an application-level error.
fn is_bridge_transport_failure(response: &BrowserResponse) -> bool {
    let BrowserResponse::Error { code, .. } = response else {
        return false;
    };
    matches!(
        code.as_str(),
        "request_write"
            | "request_flush"
            | "response_frame"
            | "response_parse"
            | "sidecar_closed"
            | "sidecar_transport_failed"
            | "request_id_mismatch"
    )
}

/// Ping round-trip proving the bridge child is alive before `spawn_bridge`
/// reports success. The read blocks, so the probe runs on a worker thread
/// bounded by `timeout`. Returns the pipes on success; on timeout the pipes
/// stay with the detached worker, which exits once the caller kills the child.
fn probe_bridge_stdio<W: Write + Send + 'static, R: BufRead + Send + 'static>(
    mut writer: W,
    mut reader: R,
    timeout: Duration,
) -> (Option<(W, R)>, io::Result<()>) {
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker = std::thread::Builder::new()
        .name("medusa-browserd-bridge-probe".to_owned())
        .spawn(move || {
            let response = forward_to_bridge(
                &mut writer,
                &mut reader,
                BRIDGE_PROBE_REQUEST_ID,
                &BrowserRequest::Ping,
            );
            let ready = matches!(response, BrowserResponse::Ok);
            let _ = sender.send((writer, reader, ready, response));
        });
    if worker.is_err() {
        return (
            None,
            Err(io::Error::other(
                "could not start bridge readiness probe worker",
            )),
        );
    }
    match receiver.recv_timeout(timeout.max(Duration::from_millis(1))) {
        Ok((writer, reader, true, _)) => (Some((writer, reader)), Ok(())),
        Ok((writer, reader, false, response)) => (
            Some((writer, reader)),
            Err(io::Error::other(format!(
                "Playwright bridge readiness probe failed: {response:?}"
            ))),
        ),
        Err(mpsc::RecvTimeoutError::Timeout) => (
            None,
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "Playwright bridge did not answer the readiness probe within {} ms",
                    timeout.as_millis()
                ),
            )),
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => (
            None,
            Err(io::Error::other(
                "Playwright bridge readiness probe worker exited",
            )),
        ),
    }
}

fn resolve_bridge_path() -> io::Result<PathBuf> {
    if let Some(configured) = std::env::var_os(BROWSER_BRIDGE_PATH_ENV) {
        let configured = PathBuf::from(configured);
        if configured.is_file() {
            return Ok(configured);
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "configured Playwright bridge does not exist: {}",
                configured.display()
            ),
        ));
    }

    let executable = std::env::current_exe()?;
    for candidate in bridge_path_candidates(&executable) {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "Playwright bridge was not found; set {BROWSER_BRIDGE_PATH_ENV} to the installed {BROWSER_BRIDGE_RELATIVE_PATH} asset"
        ),
    ))
}

fn bridge_path_candidates(executable: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(parent) = executable.parent() {
        for ancestor in parent
            .ancestors()
            .filter(|ancestor| {
                !ancestor
                    .components()
                    .any(|component| component.as_os_str() == "target")
            })
            .take(4)
        {
            let candidate = ancestor.join(BROWSER_BRIDGE_RELATIVE_PATH);
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    candidates
}

fn take_bridge_stdio(child: &mut Child) -> io::Result<(ChildStdin, BufReader<ChildStdout>)> {
    match (child.stdin.take(), child.stdout.take()) {
        (Some(stdin), Some(stdout)) => Ok((stdin, BufReader::new(stdout))),
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Playwright bridge launched without the required stdin/stdout pipes",
            ))
        }
    }
}

struct SplitTransport<'a, W, R> {
    writer: &'a mut W,
    reader: &'a mut R,
}

impl<W: Write, R> Write for SplitTransport<'_, W, R> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

impl<W: Write + Send, R: BufRead + Send> Transport for SplitTransport<'_, W, R> {
    fn read_frame(&mut self, buf: &mut Vec<u8>, max_bytes: usize) -> io::Result<usize> {
        read_bounded_frame(self.reader, buf, max_bytes)
    }
}

fn forward_to_bridge<W: Write + Send, R: BufRead + Send>(
    writer: &mut W,
    reader: &mut R,
    request_id: u64,
    request: &BrowserRequest,
) -> BrowserResponse {
    let mut transport = SplitTransport { writer, reader };
    match send_and_receive(&mut transport, request_id, request) {
        Ok(response) => response,
        Err(error) => BrowserResponse::Error {
            code: error
                .context
                .get("browser_error_kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("sidecar_transport_failed")
                .to_owned(),
            message: error.to_string(),
        },
    }
}

fn write_response<W: Write>(
    out: &mut W,
    request_id: u64,
    response: &BrowserResponse,
) -> io::Result<()> {
    let wire = BrowserRpcResponse {
        request_id,
        response: response.clone(),
    };
    let mut line = serde_json::to_vec(&wire).map_err(io::Error::other)?;
    if line.len().saturating_add(1) > MAX_BROWSER_RESPONSE_FRAME_BYTES {
        line = serde_json::to_vec(&BrowserRpcResponse {
            request_id,
            response: BrowserResponse::Error {
                code: "response_too_large".into(),
                message: format!(
                    "browser response exceeds {MAX_BROWSER_RESPONSE_FRAME_BYTES} bytes"
                ),
            },
        })
        .map_err(io::Error::other)?;
    }
    line.push(b'\n');
    out.write_all(&line)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use std::io::{self, BufRead, Cursor, Read, Write};
    use std::path::Path;
    use std::process::{Command, Stdio};

    use medusa_browser_client::protocol::{BrowserRequest, BrowserResponse};

    use super::{
        admit_verification_route, bridge_path_candidates, bridge_restart_delay, forward_to_bridge,
        is_bridge_transport_failure, normalize_navigation_request, probe_bridge_stdio,
        take_bridge_stdio, wait_for_proxy_ready, write_response,
    };

    #[derive(Default)]
    struct FailingWriter {
        fail_write: bool,
        fail_flush: bool,
        bytes: Vec<u8>,
    }

    impl Write for FailingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.fail_write {
                return Err(io::Error::other("write failed"));
            }
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush {
                return Err(io::Error::other("flush failed"));
            }
            Ok(())
        }
    }

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("read failed"))
        }
    }

    impl BufRead for FailingReader {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            Err(io::Error::other("read failed"))
        }

        fn consume(&mut self, _amount: usize) {}
    }

    fn error_code(response: BrowserResponse) -> String {
        match response {
            BrowserResponse::Error { code, .. } => code,
            other => panic!("expected error response, got {other:?}"),
        }
    }

    #[test]
    fn production_entrypoint_admits_the_same_normalized_route_contract() {
        let route = admit_verification_route(" HTTP://LOCALHOST:4173/app?mode=verify ")
            .expect("verification route");
        assert_eq!(route.normalized(), "http://localhost:4173/app?mode=verify");
        assert_eq!(route.origin(), "http://localhost:4173");
        assert!(admit_verification_route("file:///tmp/index.html").is_err());
        assert!(admit_verification_route("http://user:secret@localhost:4173/").is_err());
        assert!(admit_verification_route("http://10.0.0.1/").is_err());
    }

    #[test]
    fn verification_navigation_is_normalized_to_the_admitted_route() {
        let route = admit_verification_route("HTTP://LOCALHOST:4173/app?mode=verify")
            .expect("verification route");
        let request = normalize_navigation_request(
            BrowserRequest::Navigate {
                url: "http://LOCALHOST:4173/app?mode=verify".to_owned(),
            },
            &route,
        )
        .expect("normalized navigation");
        assert!(matches!(
            request,
            BrowserRequest::Navigate { ref url }
                if url == "http://localhost:4173/app?mode=verify"
        ));
    }

    #[test]
    fn bridge_candidates_include_repo_root_from_target_binary() {
        let candidates =
            bridge_path_candidates(Path::new("/work/repo/target/debug/medusa-browserd"));
        assert!(
            candidates
                .iter()
                .any(|path| path == Path::new("/work/repo/browser/playwright_bridge.mjs"))
        );
    }

    #[test]
    fn bridge_candidates_never_include_the_working_directory() {
        let candidates =
            bridge_path_candidates(Path::new("/work/repo/target/debug/medusa-browserd"));
        assert!(
            !candidates
                .iter()
                .any(|path| path
                    == Path::new("/work/repo/target/debug/browser/playwright_bridge.mjs"))
        );
    }

    #[test]
    fn missing_bridge_pipes_return_broken_pipe_error() {
        let executable = std::env::current_exe().expect("current test executable");
        let mut child = Command::new(executable)
            .arg("--list")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn pipe-less child");

        let error = take_bridge_stdio(&mut child).expect_err("missing bridge pipes must fail");

        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert!(error.to_string().contains("required stdin/stdout pipes"));
    }

    #[test]
    fn successful_forward_writes_correlated_request_and_parses_response() {
        let mut writer = FailingWriter::default();
        let mut reader = Cursor::new(b"{\"request_id\":4,\"kind\":\"ok\"}\n".to_vec());

        let response = forward_to_bridge(&mut writer, &mut reader, 4, &BrowserRequest::Ping);

        assert!(matches!(response, BrowserResponse::Ok));
        assert_eq!(writer.bytes, b"{\"request_id\":4,\"method\":\"ping\"}\n");
    }

    #[test]
    fn forward_reports_write_flush_read_and_parse_failures() {
        let mut write_failure = FailingWriter {
            fail_write: true,
            ..FailingWriter::default()
        };
        let mut empty = Cursor::new(Vec::<u8>::new());
        assert_eq!(
            error_code(forward_to_bridge(
                &mut write_failure,
                &mut empty,
                1,
                &BrowserRequest::Ping,
            )),
            "request_write"
        );

        let mut flush_failure = FailingWriter {
            fail_flush: true,
            ..FailingWriter::default()
        };
        assert_eq!(
            error_code(forward_to_bridge(
                &mut flush_failure,
                &mut empty,
                1,
                &BrowserRequest::Ping,
            )),
            "request_flush"
        );

        let mut writer = FailingWriter::default();
        let mut read_failure = FailingReader;
        assert_eq!(
            error_code(forward_to_bridge(
                &mut writer,
                &mut read_failure,
                1,
                &BrowserRequest::Ping,
            )),
            "response_frame"
        );

        let mut malformed = Cursor::new(b"not-json\n".to_vec());
        assert_eq!(
            error_code(forward_to_bridge(
                &mut writer,
                &mut malformed,
                1,
                &BrowserRequest::Ping,
            )),
            "response_parse"
        );
    }

    #[test]
    fn response_writer_emits_one_correlated_json_line() {
        let mut output = Vec::new();

        write_response(&mut output, 9, &BrowserResponse::Ok).unwrap();

        assert_eq!(output, b"{\"request_id\":9,\"kind\":\"ok\"}\n");
    }

    #[test]
    fn bridge_restart_backoff_grows_and_caps() {
        use std::time::Duration;

        assert_eq!(bridge_restart_delay(0), Duration::from_millis(100));
        assert_eq!(bridge_restart_delay(1), Duration::from_millis(200));
        assert_eq!(bridge_restart_delay(2), Duration::from_millis(400));
        assert_eq!(bridge_restart_delay(6), Duration::from_secs(5));
        assert_eq!(bridge_restart_delay(100), Duration::from_secs(5));
    }

    #[test]
    fn transport_failures_are_distinguished_from_bridge_answers() {
        let transport = BrowserResponse::Error {
            code: "response_frame".into(),
            message: "pipe broke".into(),
        };
        assert!(is_bridge_transport_failure(&transport));
        let application = BrowserResponse::Error {
            code: "navigation_failed".into(),
            message: "bridge refused".into(),
        };
        assert!(!is_bridge_transport_failure(&application));
        assert!(!is_bridge_transport_failure(&BrowserResponse::Ok));
    }

    #[test]
    fn readiness_probe_accepts_an_answering_bridge_and_rejects_a_silent_one() {
        use std::time::Duration;

        let ok_frame = format!(
            "{{\"request_id\":{},\"kind\":\"ok\"}}\n",
            super::BRIDGE_PROBE_REQUEST_ID
        );
        let (pipes, readiness) = probe_bridge_stdio(
            FailingWriter::default(),
            Cursor::new(ok_frame.into_bytes()),
            Duration::from_secs(5),
        );
        assert!(pipes.is_some());
        readiness.expect("answering bridge must pass the readiness probe");

        struct SilentReader;
        impl std::io::Read for SilentReader {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                std::thread::sleep(Duration::from_millis(500));
                Ok(0)
            }
        }
        impl BufRead for SilentReader {
            fn fill_buf(&mut self) -> io::Result<&[u8]> {
                std::thread::sleep(Duration::from_millis(500));
                Ok(&[])
            }
            fn consume(&mut self, _amount: usize) {}
        }
        let (_pipes, readiness) = probe_bridge_stdio(
            FailingWriter::default(),
            SilentReader,
            Duration::from_millis(50),
        );
        let error = readiness.expect_err("silent bridge must fail the readiness probe");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn proxy_readiness_probe_accepts_a_live_listener_and_rejects_a_dead_port() {
        use std::net::TcpListener;
        use std::time::Duration;

        let proxy = crate::proxy::spawn().expect("proxy");
        wait_for_proxy_ready(&proxy, Duration::from_secs(5)).expect("live proxy must be ready");

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("dead port probe");
        let address = listener.local_addr().expect("listener address");
        drop(listener);
        let dead = crate::proxy::Proxy::for_test(address);
        assert!(
            wait_for_proxy_ready(&dead, Duration::from_millis(100)).is_err(),
            "closed port must fail the readiness probe"
        );
    }
}
