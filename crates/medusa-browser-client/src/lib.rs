pub mod network_policy;
pub mod protocol;
pub mod transport;

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
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
        let resolved = resolve_sidecar_command(command)?;
        let mut command_builder = Command::new(&resolved);
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
        match handshake_transport(pipe, SIDECAR_HANDSHAKE_TIMEOUT) {
            Ok(pipe) => Ok(Self {
                child,
                transport: Some(Box::new(pipe)),
                next_request_id: 1,
            }),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
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

/// Deadline for the post-spawn Ping handshake that proves the sidecar is alive.
const SIDECAR_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Resolves a sidecar command to a canonical executable path.
///
/// Bare names are located via `PATH`; paths are canonicalized. The result is
/// verified to be an existing executable file before any child is spawned, so
/// a typo'd `MEDUSA_BROWSER_PATH` fails fast instead of launching an
/// unintended program.
fn resolve_sidecar_command(command: &str) -> MedusaResult<PathBuf> {
    if command.trim().is_empty() {
        return Err(spawn_err(
            "browser sidecar command must not be empty".to_owned(),
        ));
    }
    if command
        .bytes()
        .any(|byte| byte == 0 || byte == b'\n' || byte == b'\r')
    {
        return Err(spawn_err(format!(
            "browser sidecar command contains an invalid control character: {command}"
        )));
    }
    if command.contains([';', '|', '&', '$', '`']) {
        return Err(spawn_err(format!(
            "browser sidecar command must be a single executable path, not a shell expression: {command}"
        )));
    }
    if command.contains('/') || command.contains('\\') {
        let canonical = std::fs::canonicalize(command).map_err(|error| {
            spawn_err(format!(
                "browser sidecar executable does not exist at {command}: {error}"
            ))
        })?;
        if !canonical.is_file() {
            return Err(spawn_err(format!(
                "browser sidecar executable is not a file: {}",
                canonical.display()
            )));
        }
        return Ok(canonical);
    }
    search_path(command).ok_or_else(|| {
        spawn_err(format!(
            "browser sidecar executable `{command}` was not found on PATH"
        ))
    })
}

fn search_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        if directory.as_os_str().is_empty() {
            continue;
        }
        #[cfg(target_os = "windows")]
        {
            let with_extension = directory.join(format!("{name}.exe"));
            if is_executable_file(&with_extension) {
                return Some(with_extension);
            }
        }
        let candidate = directory.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Proves the freshly spawned sidecar is alive with a Ping round-trip before
/// `spawn_with_env` reports success. The transport read blocks, so the
/// handshake runs on a worker thread bounded by `timeout`; on timeout the
/// worker is detached and exits once the caller kills the child and its pipes
/// close.
fn handshake_transport<T: Transport + Send + 'static>(
    mut transport: T,
    timeout: Duration,
) -> MedusaResult<T> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker = thread::Builder::new()
        .name("medusa-browser-handshake".to_owned())
        .spawn(move || {
            let result = send_and_receive(&mut transport, 1, &BrowserRequest::Ping);
            let _ = sender.send((transport, result));
        })
        .map_err(|error| {
            spawn_err(format!(
                "could not start browser sidecar handshake worker: {error}"
            ))
        })?;
    let timeout = timeout.max(Duration::from_millis(1));
    match receiver.recv_timeout(timeout) {
        Ok((transport, result)) => {
            let _ = worker.join();
            match result {
                Ok(BrowserResponse::Ok) => Ok(transport),
                Ok(other) => Err(spawn_err(format!(
                    "browser sidecar handshake failed: unexpected {other:?} instead of Ok"
                ))),
                Err(error) => Err(spawn_err(format!(
                    "browser sidecar handshake failed: {error}"
                ))),
            }
        }
        Err(mpsc::RecvTimeoutError::Timeout) => Err(spawn_err(format!(
            "browser sidecar did not answer the readiness handshake within {} ms",
            timeout.as_millis()
        ))),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = worker.join();
            Err(spawn_err(
                "browser sidecar handshake worker exited before answering".to_owned(),
            ))
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
    let category = if retryable {
        ErrorCategory::Transient
    } else {
        ErrorCategory::Execution
    };
    let mut error = MedusaError::new(ErrorCode::ToolExecutionFailed, category, message)
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

    #[test]
    fn control_error_category_follows_the_retryable_flag() {
        let fatal = control_error("cancelled", "browser request cancelled", false);
        assert!(!fatal.retryable);
        assert_eq!(fatal.category, ErrorCategory::Execution);
        let retryable = control_error("timeout", "browser request timed out", true);
        assert!(retryable.retryable);
        assert_eq!(retryable.category, ErrorCategory::Transient);
    }

    #[test]
    fn sidecar_command_resolution_rejects_missing_and_shell_expressions() {
        assert!(resolve_sidecar_command("").is_err());
        assert!(resolve_sidecar_command("definitely-not-a-medusa-binary-xyz").is_err());
        assert!(resolve_sidecar_command("test-browserd; rm -rf /").is_err());
        assert!(resolve_sidecar_command("test-browserd | tee log").is_err());
        assert!(resolve_sidecar_command("/nonexistent/medusa-browserd").is_err());
    }

    #[test]
    fn sidecar_command_resolution_canonicalizes_an_explicit_path() {
        let executable = std::env::current_exe().expect("current test executable");
        let resolved = resolve_sidecar_command(executable.to_str().expect("utf-8 test executable"))
            .expect("explicit test executable must resolve");
        assert!(resolved.is_file());
    }

    #[derive(Debug)]
    struct ScriptTransport {
        response: Vec<u8>,
        delay: Duration,
    }

    impl Write for ScriptTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Transport for ScriptTransport {
        fn read_frame(&mut self, buf: &mut Vec<u8>, max_bytes: usize) -> io::Result<usize> {
            if !self.delay.is_zero() {
                thread::sleep(self.delay);
            }
            let mut reader = std::io::BufReader::new(&self.response[..]);
            read_bounded_frame(&mut reader, buf, max_bytes)
        }
    }

    #[test]
    fn handshake_proves_the_sidecar_is_alive_before_spawn_returns() {
        let answering = ScriptTransport {
            response: b"{\"request_id\":1,\"kind\":\"ok\"}\n".to_vec(),
            delay: Duration::ZERO,
        };
        let transport = handshake_transport(answering, Duration::from_secs(5))
            .expect("answering sidecar must pass the handshake");
        drop(transport);

        let silent = ScriptTransport {
            response: Vec::new(),
            delay: Duration::from_millis(500),
        };
        let error = handshake_transport(silent, Duration::from_millis(20))
            .expect_err("silent sidecar must fail the handshake");
        assert_eq!(error.code, ErrorCode::DependencyUnavailable);
        assert!(error.message.contains("readiness handshake"));
    }
}
