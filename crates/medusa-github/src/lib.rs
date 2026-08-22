//! First-class GitHub runtime capability backed by GitHub CLI and Git.
//!
//! Authentication is delegated to `gh`, which supports device/browser sign-in,
//! GitHub Enterprise hostnames, and the platform credential store. Every
//! repository, pull-request, issue, and Actions operation is built here rather
//! than being assembled ad hoc by frontends or agents.

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    thread::{self, JoinHandle},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, hidden_command};
use serde::{Deserialize, Serialize};

mod repository_creation;
pub use repository_creation::*;

/// Captured result of an external GitHub or Git command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Execution boundary that makes service command construction testable.
pub trait CommandExecutor {
    fn run(
        &self,
        program: &str,
        arguments: &[String],
        directory: Option<&Path>,
    ) -> MedusaResult<CommandOutput>;

    /// Executes while retaining at most one byte beyond each configured limit.
    ///
    /// The default keeps fake and embedded executors source-compatible. The
    /// production executor overrides this method to drain both pipes while
    /// bounding retained memory before the child exits.
    fn run_bounded(
        &self,
        program: &str,
        arguments: &[String],
        directory: Option<&Path>,
        stdout_limit: usize,
        stderr_limit: usize,
    ) -> MedusaResult<CommandOutput> {
        let mut output = self.run(program, arguments, directory)?;
        output.stdout = retain_text_prefix(&output.stdout, stdout_limit.saturating_add(1));
        output.stderr = retain_text_prefix(&output.stderr, stderr_limit.saturating_add(1));
        Ok(output)
    }
}

/// Production command executor. Arguments are never passed through a shell.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemExecutor;

