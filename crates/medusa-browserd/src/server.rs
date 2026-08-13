use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use medusa_browser_client::protocol::{BrowserRequest, BrowserResponse};

use crate::{proxy, validation::validate_public_url};

const BROWSER_BRIDGE_PATH_ENV: &str = "MEDUSA_BROWSER_BRIDGE_PATH";
const BROWSER_BRIDGE_RELATIVE_PATH: &str = "browser/playwright_bridge.mjs";

pub fn run() -> io::Result<()> {
    let proxy = proxy::spawn()?;
    let mut bridge = spawn_bridge(&proxy).map_err(io::Error::other)?;
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut line = String::new();
    loop {
        line.clear();
        let n = stdin.lock().read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let request: BrowserRequest = match serde_json::from_str(line.trim()) {
            Ok(req) => req,
            Err(e) => {
                write_response(
                    &mut stdout,
                    &BrowserResponse::Error {
                        code: "invalid_request".into(),
                        message: e.to_string(),
                    },
                )?;
                continue;
            }
        };

        if matches!(request, BrowserRequest::Ping) {
            write_response(&mut stdout, &BrowserResponse::Ok)?;
            continue;
        }
        if matches!(request, BrowserRequest::Close) {
            write_response(&mut stdout, &BrowserResponse::Ok)?;
            break;
        }
        if let BrowserRequest::Navigate { ref url } = request {
            let parsed = match url::Url::parse(url) {
                Ok(parsed) => parsed,
                Err(e) => {
                    write_response(
                        &mut stdout,
                        &BrowserResponse::Error {
                            code: "invalid_url".into(),
                            message: e.to_string(),
                        },
                    )?;
                    continue;
                }
            };
            if let Err(message) = validate_public_url(&parsed) {
                write_response(
                    &mut stdout,
                    &BrowserResponse::Error {
                        code: "invalid_url".into(),
                        message,
                    },
                )?;
                continue;
            }
        }

        let response = forward_to_bridge(&mut bridge.stdin, &mut bridge.stdout, &request);
        write_response(&mut stdout, &response)?;
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

fn forward_to_bridge<W: Write, R: BufRead>(
    writer: &mut W,
    reader: &mut R,
    request: &BrowserRequest,
) -> BrowserResponse {
    let mut line = match serde_json::to_string(request) {
        Ok(s) => s,
        Err(e) => {
            return BrowserResponse::Error {
                code: "internal".into(),
                message: e.to_string(),
            };
        }
    };
    line.push('\n');
    if let Err(e) = writer.write_all(line.as_bytes()) {
        return BrowserResponse::Error {
            code: "sidecar_write_failed".into(),
            message: e.to_string(),
        };
    }
    if let Err(e) = writer.flush() {
        return BrowserResponse::Error {
            code: "sidecar_flush_failed".into(),
            message: e.to_string(),
        };
    }
    let mut response = String::new();
    if let Err(e) = reader.read_line(&mut response) {
        return BrowserResponse::Error {
            code: "sidecar_read_failed".into(),
            message: e.to_string(),
        };
    }
    match serde_json::from_str(response.trim()) {
        Ok(parsed) => parsed,
        Err(e) => BrowserResponse::Error {
            code: "sidecar_parse_failed".into(),
            message: e.to_string(),
        },
    }
}

fn write_response<W: Write>(out: &mut W, response: &BrowserResponse) -> io::Result<()> {
    let mut line = serde_json::to_string(response).map_err(io::Error::other)?;
    line.push('\n');
    out.write_all(line.as_bytes())?;
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
    fn successful_forward_writes_request_and_parses_response() {
        let mut writer = FailingWriter::default();
        let mut reader = Cursor::new(b"{\"kind\":\"ok\"}\n".to_vec());

        let response = forward_to_bridge(&mut writer, &mut reader, &BrowserRequest::Ping);

        assert!(matches!(response, BrowserResponse::Ok));
        assert_eq!(writer.bytes, b"{\"method\":\"ping\"}\n");
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
                &BrowserRequest::Ping,
            )),
            "sidecar_write_failed"
        );

        let mut flush_failure = FailingWriter {
            fail_flush: true,
            ..FailingWriter::default()
        };
        assert_eq!(
            error_code(forward_to_bridge(
                &mut flush_failure,
                &mut empty,
                &BrowserRequest::Ping,
            )),
            "sidecar_flush_failed"
        );

        let mut writer = FailingWriter::default();
        let mut read_failure = FailingReader;
        assert_eq!(
            error_code(forward_to_bridge(
                &mut writer,
                &mut read_failure,
                &BrowserRequest::Ping,
            )),
            "sidecar_read_failed"
        );

        let mut malformed = Cursor::new(b"not-json\n".to_vec());
        assert_eq!(
            error_code(forward_to_bridge(
                &mut writer,
                &mut malformed,
                &BrowserRequest::Ping,
            )),
            "sidecar_parse_failed"
        );
    }

    #[test]
    fn response_writer_emits_one_json_line() {
        let mut output = Vec::new();

        write_response(&mut output, &BrowserResponse::Ok).unwrap();

        assert_eq!(output, b"{\"kind\":\"ok\"}\n");
    }
}
