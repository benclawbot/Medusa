use std::{
    fs::{self, File},
    io::Read,
    path::Path,
    process::{Command, ExitStatus, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use medusa_browser_client::{BrowserClient, BrowserRequest, BrowserResponse};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_process_containment::OwnedProcessTree;

const VERIFICATION_TIMEOUT: Duration = Duration::from_secs(300);
const VERIFICATION_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExecutedVerificationCommand {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub passed: bool,
}

pub(crate) fn execute_verification_command(
    repo: &Path,
    program: &str,
    args: &[String],
) -> MedusaResult<ExecutedVerificationCommand> {
    let cancellation = AtomicBool::new(false);
    execute_verification_command_cancellable(repo, program, args, &cancellation)
}

pub(crate) fn execute_verification_command_cancellable(
    repo: &Path,
    program: &str,
    args: &[String],
    cancellation: &AtomicBool,
) -> MedusaResult<ExecutedVerificationCommand> {
    let program = platform_program(program);
    let output = run_supervised_command(repo, program, args, VERIFICATION_TIMEOUT, cancellation)?;
    Ok(ExecutedVerificationCommand {
        exit_code: output.status.code(),
        timed_out: output.timed_out,
        duration_ms: output.duration.as_millis() as u64,
        stdout: output.stdout,
        stderr: output.stderr,
        passed: output.status.success() && !output.timed_out,
    })
}

pub(crate) fn required_browser_verification(repo: &Path) -> MedusaResult<VerificationResult> {
    let route = std::env::var("MEDUSA_BROWSER_VERIFY_URL").map_err(|_| {
        MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Environment,
            "UI changes require browser verification, but MEDUSA_BROWSER_VERIFY_URL is not set; start the application and provide a runnable route",
        )
    })?;
    let command = std::env::var("MEDUSA_BROWSERD").unwrap_or_else(|_| "medusa-browserd".into());
    let mut client = BrowserClient::spawn_with_env(
        &command,
        &[("MEDUSA_BROWSER_VERIFICATION_ORIGIN", route.as_str())],
    )
    .map_err(|error| {
        MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Environment,
            format!(
                "UI changes require browser verification, but {command} could not start: {error}"
            ),
        )
    })?;
    let mut result = VerificationResult {
        passed: true,
        evidence: vec![format!("browser_requested_route={route}")],
    };

    match client.request(BrowserRequest::Navigate { url: route })? {
        BrowserResponse::Navigate { final_url, status } => {
            result.evidence.push(format!("browser_route={final_url}"));
            result.evidence.push(format!("browser_status={status}"));
            result.passed &= status < 400;
        }
        BrowserResponse::Error { code, message } => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_error={code}:{message}"));
            return Ok(result);
        }
        other => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_unexpected_navigation={other:?}"));
            return Ok(result);
        }
    }

    match client.request(BrowserRequest::Snapshot)? {
        BrowserResponse::Snapshot { text, refs } => {
            let nonempty = !text.trim().is_empty();
            result
                .evidence
                .push(format!("browser_snapshot_nonempty={nonempty}"));
            result
                .evidence
                .push(format!("browser_snapshot_refs={}", refs.len()));
            result.passed &= nonempty;
        }
        BrowserResponse::Error { code, message } => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_snapshot_error={code}:{message}"));
        }
        other => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_unexpected_snapshot={other:?}"));
        }
    }

    match client.request(BrowserRequest::Evaluate {
        expression: "JSON.stringify(globalThis.__MEDUSA_CONSOLE_ERRORS__ || [])".to_owned(),
    })? {
        BrowserResponse::Evaluate { value } => {
            let serialized = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            let clean = serialized == "[]" || serialized == "\"[]\"" || serialized == "null";
            result
                .evidence
                .push(format!("browser_console_errors={serialized}"));
            result.passed &= clean;
        }
        BrowserResponse::Error { code, message } => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_console_probe_error={code}:{message}"));
        }
        other => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_unexpected_console_probe={other:?}"));
        }
    }

    match client.request(BrowserRequest::Evaluate {
        expression: r#"JSON.stringify({missing_alt:document.querySelectorAll('img:not([alt])').length,unlabeled_controls:Array.from(document.querySelectorAll('button,input,select,textarea,a[href]')).filter((element)=>!(element.getAttribute('aria-label')||element.getAttribute('aria-labelledby')||element.textContent?.trim()||element.getAttribute('title'))).length})"#.to_owned(),
    })? {
        BrowserResponse::Evaluate { value } => {
            let report = value
                .as_str()
                .and_then(|serialized| serde_json::from_str::<serde_json::Value>(serialized).ok())
                .unwrap_or(value);
            let missing_alt = report
                .get("missing_alt")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(u64::MAX);
            let unlabeled_controls = report
                .get("unlabeled_controls")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(u64::MAX);
            result.evidence.push(format!(
                "browser_accessibility=missing_alt:{missing_alt},unlabeled_controls:{unlabeled_controls}"
            ));
            result.passed &= missing_alt == 0 && unlabeled_controls == 0;
        }
        BrowserResponse::Error { code, message } => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_accessibility_error={code}:{message}"));
        }
        other => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_unexpected_accessibility={other:?}"));
        }
    }

    match client.request(BrowserRequest::Screenshot { full_page: true })? {
        BrowserResponse::Screenshot {
            format,
            bytes_base64,
        } => {
            let directory = repo.join(".medusa/verification/screenshots");
            fs::create_dir_all(&directory)?;
            let path = directory.join(format!("{}.{}", ulid::Ulid::new(), format));
            fs::write(&path, decode_base64(&bytes_base64)?)?;
            result
                .evidence
                .push(format!("browser_screenshot={}", path.display()));
        }
        BrowserResponse::Error { code, message } => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_screenshot_error={code}:{message}"));
        }
        other => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_unexpected_screenshot={other:?}"));
        }
    }
    result.evidence.push(format!(
        "browser_result={}",
        if result.passed { "passed" } else { "failed" }
    ));
    Ok(result)
}

