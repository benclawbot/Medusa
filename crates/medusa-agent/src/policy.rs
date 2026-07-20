use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

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
    #[cfg(not(target_os = "linux"))]
    {
        let root = repo.canonicalize()?;
        #[cfg(windows)]
        if program.eq_ignore_ascii_case("ls") {
            let mut command = Command::new("cmd");
            command.args(["/C", "dir"]).current_dir(root);
            return output_with_timeout(&mut command, "Windows directory listing");
        }
        let mut command = Command::new(program);
        command.args(args).current_dir(root);
        output_with_timeout(&mut command, "local shell command")
    }
}

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

fn policy_denied(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::PolicyDenied, ErrorCategory::Policy, message)
}
