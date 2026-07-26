use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::Output,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

#[cfg(any(target_os = "linux", target_os = "macos"))]
const SHELL_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

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
    validate_shell_command_hard_denials(program, args)?;
    #[cfg(not(target_os = "linux"))]
    validate_portable_shell_command(
        &Path::new(program)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(program)
            .to_ascii_lowercase(),
        args,
    )?;
    Ok(())
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
        "pwsh",
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

    if basename == "git" {
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

#[cfg(not(target_os = "linux"))]
fn validate_portable_shell_command(program: &str, args: &[String]) -> MedusaResult<()> {
    let first = args.first().map(String::as_str).unwrap_or_default();
    let allowed = match program {
        "cargo" => matches!(
            first,
            "build"
                | "check"
                | "clippy"
                | "fmt"
                | "metadata"
                | "test"
                | "tree"
                | "--version"
                | "version"
        ),
        "git" => matches!(
            first,
            "branch" | "diff" | "log" | "ls-files" | "rev-parse" | "show" | "status"
        ),
        "fd" | "find" | "ls" | "rg" | "tree" => true,
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(policy_denied(format!(
            "portable shell command is not approved: {program} {}",
            args.join(" ")
        )))
    }
}

pub(crate) fn sandboxed_command(
    repo: &Path,
    program: &str,
    args: &[String],
) -> MedusaResult<Output> {
    #[cfg(target_os = "linux")]
    {
        let root = repo.canonicalize()?;
        let mut command = Command::new("bwrap");
        command
            .args([
                "--die-with-parent",
                "--new-session",
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
            .arg("--")
            .arg(program)
            .args(args);
        output_with_timeout(&mut command, "Linux bubblewrap sandbox")
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
            .current_dir(&root);
        let result = output_with_timeout(&mut command, "macOS sandbox-exec sandbox");
        let _ = fs::remove_file(&profile_path);
        result
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
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
fn output_with_timeout(command: &mut Command, description: &str) -> MedusaResult<Output> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Environment,
                format!("{description} unavailable: {error}"),
            )
        })?;
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output().map_err(|error| {
                MedusaError::new(
                    ErrorCode::ToolExecutionFailed,
                    ErrorCategory::Execution,
                    format!("{description} failed while collecting output: {error}"),
                )
            });
        }
        if started.elapsed() >= SHELL_COMMAND_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
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

fn policy_denied(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::PolicyDenied, ErrorCategory::Policy, message)
}

#[cfg(all(test, not(any(target_os = "linux", target_os = "macos"))))]
mod tests {
    use super::*;

    #[test]
    fn sandboxed_execution_fails_closed_without_backend() {
        let error = sandboxed_command(Path::new("."), "cargo", &["--version".into()])
            .expect_err("unsupported platforms must not launch a bare process");
        assert_eq!(error.code, ErrorCode::SandboxUnavailable);
        assert_eq!(
            error.context.get("sandbox_backend"),
            Some(&serde_json::Value::String("unavailable".into()))
        );
    }
}
