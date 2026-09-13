use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::Output,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering},
    },
    time::Duration,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::sync::atomic::Ordering;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::Instant,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use medusa_process_containment::OwnedProcessTree;
use medusa_process_containment::ProcessLimits;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[path = "analysis_process_tracker.rs"]
mod analysis_process_tracker;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use analysis_process_tracker::AnalysisProcessTracker;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "unix_sandbox.rs"]
mod unix_sandbox;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

#[cfg(windows)]
#[path = "windows_sandbox.rs"]
mod windows_sandbox;

const SHELL_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_SHELL_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
#[cfg(target_os = "macos")]
const ANALYSIS_MEMORY_LIMIT_BYTES: u64 = 512 * 1024 * 1024;

pub(crate) fn safe_path(repo: &Path, relative: &str) -> MedusaResult<PathBuf> {
    let path = Path::new(relative);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(policy_denied(format!(
            "path escapes repository: {relative}"
        )));
    }

    let root = repo.canonicalize()?;
    let mut resolved = root.clone();
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err(policy_denied(format!(
                "invalid repository path: {relative}"
            )));
        };
        resolved.push(name);
        if resolved.exists() {
            let metadata = fs::symlink_metadata(&resolved)?;
            if metadata.file_type().is_symlink() {
                return Err(policy_denied(format!(
                    "repository path traverses a symlink: {relative}"
                )));
            }
            let canonical = resolved.canonicalize()?;
            if !canonical.starts_with(&root) {
                return Err(policy_denied(format!(
                    "path escapes repository: {relative}"
                )));
            }
            resolved = canonical;
        }
    }
    if !resolved.starts_with(&root) {
        return Err(policy_denied(format!(
            "path escapes repository: {relative}"
        )));
    }
    Ok(resolved)
}

pub fn validate_shell_command(program: &str, args: &[String]) -> MedusaResult<()> {
    // Admission is platform-neutral. The hard-denial policy blocks commands that
    // can escape containment or mutate host security state; every remaining
    // executable is constrained by bubblewrap, Seatbelt, or AppContainer at
    // execution time. This keeps language/toolchain support consistent across OSes.
    validate_shell_command_hard_denials(program, args)
}

pub(crate) fn validate_shell_command_hard_denials(
    program: &str,
    args: &[String],
) -> MedusaResult<()> {
    medusa_tool_policy::validate_shell_command(program, args).map_err(policy_denied)
}

pub(crate) fn sandboxed_command(
    repo: &Path,
    program: &str,
    args: &[String],
) -> MedusaResult<Output> {
    let cancellation = AtomicBool::new(false);
    sandboxed_command_cancellable(repo, program, args, &cancellation)
}

