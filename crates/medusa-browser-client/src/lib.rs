pub mod network_policy;
pub mod protocol;
pub mod transport;

use std::io::{BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

pub use protocol::{BrowserRequest, BrowserResponse, ElementRef, TabInfo};
use transport::{Transport, read_bounded_frame, send_and_receive};

pub struct BrowserClient {
    child: Child,
    transport: Option<Box<dyn Transport>>,
    next_request_id: u64,
}

impl BrowserClient {
    pub fn spawn(command: &str) -> MedusaResult<Self> {
        Self::spawn_with_env(command, &[])
    }

    pub fn spawn_with_env(command: &str, environment: &[(&str, &str)]) -> MedusaResult<Self> {
        let mut command_builder = Command::new(command);
        command_builder.arg("--stdio");
        for (key, value) in environment {
            command_builder.env(key, value);
        }
        #[cfg(target_os = "windows")]
        command_builder.creation_flags(0x0800_0000);
        let mut child = command_builder
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| spawn_err(format!("could not launch {command}: {error}")))?;
        let (stdin, stdout) = take_stdio(&mut child, command)?;
        let pipe = StdioPipe::new(stdout, stdin);
        Ok(Self {
            child,
            transport: Some(Box::new(pipe)),
            next_request_id: 1,
        })
    }

    pub fn request(&mut self, request: BrowserRequest) -> MedusaResult<BrowserResponse> {
        static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);
        self.request_with_control(request, Duration::from_secs(30), &NEVER_CANCELLED)
    }

    pub fn request_with_control(
        &mut self,
        request: BrowserRequest,
        timeout: Duration,
        cancellation: &AtomicBool,
    ) -> MedusaResult<BrowserResponse> {
        if cancellation.load(Ordering::Acquire) {
            self.terminate_child();
            return Err(control_error(
                "cancelled",
                "browser request cancelled before dispatch",
                false,
            ));
        }
        let Some(mut transport) = self.transport.take() else {
            self.terminate_child();
            return Err(control_error(
                "sidecar_reset",
                "browser sidecar transport is unavailable after reset",
                true,
            ));
        };

        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.checked_add(1).unwrap_or(1).max(1);
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("medusa-browser-request".to_owned())
            .spawn(move || {
                let result = send_and_receive(transport.as_mut(), request_id, &request);
                let _ = sender.send((transport, result));
            });
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                self.terminate_child();
                return Err(control_error(
                    "sidecar_reset",
                    format!("could not start browser request worker: {error}"),
                    true,
                ));
            }
        };

        let timeout = timeout.max(Duration::from_millis(1));
        let started = Instant::now();
        loop {
            if cancellation.load(Ordering::Acquire) {
                self.terminate_child();
                drop(worker);
                return Err(control_error(
                    "cancelled",
                    format!("browser request {request_id} cancelled in flight; sidecar reset"),
                    false,
                ));
            }
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                self.terminate_child();
                drop(worker);
                return Err(control_error(
                    "timeout",
                    format!(
                        "browser request {request_id} exceeded its {} ms deadline; sidecar reset",
                        timeout.as_millis()
                    ),
                    true,
                ));
            }
            let wait = timeout
                .saturating_sub(elapsed)
                .min(Duration::from_millis(20));
            match receiver.recv_timeout(wait) {
                Ok((transport, result)) => {
                    self.transport = Some(transport);
                    if worker.join().is_err() {
                        self.terminate_child();
                        return Err(control_error(
                            "sidecar_reset",
                            "browser request worker panicked; sidecar reset",
                            true,
                        ));
                    }
                    return result;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.terminate_child();
                    let _ = worker.join();
                    return Err(control_error(
                        "sidecar_reset",
                        "browser request worker disconnected; sidecar reset",
                        true,
                    ));
                }
            }
        }
    }

    fn terminate_child(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for BrowserClient {
    fn drop(&mut self) {
        self.terminate_child();
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
    fn read_frame(&mut self, buf: &mut Vec<u8>, max_bytes: usize) -> std::io::Result<usize> {
        read_bounded_frame(&mut self.reader, buf, max_bytes)
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

fn control_error(kind: &'static str, message: impl Into<String>, retryable: bool) -> MedusaError {
    let mut error = MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Transient,
        message,
    )
    .with_retryable(retryable);
    error
        .context
        .insert("browser_error_kind".to_owned(), serde_json::json!(kind));
    error
        .context
        .insert("browser_sidecar_reset".to_owned(), serde_json::json!(true));
    error
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Arc;

    use super::*;

    struct BlockingTransport;

    impl Write for BlockingTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Transport for BlockingTransport {
        fn read_frame(&mut self, _buf: &mut Vec<u8>, _max_bytes: usize) -> io::Result<usize> {
            thread::sleep(Duration::from_millis(250));
            Ok(0)
        }
    }

    fn test_client_with_transport(transport: Box<dyn Transport>) -> BrowserClient {
        let executable = std::env::current_exe().expect("current test executable");
        let child = Command::new(executable)
            .arg("--list")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("test child");
        BrowserClient {
            child,
            transport: Some(transport),
            next_request_id: 1,
        }
    }

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

    #[test]
    fn request_deadline_bounds_a_blocked_transport() {
        let mut client = test_client_with_transport(Box::new(BlockingTransport));
        let cancellation = AtomicBool::new(false);
        let started = Instant::now();
        let error = client
            .request_with_control(
                BrowserRequest::Ping,
                Duration::from_millis(20),
                &cancellation,
            )
            .expect_err("blocked request must time out");
        assert!(started.elapsed() < Duration::from_millis(200));
        assert!(error.message.contains("deadline"));
        assert_eq!(
            error.context.get("browser_error_kind"),
            Some(&serde_json::json!("timeout"))
        );
    }

    #[test]
    fn cancellation_interrupts_an_in_flight_request() {
        let mut client = test_client_with_transport(Box::new(BlockingTransport));
        let cancellation = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancellation);
        let toggler = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            trigger.store(true, Ordering::Release);
        });
        let started = Instant::now();
        let error = client
            .request_with_control(BrowserRequest::Ping, Duration::from_secs(1), &cancellation)
            .expect_err("cancelled request must stop");
        toggler.join().expect("cancellation toggler");
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(
            error.context.get("browser_error_kind"),
            Some(&serde_json::json!("cancelled"))
        );
    }
}