impl CommandExecutor for SystemExecutor {
    fn run(
        &self,
        program: &str,
        arguments: &[String],
        directory: Option<&Path>,
    ) -> MedusaResult<CommandOutput> {
        let mut command = hidden_command(program);
        command.args(arguments);
        if let Some(directory) = directory {
            command.current_dir(directory);
        }
        let output = command.output().map_err(command_error)?;
        Ok(CommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }

    fn run_bounded(
        &self,
        program: &str,
        arguments: &[String],
        directory: Option<&Path>,
        stdout_limit: usize,
        stderr_limit: usize,
    ) -> MedusaResult<CommandOutput> {
        let mut command = hidden_command(program);
        command
            .args(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(directory) = directory {
            command.current_dir(directory);
        }
        let mut child = command.spawn().map_err(command_error)?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| internal_error("bounded command stdout pipe was unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| internal_error("bounded command stderr pipe was unavailable"))?;
        let stdout_reader = spawn_bounded_reader(stdout, stdout_limit.saturating_add(1));
        let stderr_reader = spawn_bounded_reader(stderr, stderr_limit.saturating_add(1));
        let status = child.wait().map_err(command_error)?;
        let stdout = join_bounded_reader(stdout_reader)?;
        let stderr = join_bounded_reader(stderr_reader)?;
        Ok(CommandOutput {
            success: status.success(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }
}

/// Pull request merge strategy supported by GitHub.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    Merge,
    Squash,
    Rebase,
}

/// Credentials are stored by GitHub CLI in its secure OS-keychain backend where available.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AuthStatus {
    pub hostname: String,
    pub authenticated: bool,
    pub credential_backend: &'static str,
}

/// Typed GitHub service that owns authentication and all GitHub lifecycle operations.
#[derive(Clone, Debug)]
pub struct GitHubService<E = SystemExecutor> {
    executor: E,
    repository: String,
    hostname: String,
    directory: Option<PathBuf>,
}

impl GitHubService<SystemExecutor> {
    #[must_use]
    pub fn new(repository: impl Into<String>) -> Self {
        Self::with_executor(repository, "github.com", None, SystemExecutor)
    }
}

impl<E: CommandExecutor> GitHubService<E> {
    #[must_use]
    pub fn enterprise(
        repository: impl Into<String>,
        hostname: impl Into<String>,
        directory: Option<PathBuf>,
        executor: E,
    ) -> Self {
        Self::with_executor(repository, hostname, directory, executor)
    }

    #[must_use]
    pub fn with_executor(
        repository: impl Into<String>,
        hostname: impl Into<String>,
        directory: Option<PathBuf>,
        executor: E,
    ) -> Self {
        Self {
            executor,
            repository: repository.into(),
            hostname: hostname.into(),
            directory,
        }
    }

    /// Opens GitHub's device/browser authorization flow. `gh` persists the result in its secure credential store.
    pub fn authenticate_device_flow(&self) -> MedusaResult<AuthStatus> {
        self.gh([
            "auth",
            "login",
            "--hostname",
            &self.hostname,
            "--web",
            "--git-protocol",
            "https",
        ])?;
        self.auth_status()
    }

    /// Explicit browser OAuth alias for desktop frontends.
    pub fn authenticate_browser_oauth(&self) -> MedusaResult<AuthStatus> {
        self.authenticate_device_flow()
    }

    pub fn auth_status(&self) -> MedusaResult<AuthStatus> {
        let output = self.gh_status(["auth", "status", "--hostname", &self.hostname])?;
        Ok(AuthStatus {
            hostname: self.hostname.clone(),
            authenticated: output.success,
            credential_backend: "gh secure credential store",
        })
    }

    pub fn clone(&self, destination: &Path) -> MedusaResult<String> {
        self.git_in(
            None,
            [
                "clone",
                &self.clone_url(),
                &destination.display().to_string(),
            ],
        )
    }

    pub fn fetch(&self) -> MedusaResult<String> {
        self.git(["fetch", "--prune", "origin"])
    }
    pub fn pull(&self) -> MedusaResult<String> {
        self.git(["pull", "--ff-only"])
    }
    pub fn push(&self) -> MedusaResult<String> {
        self.git(["push"])
    }
    pub fn checkout(&self, reference: &str) -> MedusaResult<String> {
        self.git(["checkout", reference])
    }
    pub fn branches(&self) -> MedusaResult<String> {
        self.git(["branch", "--all", "--no-color"])
    }
    pub fn tags(&self) -> MedusaResult<String> {
        self.git(["tag", "--list"])
    }

    pub fn create_pr(
        &self,
        title: &str,
        body: &str,
        base: &str,
        head: Option<&str>,
    ) -> MedusaResult<String> {
        let mut args = strings([
            "pr",
            "create",
            "--repo",
            &self.repository,
            "--title",
            title,
            "--body",
            body,
            "--base",
            base,
        ]);
        if let Some(head) = head {
            args.extend(strings(["--head", head]));
        }
        self.run("gh", args, self.directory.as_deref())
    }

    pub fn update_pr(
        &self,
        number: u64,
        title: Option<&str>,
        body: Option<&str>,
    ) -> MedusaResult<String> {
        let mut args = strings([
            "pr",
            "edit",
            &number.to_string(),
            "--repo",
            &self.repository,
        ]);
        if let Some(title) = title {
            args.extend(strings(["--title", title]));
        }
        if let Some(body) = body {
            args.extend(strings(["--body", body]));
        }
        self.run("gh", args, self.directory.as_deref())
    }

    pub fn review_pr(&self, number: u64, body: &str, approve: bool) -> MedusaResult<String> {
        let event = if approve { "--approve" } else { "--comment" };
        self.gh([
            "pr",
            "review",
            &number.to_string(),
            "--repo",
            &self.repository,
            event,
            "--body",
            body,
        ])
    }

    pub fn merge_pr(&self, number: u64, strategy: MergeStrategy) -> MedusaResult<String> {
        let strategy = match strategy {
            MergeStrategy::Merge => "--merge",
            MergeStrategy::Squash => "--squash",
            MergeStrategy::Rebase => "--rebase",
        };
        self.gh([
            "pr",
            "merge",
            &number.to_string(),
            "--repo",
            &self.repository,
            strategy,
            "--delete-branch",
        ])
    }

    pub fn close_pr(&self, number: u64) -> MedusaResult<String> {
        self.gh([
            "pr",
            "close",
            &number.to_string(),
            "--repo",
            &self.repository,
            "--delete-branch",
        ])
    }

    pub fn create_issue(&self, title: &str, body: &str) -> MedusaResult<String> {
        self.gh([
            "issue",
            "create",
            "--repo",
            &self.repository,
            "--title",
            title,
            "--body",
            body,
        ])
    }

    pub fn comment_issue(&self, number: u64, body: &str) -> MedusaResult<String> {
        self.gh([
            "issue",
            "comment",
            &number.to_string(),
            "--repo",
            &self.repository,
            "--body",
            body,
        ])
    }

    pub fn assign_issue(&self, number: u64, assignee: &str) -> MedusaResult<String> {
        self.gh([
            "issue",
            "edit",
            &number.to_string(),
            "--repo",
            &self.repository,
            "--add-assignee",
            assignee,
        ])
    }

    pub fn label_issue(&self, number: u64, label: &str) -> MedusaResult<String> {
        self.gh([
            "issue",
            "edit",
            &number.to_string(),
            "--repo",
            &self.repository,
            "--add-label",
            label,
        ])
    }

    pub fn milestone_issue(&self, number: u64, milestone: &str) -> MedusaResult<String> {
        self.gh([
            "issue",
            "edit",
            &number.to_string(),
            "--repo",
            &self.repository,
            "--milestone",
            milestone,
        ])
    }

    pub fn watch_workflow(&self, run_id: u64) -> MedusaResult<String> {
        self.gh([
            "run",
            "watch",
            &run_id.to_string(),
            "--repo",
            &self.repository,
            "--exit-status",
        ])
    }

    pub fn download_workflow_logs(&self, run_id: u64) -> MedusaResult<String> {
        self.gh([
            "run",
            "view",
            &run_id.to_string(),
            "--repo",
            &self.repository,
            "--log",
        ])
    }

    pub fn rerun_failed_jobs(&self, run_id: u64) -> MedusaResult<String> {
        self.gh([
            "run",
            "rerun",
            &run_id.to_string(),
            "--repo",
            &self.repository,
            "--failed",
        ])
    }

    pub fn cancel_workflow(&self, run_id: u64) -> MedusaResult<String> {
        self.gh([
            "run",
            "cancel",
            &run_id.to_string(),
            "--repo",
            &self.repository,
        ])
    }

    fn clone_url(&self) -> String {
        self.clone_url_for(&self.repository)
    }

    fn gh<const N: usize>(&self, arguments: [&str; N]) -> MedusaResult<String> {
        self.run("gh", strings(arguments), self.directory.as_deref())
    }

    fn gh_status<const N: usize>(&self, arguments: [&str; N]) -> MedusaResult<CommandOutput> {
        self.executor
            .run("gh", &strings(arguments), self.directory.as_deref())
    }

    fn git<const N: usize>(&self, arguments: [&str; N]) -> MedusaResult<String> {
        self.git_in(self.directory.as_deref(), arguments)
    }

    fn git_in<const N: usize>(
        &self,
        directory: Option<&Path>,
        arguments: [&str; N],
    ) -> MedusaResult<String> {
        self.run("git", strings(arguments), directory)
    }

    fn run(
        &self,
        program: &str,
        arguments: Vec<String>,
        directory: Option<&Path>,
    ) -> MedusaResult<String> {
        let output = self.executor.run(program, &arguments, directory)?;
        if output.success {
            Ok(output.stdout)
        } else {
            Err(execution_error(
                &format!("{program} {}", arguments.join(" ")),
                repository_creation::sanitize_external_error(&output.stderr),
            ))
        }
    }
}

fn strings<const N: usize>(arguments: [&str; N]) -> Vec<String> {
    arguments.into_iter().map(str::to_owned).collect()
}

fn retain_text_prefix(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].to_owned()
}

fn spawn_bounded_reader<R>(reader: R, retain_limit: usize) -> JoinHandle<std::io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || read_and_drain(reader, retain_limit))
}

fn read_and_drain<R: Read>(mut reader: R, retain_limit: usize) -> std::io::Result<Vec<u8>> {
    let mut retained = Vec::with_capacity(retain_limit.min(8_192));
    let mut buffer = [0_u8; 8_192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = retain_limit.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    Ok(retained)
}

fn join_bounded_reader(reader: JoinHandle<std::io::Result<Vec<u8>>>) -> MedusaResult<Vec<u8>> {
    reader
        .join()
        .map_err(|_| internal_error("bounded command output reader panicked"))?
        .map_err(command_error)
}

fn invalid_input(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidInput, ErrorCategory::Validation, message)
}

fn policy_denied(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::PolicyDenied, ErrorCategory::Policy, message)
}

fn execution_error(operation: &str, detail: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        format!("{operation} failed: {}", detail.into()),
    )
}

fn partial_failure(web_url: &str, cause: MedusaError) -> MedusaError {
    MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        format!(
            "remote repository {web_url} was created or reused, but local bootstrap failed: {}; retry with reuse_existing=true after correcting the local problem",
            cause.message
        ),
    )
    .with_retryable(true)
}

fn internal_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        message,
    )
}

fn environment_error(error: std::io::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::PersistenceFailed,
        ErrorCategory::Environment,
        error.to_string(),
    )
}

fn command_error(error: std::io::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        error.to_string(),
    )
}
