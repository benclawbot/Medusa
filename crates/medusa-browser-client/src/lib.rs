pub mod protocol;
pub mod transport;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

pub use protocol::{BrowserRequest, BrowserResponse, ElementRef, TabInfo};
use transport::{Transport, send_and_receive};

pub struct BrowserClient {
    child: Child,
    transport: Box<dyn Transport>,
}

impl BrowserClient {
    pub fn spawn(command: &str) -> MedusaResult<Self> {
        let mut child = Command::new(command)
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| spawn_err(format!("could not launch {command}: {e}")))?;
        let (stdin, stdout) = take_stdio(&mut child, command)?;
        let pipe = StdioPipe::new(stdout, stdin);
        Ok(Self {
            child,
            transport: Box::new(pipe),
        })
    }

    pub fn request(&mut self, request: BrowserRequest) -> MedusaResult<BrowserResponse> {
        send_and_receive(self.transport.as_mut(), &request)
    }
}

impl Drop for BrowserClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn take_stdio(child: &mut Child, command: &str) -> MedusaResult<(ChildStdin, ChildStdout)> {
    match (child.stdin.take(), child.stdout.take()) {
        (Some(stdin), Some(stdout)) => Ok((stdin, stdout)),
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            Err(spawn_err(format!(
                "launched {command} without the required stdin/stdout pipes"
            )))
        }
    }
}

struct StdioPipe {
    reader: BufReader<ChildStdout>,
    writer: ChildStdin,
}

impl StdioPipe {
    fn new(stdout: ChildStdout, stdin: ChildStdin) -> Self {
        Self {
            reader: BufReader::new(stdout),
            writer: stdin,
        }
    }
}

impl Write for StdioPipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writer.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

impl Transport for StdioPipe {
    fn read_frame(&mut self, buf: &mut String) -> std::io::Result<usize> {
        self.reader.read_line(buf)
    }
}

fn spawn_err(message: String) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        message,
    )
    .with_retryable(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_stdio_pipes_return_a_retryable_dependency_error() {
        let executable = std::env::current_exe().expect("current test executable");
        let mut child = Command::new(executable)
            .arg("--list")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn pipe-less child");

        let error = take_stdio(&mut child, "test-browser").expect_err("missing pipes must fail");

        assert_eq!(error.code, ErrorCode::DependencyUnavailable);
        assert_eq!(error.category, ErrorCategory::Transient);
        assert!(error.retryable);
        assert!(error.message.contains("required stdin/stdout pipes"));
    }
}