pub(crate) fn sandboxed_command_cancellable(
    repo: &Path,
    program: &str,
    args: &[String],
    cancellation: &AtomicBool,
) -> MedusaResult<Output> {
    sandboxed_command_cancellable_with_limits(
        repo,
        program,
        args,
        cancellation,
        CommandLimits::default(),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CommandLimits {
    pub timeout: Duration,
    pub max_output_bytes: usize,
    pub process_limits: ProcessLimits,
    pub max_disk_bytes: Option<u64>,
}

impl Default for CommandLimits {
    fn default() -> Self {
        Self {
            timeout: SHELL_COMMAND_TIMEOUT,
            max_output_bytes: DEFAULT_SHELL_OUTPUT_BYTES,
            process_limits: ProcessLimits::default(),
            max_disk_bytes: None,
        }
    }
}

pub(crate) fn sandboxed_command_cancellable_with_limits(
    repo: &Path,
    program: &str,
    args: &[String],
    cancellation: &AtomicBool,
    limits: CommandLimits,
) -> MedusaResult<Output> {
    #[cfg(target_os = "linux")]
    {
        let root = repo.canonicalize()?;
        let mut command = unix_sandbox::linux_command(&root, program, args)?;
        output_with_timeout(
            &mut command,
            "Linux bubblewrap sandbox",
            cancellation,
            &root,
            program,
            args,
            limits,
        )
    }
    #[cfg(target_os = "macos")]
    {
        let root = repo.canonicalize()?;
        let profile_path = std::env::temp_dir().join(format!(
            "medusa-sandbox-{}-{}.sb",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let mut command = unix_sandbox::macos_command(&root, program, args, &profile_path)?;
        let result = output_with_timeout(
            &mut command,
            "macOS sandbox-exec sandbox",
            cancellation,
            &root,
            program,
            args,
            limits,
        );
        let _ = fs::remove_file(&profile_path);
        result
    }
    #[cfg(windows)]
    {
        windows_sandbox::run_cancellable_with_limits(repo, program, args, cancellation, limits)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (repo, program, args);
        Err(sandbox_unavailable(
            "no containment backend is available for this platform",
        ))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn output_with_timeout(
    command: &mut Command,
    description: &str,
    cancellation: &AtomicBool,
    root: &Path,
    program: &str,
    args: &[String],
    limits: CommandLimits,
) -> MedusaResult<Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut tree =
        OwnedProcessTree::spawn_with_limits(command, limits.process_limits).map_err(|error| {
            MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Environment,
                format!("{description} unavailable: {error}"),
            )
        })?;
    let is_analysis_process = root.to_string_lossy().contains("/analysis-workspace-v1/");
    let mut process_tracker = if is_analysis_process {
        Some(AnalysisProcessTracker::started(
            root,
            program,
            args,
            tree.ownership_receipt(),
        )?)
    } else {
        None
    };
    let initial_disk_bytes = limits.max_disk_bytes.map(|_| directory_bytes(root));
    let stdout = tree.take_stdout().ok_or_else(|| {
        MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            format!("{description} stdout pipe was unavailable"),
        )
    })?;
    let stderr = tree.take_stderr().ok_or_else(|| {
        MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            format!("{description} stderr pipe was unavailable"),
        )
    })?;
    let output_bytes = Arc::new(AtomicUsize::new(0));
    let output_limit_reached = Arc::new(AtomicBool::new(false));
    let stdout_reader = spawn_bounded_reader(
        stdout,
        limits.max_output_bytes,
        Arc::clone(&output_bytes),
        Arc::clone(&output_limit_reached),
    );
    let stderr_reader = spawn_bounded_reader(
        stderr,
        limits.max_output_bytes,
        Arc::clone(&output_bytes),
        Arc::clone(&output_limit_reached),
    );
    let started = Instant::now();
    loop {
        if cancellation.load(Ordering::Acquire) {
            let _ = tree.terminate();
            let _ = tree.wait();
            if let Some(tracker) = process_tracker.take() {
                let _ = tracker.failed("analysis execution cancelled");
            }
            return Err(cancelled_command(description));
        }
        if output_limit_reached.load(Ordering::Acquire) {
            let _ = tree.terminate();
            let status = tree.wait()?;
            let stdout = join_bounded_reader(stdout_reader, description, "stdout")?;
            let stderr = join_bounded_reader(stderr_reader, description, "stderr")?;
            return Ok(Output {
                status,
                stdout,
                stderr,
            });
        }
        if let Some(maximum) = limits.max_disk_bytes {
            let used = directory_bytes(root).saturating_sub(initial_disk_bytes.unwrap_or(0));
            if used > maximum {
                let _ = tree.terminate();
                let status = tree.wait()?;
                let stdout = join_bounded_reader(stdout_reader, description, "stdout")?;
                let stderr = join_bounded_reader(stderr_reader, description, "stderr")?;
                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
        }
        #[cfg(target_os = "macos")]
        if is_analysis_process {
            match tree.resident_memory_bytes() {
                Ok(bytes) if bytes > ANALYSIS_MEMORY_LIMIT_BYTES => {
                    let _ = tree.terminate();
                    let _ = tree.wait();
                    if let Some(tracker) = process_tracker.take() {
                        let _ = tracker.failed("analysis execution exceeded memory limit");
                    }
                    return Err(MedusaError::new(
                        ErrorCode::ToolExecutionFailed,
                        ErrorCategory::Execution,
                        format!(
                            "{description} exceeded the {ANALYSIS_MEMORY_LIMIT_BYTES} byte analysis memory limit"
                        ),
                    ));
                }
                Ok(_) => {}
                Err(error) => {
                    let _ = tree.terminate();
                    let _ = tree.wait();
                    if let Some(tracker) = process_tracker.take() {
                        let _ = tracker.failed("analysis memory accounting failed");
                    }
                    return Err(MedusaError::new(
                        ErrorCode::ToolExecutionFailed,
                        ErrorCategory::Execution,
                        format!("{description} memory accounting failed closed: {error}"),
                    ));
                }
            }
        }
        if let Some(status) = tree.try_wait()? {
            if let Some(tracker) = process_tracker.take() {
                tracker.exited(status.code())?;
            }
            let stdout = stdout_reader.join().map_err(|_| {
                MedusaError::new(
                    ErrorCode::ToolExecutionFailed,
                    ErrorCategory::Execution,
                    format!("{description} stdout reader terminated unexpectedly"),
                )
            })??;
            let stderr = stderr_reader.join().map_err(|_| {
                MedusaError::new(
                    ErrorCode::ToolExecutionFailed,
                    ErrorCategory::Execution,
                    format!("{description} stderr reader terminated unexpectedly"),
                )
            })??;
            return Ok(Output {
                status,
                stdout,
                stderr,
            });
        }
        if started.elapsed() >= limits.timeout {
            let _ = tree.terminate();
            let _ = tree.wait();
            if let Some(tracker) = process_tracker.take() {
                let _ = tracker.failed("analysis execution timed out");
            }
            return Err(MedusaError::new(
                ErrorCode::ToolExecutionFailed,
                ErrorCategory::Execution,
                format!(
                    "{description} timed out after {} seconds",
                    limits.timeout.as_secs()
                ),
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn directory_bytes(root: &Path) -> u64 {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .fold(0_u64, u64::saturating_add)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn spawn_bounded_reader<R>(
    mut pipe: R,
    limit: usize,
    used: Arc<AtomicUsize>,
    limit_reached: Arc<AtomicBool>,
) -> thread::JoinHandle<std::io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            if limit_reached.load(AtomicOrdering::Acquire) {
                break;
            }
            let current = used.load(AtomicOrdering::Acquire);
            if current >= limit {
                limit_reached.store(true, AtomicOrdering::Release);
                break;
            }
            let allowance = (limit - current).min(buffer.len());
            let read = pipe.read(&mut buffer[..allowance])?;
            if read == 0 {
                break;
            }
            let previous = used.fetch_add(read, AtomicOrdering::AcqRel);
            if previous.saturating_add(read) > limit {
                let keep = limit.saturating_sub(previous);
                bytes.extend_from_slice(&buffer[..keep.min(read)]);
                limit_reached.store(true, AtomicOrdering::Release);
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if previous.saturating_add(read) == limit {
                limit_reached.store(true, AtomicOrdering::Release);
            }
        }
        Ok(bytes)
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn join_bounded_reader(
    reader: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    description: &str,
    stream: &str,
) -> MedusaResult<Vec<u8>> {
    reader
        .join()
        .map_err(|_| {
            MedusaError::new(
                ErrorCode::ToolExecutionFailed,
                ErrorCategory::Execution,
                format!("{description} {stream} reader terminated unexpectedly"),
            )
        })?
        .map_err(|error| {
            MedusaError::new(
                ErrorCode::ToolExecutionFailed,
                ErrorCategory::Execution,
                format!("{description} {stream} read failed: {error}"),
            )
        })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn sandbox_unavailable(message: impl Into<String>) -> MedusaError {
    let mut error = MedusaError::new(
        ErrorCode::SandboxUnavailable,
        ErrorCategory::Environment,
        message,
    );
    error.context.insert(
        "sandbox_backend".into(),
        serde_json::Value::String("unavailable".into()),
    );
    error
        .context
        .insert("effective_restrictions".into(), serde_json::json!([]));
    error
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn cancelled_command(description: &str) -> MedusaError {
    let mut error = MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        format!("{description} cancelled"),
    );
    error
        .context
        .insert("cancelled".into(), serde_json::Value::Bool(true));
    error
}

fn policy_denied(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::PolicyDenied, ErrorCategory::Policy, message)
}

#[cfg(test)]
mod command_admission_tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn bounded_readers_never_retain_more_than_the_shared_limit() {
        let used = Arc::new(AtomicUsize::new(0));
        let reached = Arc::new(AtomicBool::new(false));
        let stdout = spawn_bounded_reader(
            std::io::Cursor::new(vec![b'x'; 4096]),
            1024,
            Arc::clone(&used),
            Arc::clone(&reached),
        );
        let stderr = spawn_bounded_reader(
            std::io::Cursor::new(vec![b'y'; 4096]),
            1024,
            Arc::clone(&used),
            Arc::clone(&reached),
        );
        let stdout = stdout.join().expect("stdout reader").expect("stdout read");
        let stderr = stderr.join().expect("stderr reader").expect("stderr read");
        assert!(reached.load(AtomicOrdering::Acquire));
        assert_eq!(stdout.len() + stderr.len(), 1024);
        assert!(used.load(AtomicOrdering::Acquire) >= 1024);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn contained_commands_are_terminated_at_the_output_limit() {
        let repository = tempfile::tempdir().expect("repository");
        let cancellation = AtomicBool::new(false);
        let output = sandboxed_command_cancellable_with_limits(
            repository.path(),
            "python3",
            &[
                "-c".to_owned(),
                "import sys, time; sys.stdout.write('x' * 4096); sys.stdout.flush(); time.sleep(2)"
                    .to_owned(),
            ],
            &cancellation,
            CommandLimits {
                max_output_bytes: 1024,
                ..CommandLimits::default()
            },
        )
        .expect("bounded command");
        assert!(!output.status.success());
        assert!(output.stdout.len() + output.stderr.len() <= 1024);
    }

    #[test]
    fn contained_language_toolchains_are_admitted_on_every_platform() {
        for program in [
            "python",
            "python.exe",
            "node",
            "node.exe",
            "ruby",
            "ruby.exe",
        ] {
            assert!(validate_shell_command(program, &[]).is_ok());
        }
    }

    #[test]
    fn shells_and_network_clients_remain_hard_denied() {
        for program in ["sh", "bash", "powershell.exe", "curl", "wget", "ssh"] {
            assert!(validate_shell_command(program, &[]).is_err());
        }
    }
}
