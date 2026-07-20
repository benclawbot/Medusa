//! First-class GitHub runtime capability backed by GitHub CLI and Git.
//!
//! Authentication is delegated to `gh`, which supports device/browser sign-in,
//! GitHub Enterprise hostnames, and the platform credential store. Every
//! repository, pull-request, issue, and Actions operation is built here rather
//! than being assembled ad hoc by frontends or agents.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};

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
        let mut command = Command::new(program);
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
        format!("https://{}/{}.git", self.hostname, self.repository)
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
            Err(MedusaError::new(
                ErrorCode::ToolExecutionFailed,
                ErrorCategory::Execution,
                format!(
                    "{program} {} failed: {}",
                    arguments.join(" "),
                    output.stderr
                ),
            ))
        }
    }
}

fn strings<const N: usize>(arguments: [&str; N]) -> Vec<String> {
    arguments.into_iter().map(str::to_owned).collect()
}

fn command_error(error: std::io::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default, Debug)]
    struct FakeExecutor(Arc<Mutex<Vec<(String, Vec<String>)>>>);
    impl CommandExecutor for FakeExecutor {
        fn run(
            &self,
            program: &str,
            arguments: &[String],
            _: Option<&Path>,
        ) -> MedusaResult<CommandOutput> {
            self.0
                .lock()
                .expect("lock")
                .push((program.into(), arguments.into()));
            Ok(CommandOutput {
                success: true,
                stdout: "ok".into(),
                stderr: String::new(),
            })
        }
    }
    fn service(fake: FakeExecutor) -> GitHubService<FakeExecutor> {
        GitHubService::enterprise("acme/medusa", "github.example", None, fake)
    }

    #[test]
    fn device_flow_targets_enterprise_host_and_secure_store() {
        let fake = FakeExecutor::default();
        let status = service(fake.clone())
            .authenticate_device_flow()
            .expect("login");
        assert!(status.authenticated);
        assert_eq!(status.hostname, "github.example");
        let calls = fake.0.lock().expect("lock");
        assert_eq!(calls[0].0, "gh");
        assert!(
            calls[0]
                .1
                .windows(2)
                .any(|pair| pair == ["--hostname", "github.example"])
        );
        assert!(calls[0].1.contains(&"--web".into()));
    }

    #[test]
    fn pull_request_and_actions_lifecycle_use_typed_commands() {
        let fake = FakeExecutor::default();
        let github = service(fake.clone());
        github.merge_pr(42, MergeStrategy::Squash).expect("merge");
        github.rerun_failed_jobs(99).expect("rerun");
        github.cancel_workflow(99).expect("cancel");
        let calls = fake.0.lock().expect("lock");
        assert!(calls[0].1.contains(&"--squash".into()));
        assert!(calls[0].1.contains(&"--delete-branch".into()));
        assert!(calls[1].1.contains(&"--failed".into()));
        assert_eq!(calls[2].1[1], "cancel");
    }

    #[test]
    fn repository_clone_uses_enterprise_url_without_shell_interpolation() {
        let fake = FakeExecutor::default();
        service(fake.clone())
            .clone(Path::new("checkout"))
            .expect("clone");
        let calls = fake.0.lock().expect("lock");
        assert_eq!(calls[0].0, "git");
        assert_eq!(calls[0].1[1], "https://github.example/acme/medusa.git");
    }
}
