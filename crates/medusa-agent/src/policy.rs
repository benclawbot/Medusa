use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::Output,
    sync::atomic::AtomicBool,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::sync::atomic::Ordering;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use medusa_process_containment::OwnedProcessTree;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[path = "analysis_process_tracker.rs"]
mod analysis_process_tracker;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use analysis_process_tracker::AnalysisProcessTracker;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

#[cfg(windows)]
#[path = "windows_sandbox.rs"]
mod windows_sandbox;

#[cfg(any(target_os = "linux", target_os = "macos"))]
const SHELL_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
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
    let basename = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
        .to_ascii_lowercase();
    const DENIED_PROGRAMS: &[&str] = &[
        "rm",
        "sudo",
        "doas",
        "shutdown",
        "reboot",
        "halt",
        "poweroff",
        "mkfs",
        "dd",
        "mount",
        "umount",
        "chown",
        "chmod",
        "kill",
        "pkill",
        "killall",
        "systemctl",
        "launchctl",
        "reg",
        "reg.exe",
        "sc",
        "sc.exe",
        "netsh",
        "curl",
        "wget",
        "nc",
        "ncat",
        "socat",
        "ssh",
        "scp",
        "sftp",
        "rsync",
        "env",
        "printenv",
        "set",
        "bash",
        "sh",
        "zsh",
        "fish",
        "cmd",
        "cmd.exe",
        "powershell",
        "powershell.exe",
        "pwsh",
        "pwsh.exe",
    ];
    if DENIED_PROGRAMS.contains(&basename.as_str()) {
        return Err(policy_denied(format!("hard-denied command: {program}")));
    }

    let normalized = args.join(" ").to_ascii_lowercase();
    const DENIED_FRAGMENTS: &[&str] = &[
        "curl | sh",
        "curl|sh",
        "wget | sh",
        "wget|sh",
        "/etc/shadow",
        "/etc/passwd",
        ".ssh/",
        "id_rsa",
        "id_ed25519",
        "authorization:",
        "api_key",
        "api-key",
        "secret_access_key",
        "disable-defender",
        "set-mppreference",
        "tamper protection",
        "endpoint protection",
        "--no-verify",
        "--force-with-lease",
        "--force",
        " -f ",
    ];
    if DENIED_FRAGMENTS
        .iter()
        .any(|fragment| normalized.contains(fragment))
    {
        return Err(policy_denied(format!(
            "hard-denied command arguments: {program}"
        )));
    }

    let normalized_program = basename
        .strip_suffix(".exe")
        .or_else(|| basename.strip_suffix(".com"))
        .or_else(|| basename.strip_suffix(".cmd"))
        .or_else(|| basename.strip_suffix(".bat"))
        .unwrap_or(&basename);
    if normalized_program == "git" {
        let first = args.first().map(String::as_str).unwrap_or_default();
        if matches!(first, "push" | "clean" | "reset" | "reflog" | "gc")
            || (first == "config"
                && args
                    .iter()
                    .any(|arg| arg == "--global" || arg == "--system"))
            || args
                .iter()
                .any(|arg| arg == "--force" || arg == "--force-with-lease")
        {
            return Err(policy_denied(format!(
                "denied Git mutation: git {}",
                args.join(" ")
            )));
        }
    }
    Ok(())
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
    #[cfg(target_os = "linux")]
    {
        let root = repo.canonicalize()?;
        let mut command = Command::new("bwrap");
        command
            .args([
                "--die-with-parent",
                "--new-session",
                // Enter a subordinate user namespace before creating the network namespace. This
                // gives bubblewrap only the namespace-local capabilities required to configure
                // loopback while retaining the fail-closed no-network boundary on restricted CI
                // hosts and unprivileged installations.
                "--unshare-user",
                "--uid",
                "0",
                "--gid",
                "0",
                "--unshare-net",
                "--ro-bind",
                "/",
                "/",
                "--bind",
            ])
            .arg(&root)
            .arg(&root)
            .arg("--chdir")
            .arg(&root)
            .args(["--tmpfs", "/tmp", "--clearenv", "--setenv", "PATH"])
            .arg(std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()))
            .args(["--setenv", "PYTHONDONTWRITEBYTECODE", "1"])
            .arg("--")
            .arg(program)
            .args(args);
        output_with_timeout(
            &mut command,
            "Linux bubblewrap sandbox",
            cancellation,
            &root,
            program,
            args,
        )
    }
    #[cfg(target_os = "macos")]
    {
        let root = repo.canonicalize()?;
        let profile = format!(
            "(version 1)\n(deny default)\n(allow process-exec* process-fork)\n\
             (allow file-read* (subpath \"/\"))\n\
             (allow file-write* (subpath \"{}\") (subpath \"/tmp\") (subpath \"/private/tmp\"))\n\
             (deny network*)\n",
            sandbox_profile_string(&root)
        );
        let profile_path = std::env::temp_dir().join(format!(
            "medusa-sandbox-{}-{}.sb",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        fs::write(&profile_path, profile)?;
        let mut command = Command::new("sandbox-exec");
        command
            .arg("-f")
            .arg(&profile_path)
            .arg(program)
            .args(args)
            .current_dir(&root)
            .env("PYTHONDONTWRITEBYTECODE", "1");
        let result = output_with_timeout(
            &mut command,
            "macOS sandbox-exec sandbox",
            cancellation,
            &root,
            program,
            args,
        );
        let _ = fs::remove_file(&profile_path);
        result
    }
    #[cfg(windows)]
    {
        windows_sandbox::run_cancellable(repo, program, args, cancellation)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (repo, program, args);
        Err(sandbox_unavailable(
            "no containment backend is available for this platform",
        ))
    }
}

#[cfg(target_os = "macos")]
fn sandbox_profile_string(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn output_with_timeout(
    command: &mut Command,
    description: &str,
    cancellation: &AtomicBool,
    root: &Path,
    program: &str,
    args: &[String],
) -> MedusaResult<Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut tree = OwnedProcessTree::spawn(command).map_err(|error| {
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
    let stdout_reader = thread::spawn(move || {
        let mut pipe = stdout;
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = thread::spawn(move || {
        let mut pipe = stderr;
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).map(|_| bytes)
    });
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
        if started.elapsed() >= SHELL_COMMAND_TIMEOUT {
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
                    SHELL_COMMAND_TIMEOUT.as_secs()
                ),
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
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
