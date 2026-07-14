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

        let response = forward_to_bridge(&mut bridge, &request);
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

impl Write for Bridge {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stdin.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdin.flush()
    }
}

impl BufRead for Bridge {
    fn read_line(&mut self, buf: &mut String) -> io::Result<usize> {
        self.stdout.read_line(buf)
    }

    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.stdout.fill_buf()
    }

    fn consume(&mut self, n: usize) {
        self.stdout.consume(n);
    }
}

impl io::Read for Bridge {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        io::Read::read(&mut self.stdout, buf)
    }
}

fn forward_to_bridge(bridge: &mut Bridge, request: &BrowserRequest) -> BrowserResponse {
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
    if let Err(e) = bridge.write_all(line.as_bytes()) {
        return BrowserResponse::Error {
            code: "sidecar_write_failed".into(),
            message: e.to_string(),
        };
    }
    if let Err(e) = bridge.flush() {
        return BrowserResponse::Error {
            code: "sidecar_flush_failed".into(),
            message: e.to_string(),
        };
    }
    let mut response = String::new();
    if let Err(e) = bridge.read_line(&mut response) {
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