fn run_supervised_command<S: AsRef<std::ffi::OsStr>>(
    repo: &Path,
    program: &str,
    args: &[S],
    timeout: Duration,
    cancellation: &AtomicBool,
) -> MedusaResult<SupervisedOutput> {
    if cancellation.load(Ordering::Acquire) {
        return Err(cancelled_command(program));
    }
    let id = ulid::Ulid::new();
    let stdout_path = std::env::temp_dir().join(format!("medusa-verify-{id}.stdout"));
    let stderr_path = std::env::temp_dir().join(format!("medusa-verify-{id}.stderr"));
    let stdout_file = File::create(&stdout_path)?;
    let stderr_file = File::create(&stderr_path)?;
    let started = Instant::now();
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(repo)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    let mut child =
        OwnedProcessTree::spawn(&mut command).map_err(|error| command_error(program, error))?;
    let (status, timed_out) = loop {
        if cancellation.load(Ordering::Acquire) {
            let _ = child.terminate();
            let _ = child.wait();
            let _ = fs::remove_file(&stdout_path);
            let _ = fs::remove_file(&stderr_path);
            return Err(cancelled_command(program));
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| command_error(program, error))?
        {
            break (status, false);
        }
        if started.elapsed() >= timeout {
            let _ = child.terminate();
            let status = child
                .wait()
                .map_err(|error| command_error(program, error))?;
            break (status, true);
        }
        thread::sleep(VERIFICATION_POLL_INTERVAL);
    };
    let stdout = read_and_remove(&stdout_path)?;
    let stderr = read_and_remove(&stderr_path)?;
    Ok(SupervisedOutput {
        status,
        stdout,
        stderr,
        timed_out,
        duration: started.elapsed(),
    })
}

fn read_and_remove(path: &Path) -> MedusaResult<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    let _ = fs::remove_file(path);
    Ok(bytes)
}

fn decode_base64(input: &str) -> MedusaResult<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = Vec::new();
    let mut chunk = [0u8; 4];
    let mut length = 0;
    for byte in input.bytes().filter(|byte| !byte.is_ascii_whitespace()) {
        if byte == b'=' {
            chunk[length] = 64;
        } else if let Some(index) = TABLE.iter().position(|candidate| *candidate == byte) {
            chunk[length] = index as u8;
        } else {
            return Err(MedusaError::new(
                ErrorCode::InvalidConfiguration,
                ErrorCategory::Validation,
                "browser screenshot returned invalid base64",
            ));
        }
        length += 1;
        if length == 4 {
            output.push((chunk[0] << 2) | (chunk[1] >> 4));
            if chunk[2] != 64 {
                output.push((chunk[1] << 4) | (chunk[2] >> 2));
            }
            if chunk[3] != 64 {
                output.push((chunk[2] << 6) | chunk[3]);
            }
            length = 0;
        }
    }
    if length != 0 {
        return Err(MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            "browser screenshot base64 was truncated",
        ));
    }
    Ok(output)
}

#[cfg(windows)]
fn platform_program(program: &str) -> &str {
    match program {
        "npm" => "npm.cmd",
        "pnpm" => "pnpm.cmd",
        "yarn" => "yarn.cmd",
        "bun" => "bun.exe",
        "python" => "python.exe",
        "cargo" => "cargo.exe",
        "rustfmt" => "rustfmt.exe",
        "bash" => "bash.exe",
        "powershell" => "powershell.exe",
        _ => program,
    }
}

#[cfg(not(windows))]
fn platform_program(program: &str) -> &str {
    program
}

fn command_error(program: &str, error: std::io::Error) -> MedusaError {
    let message = if error.kind() == std::io::ErrorKind::NotFound {
        format!("verification program `{program}` was not found on PATH")
    } else {
        format!("failed to run verification program `{program}`: {error}")
    };
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        message,
    )
}

fn cancelled_command(program: &str) -> MedusaError {
    let mut error = MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        format!("verification program `{program}` cancelled"),
    );
    error
        .context
        .insert("cancelled".to_owned(), serde_json::Value::Bool(true));
    error
}

struct SupervisedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
    duration: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationResult {
    pub passed: bool,
    pub evidence: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_execution_preserves_raw_streams() {
        let directory = tempfile::tempdir().expect("tempdir");
        let result =
            execute_verification_command(directory.path(), "rustc", &["--version".to_owned()])
                .expect("command");
        assert!(result.passed);
        assert!(!result.stdout.is_empty());
        assert!(result.stderr.is_empty());
    }

    #[test]
    fn pre_cancelled_command_does_not_execute() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cancellation = AtomicBool::new(true);
        let error = execute_verification_command_cancellable(
            directory.path(),
            "rustc",
            &["--version".to_owned()],
            &cancellation,
        )
        .expect_err("cancelled");
        assert_eq!(
            error.context.get("cancelled"),
            Some(&serde_json::Value::Bool(true))
        );
    }

    #[test]
    fn rejects_truncated_base64() {
        assert!(decode_base64("abc").is_err());
    }
}
