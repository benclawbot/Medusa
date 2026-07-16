use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use medusa_browser_client::protocol::{BrowserRequest, BrowserResponse};

use crate::validation::validate_public_url;

pub fn run() -> io::Result<()> {
    let mut bridge = spawn_bridge().map_err(io::Error::other)?;
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

struct Bridge {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

fn spawn_bridge() -> io::Result<Bridge> {
    let mut child = Command::new("node")
        .arg("browser/playwright_bridge.mjs")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdin = child.stdin.take().expect("stdin");
    let stdout = BufReader::new(child.stdout.take().expect("stdout"));
    Ok(Bridge {
        child,
        stdin,
        stdout,
    })
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

    use medusa_browser_client::protocol::{BrowserRequest, BrowserResponse};

    use super::{forward_to_bridge, write_response};

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
