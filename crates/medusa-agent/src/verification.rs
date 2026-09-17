use std::{
    fs::{self, File},
    io::Read,
    path::Path,
    process::{ExitStatus, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, hidden_command};
use medusa_process_containment::OwnedProcessTree;

mod static_verification_server;

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

pub(crate) fn required_ui_verification(repo: &Path) -> MedusaResult<VerificationResult> {
    let server = static_verification_server::StaticVerificationServer::start(repo)
        .map_err(|error| {
            dependency_error(format!("UI changes require a static artifact: {error}"))
        })?
        .ok_or_else(|| dependency_error("UI changes require a generated index.html"))?;
    let (status, body) = server
        .probe()
        .map_err(|error| dependency_error(format!("static UI verification failed: {error}")))?;
    let document_nonempty = !body.trim().is_empty();
    let missing_alt = count_missing_alt(&body);
    let unlabeled_controls = count_unlabeled_controls(&body);
    let passed = status < 400 && document_nonempty && missing_alt == 0 && unlabeled_controls == 0;
    Ok(VerificationResult {
        passed,
        evidence: vec![
            "ui_verification_mode=static_http".to_owned(),
            format!("ui_status={status}"),
            format!("ui_document_nonempty={document_nonempty}"),
            format!(
                "ui_accessibility=missing_alt:{missing_alt},unlabeled_controls:{unlabeled_controls}"
            ),
            format!("ui_result={}", if passed { "passed" } else { "failed" }),
        ],
    })
}

fn count_missing_alt(document: &str) -> usize {
    document
        .split('<')
        .filter(|fragment| fragment.trim_start().starts_with("img") && !fragment.contains("alt="))
        .count()
}

fn count_unlabeled_controls(document: &str) -> usize {
    ["button", "input", "select", "textarea"]
        .into_iter()
        .map(|tag| {
            document
                .split('<')
                .filter(|fragment| {
                    let fragment = fragment.trim_start();
                    fragment.starts_with(tag)
                        && !fragment.contains("aria-label=")
                        && !fragment.contains("aria-labelledby=")
                        && !fragment.contains("title=")
                })
                .count()
        })
        .sum()
}

fn dependency_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        message,
    )
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
    let mut command = hidden_command(program);
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
    fn missing_command_is_environment_error() {
        let directory = tempfile::tempdir().expect("tempdir");
        let error = execute_verification_command(
            directory.path(),
            "medusa-command-that-does-not-exist-1073",
            &[],
        )
        .expect_err("missing command");
        assert_eq!(error.category, ErrorCategory::Environment);
        assert_eq!(error.code, ErrorCode::DependencyUnavailable);
    }

    #[test]
    fn spawned_non_zero_command_is_tool_execution_result() {
        let directory = tempfile::tempdir().expect("tempdir");
        let result = execute_verification_command(
            directory.path(),
            "rustc",
            &["--medusa-invalid-option-1073".to_owned()],
        )
        .expect("spawned command");
        assert!(!result.passed);
        assert!(!result.timed_out);
        assert_ne!(result.exit_code, Some(0));
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
    fn static_ui_verification_serves_generated_index_without_environment() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(
            directory.path().join("index.html"),
            "<!doctype html><title>Medusa test page</title>",
        )
        .expect("index");

        let server = static_verification_server::StaticVerificationServer::start(directory.path())
            .expect("server")
            .expect("index should enable automatic verification");
        assert!(server.route().starts_with("http://127.0.0.1:"));
        assert_eq!(
            static_verification_server::fetch_for_test(&server),
            (
                200,
                "<!doctype html><title>Medusa test page</title>".to_owned()
            )
        );
        assert_eq!(
            server.probe().expect("static verification probe"),
            (
                200,
                "<!doctype html><title>Medusa test page</title>".to_owned()
            )
        );
    }
}
