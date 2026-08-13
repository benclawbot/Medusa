use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use medusa_browser_client::{
    protocol::{
        BrowserRequest, BrowserResponse, BrowserRpcRequest, BrowserRpcResponse,
        MAX_BROWSER_REQUEST_FRAME_BYTES, MAX_BROWSER_RESPONSE_FRAME_BYTES,
    },
    transport::{Transport, read_bounded_frame, send_and_receive},
};

use crate::{proxy, validation::validate_public_url};

const BROWSER_BRIDGE_PATH_ENV: &str = "MEDUSA_BROWSER_BRIDGE_PATH";
const BROWSER_BRIDGE_RELATIVE_PATH: &str = "browser/playwright_bridge.mjs";

pub fn run() -> io::Result<()> {
    let proxy = proxy::spawn()?;
    let mut bridge = spawn_bridge(&proxy).map_err(io::Error::other)?;
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut frame = Vec::with_capacity(4096);
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
        if matches!(request, BrowserRequest::Close) {
            let response = forward_to_bridge(
                &mut bridge.stdin,
                &mut bridge.stdout,
                request_id,
                &request,
            );
            write_response(&mut stdout, request_id, &response)?;
            break;
        }
        if let BrowserRequest::Navigate { ref url } = request {
            let parsed = match url::Url::parse(url) {
                Ok(parsed) => parsed,
                Err(error) => {
                    write_response(
                        &mut stdout,
                        request_id,
                        &BrowserResponse::Error {
                            code: "invalid_url".into(),
                            message: error.to_string(),
                        },
                    )?;
                    continue;
                }
            };
            if let Err(message) = validate_public_url(&parsed) {
                write_response(
                    &mut stdout,
                    request_id,
                    &BrowserResponse::Error {
                        code: "invalid_url".into(),
                        message,
                    },
                )?;
                continue;
            }
        }

        let response = forward_to_bridge(
            &mut bridge.stdin,
            &mut bridge.stdout,
            request_id,
            &request,
        );
        write_response(&mut stdout, request_id, &response)?;
    }
    let _ = bridge.child.kill();
    let _ = bridge.child.wait();
    Ok(())
}

pub(crate) fn check_readiness() -> io::Result<()> {
    resolve_bridge_path().map(|_| ())
}

struct Bridge {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

fn spawn_bridge(proxy: &proxy::Proxy) -> io::Result<Bridge> {
    let bridge_path = resolve_bridge_path()?;
    let mut child = Command::new("node")
        .arg(bridge_path)
        .env("MEDUSA_BROWSER_PROXY", proxy.server())
        .env("MEDUSA_BROWSER_PARENT_PID", std::process::id().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let (stdin, stdout) = take_bridge_stdio(&mut child)?;
    Ok(Bridge {
        child,
        stdin,
        stdout,
    })
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

    let current_dir = std::env::current_dir()?;
    let executable = std::env::current_exe()?;
    for candidate in bridge_path_candidates(&current_dir, &executable) {
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

fn bridge_path_candidates(current_dir: &Path, executable: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![current_dir.join(BROWSER_BRIDGE_RELATIVE_PATH)];
    if let Some(parent) = executable.parent() {
        for ancestor in parent.ancestors().take(4) {
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

    use super::{bridge_path_candidates, forward_to_bridge, take_bridge_stdio, write_response};

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
    fn bridge_candidates_include_repo_root_from_target_binary() {
        let candidates = bridge_path_candidates(
            Path::new("/work/repo/crates/medusa-agent"),
            Path::new("/work/repo/target/debug/medusa-browserd"),
        );
        assert!(
            candidates
                .iter()
                .any(|path| path == Path::new("/work/repo/browser/playwright_bridge.mjs"))
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
        assert_eq!(
            writer.bytes,
            b"{\"request_id\":4,\"method\":\"ping\"}\n"
        );
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
}
