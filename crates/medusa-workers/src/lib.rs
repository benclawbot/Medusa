//! Parallel worker orchestration with Git worktrees and deterministic merge coordination.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Durable outcome of a delegated worker.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    Ready,
    Running,
    Succeeded,
    Failed,
}

/// Isolated worker checkout and branch metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Worker {
    pub id: String,
    pub branch: String,
    pub worktree: PathBuf,
    pub state: WorkerState,
    pub commit: Option<String>,
    pub stdout: String,
    pub stderr: String,
}

/// Command delegated to a worker worktree.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DelegatedTask {
    pub program: String,
    pub args: Vec<String>,
    pub commit_message: String,
}

/// Manages isolated branches and worktrees for one repository.
#[derive(Clone, Debug)]
pub struct WorkerManager {
    repo: PathBuf,
    worktree_root: PathBuf,
}

impl WorkerManager {
    pub fn new(repo: impl Into<PathBuf>, worktree_root: impl Into<PathBuf>) -> MedusaResult<Self> {
        let manager = Self {
            repo: repo.into(),
            worktree_root: worktree_root.into(),
        };
        fs::create_dir_all(&manager.worktree_root)?;
        Ok(manager)
    }

    /// Creates an isolated worktree from the current repository HEAD.
    pub fn create_worker(&self, label: &str) -> MedusaResult<Worker> {
        validate_label(label)?;
        let id = format!("wrk-{}", Ulid::new());
        let branch = format!("medusa/{label}-{id}");
        let worktree = self.worktree_root.join(&id);
        run_git(
            &self.repo,
            &["worktree", "add", "-b", &branch, path_text(&worktree)?, "HEAD"],
        )?;
        Ok(Worker {
            id,
            branch,
            worktree,
            state: WorkerState::Ready,
            commit: None,
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    /// Runs tasks concurrently in their isolated worktrees and commits successful changes.
    pub fn delegate_parallel(
        &self,
        assignments: Vec<(Worker, DelegatedTask)>,
    ) -> MedusaResult<Vec<Worker>> {
        let handles = assignments
            .into_iter()
            .map(|(worker, task)| thread::spawn(move || execute_worker(worker, task)))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| {
                handle.join().map_err(|_| {
                    MedusaError::new(
                        ErrorCode::InternalInvariant,
                        ErrorCategory::Internal,
                        "worker thread panicked",
                    )
                })?
            })
            .collect()
    }

    /// Cherry-picks successful worker commits in stable worker-ID order.
    pub fn merge_successful(&self, workers: &[Worker]) -> MedusaResult<Vec<String>> {
        ensure_clean(&self.repo)?;
        let mut ordered = workers
            .iter()
            .filter(|worker| worker.state == WorkerState::Succeeded)
            .collect::<Vec<_>>();
        ordered.sort_by(|left, right| left.id.cmp(&right.id));
        let mut merged = Vec::new();
        for worker in ordered {
            let commit = worker.commit.as_deref().ok_or_else(|| {
                MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Internal,
                    format!("successful worker {} has no commit", worker.id),
                )
            })?;
            if let Err(error) = run_git(&self.repo, &["cherry-pick", commit]) {
                let _ = run_git(&self.repo, &["cherry-pick", "--abort"]);
                return Err(error);
            }
            merged.push(commit.to_owned());
        }
        Ok(merged)
    }

    /// Runs combined repository verification after all worker commits merge.
    pub fn verify_combined(&self) -> MedusaResult<String> {
        let output = if self.repo.join("verify.sh").is_file() {
            Command::new("sh")
                .arg("verify.sh")
                .current_dir(&self.repo)
                .output()?
        } else {
            Command::new("cargo")
                .args(["test", "--workspace", "--all-features"])
                .current_dir(&self.repo)
                .output()?
        };
        output_result("combined verification", output)
    }

    /// Removes worktrees after their commits are merged or rejected.
    pub fn cleanup(&self, workers: &[Worker]) -> MedusaResult<()> {
        for worker in workers {
            if worker.worktree.exists() {
                run_git(
                    &self.repo,
                    &["worktree", "remove", "--force", path_text(&worker.worktree)?],
                )?;
            }
        }
        run_git(&self.repo, &["worktree", "prune"])
    }
}

fn execute_worker(mut worker: Worker, task: DelegatedTask) -> MedusaResult<Worker> {
    worker.state = WorkerState::Running;
    let output = Command::new(&task.program)
        .args(&task.args)
        .current_dir(&worker.worktree)
        .output()?;
    worker.stdout = bounded(&output.stdout);
    worker.stderr = bounded(&output.stderr);
    if !output.status.success() {
        worker.state = WorkerState::Failed;
        return Ok(worker);
    }
    run_git(&worker.worktree, &["add", "-A"])?;
    run_git(
        &worker.worktree,
        &["commit", "-m", &task.commit_message],
    )?;
    worker.commit = Some(git_stdout(&worker.worktree, &["rev-parse", "HEAD"])?);
    worker.state = WorkerState::Succeeded;
    Ok(worker)
}

fn ensure_clean(repo: &Path) -> MedusaResult<()> {
    if git_stdout(repo, &["status", "--porcelain"])?.is_empty() {
        Ok(())
    } else {
        Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            "merge coordinator requires a clean repository",
        ))
    }
}

fn run_git(repo: &Path, args: &[&str]) -> MedusaResult<()> {
    let output = Command::new("git").args(args).current_dir(repo).output()?;
    output_result(&format!("git {}", args.join(" ")), output).map(|_| ())
}

fn git_stdout(repo: &Path, args: &[&str]) -> MedusaResult<String> {
    let output = Command::new("git").args(args).current_dir(repo).output()?;
    output_result(&format!("git {}", args.join(" ")), output).map(|text| text.trim().to_owned())
}

fn output_result(label: &str, output: Output) -> MedusaResult<String> {
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!(
                "{label} failed with {}\nstdout={}\nstderr={}",
                output.status,
                bounded(&output.stdout),
                bounded(&output.stderr)
            ),
        ))
    }
}

fn bounded(bytes: &[u8]) -> String {
    const LIMIT: usize = 1_000_000;
    let mut value = String::from_utf8_lossy(bytes).into_owned();
    if value.len() > LIMIT {
        value.truncate(LIMIT);
        value.push_str("\n[truncated]");
    }
    value
}

fn path_text(path: &Path) -> MedusaResult<&str> {
    path.to_str().ok_or_else(|| {
        MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            format!("path is not valid UTF-8: {}", path.display()),
        )
    })
}

fn validate_label(label: &str) -> MedusaResult<()> {
    if !label.is_empty()
        && label
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        Ok(())
    } else {
        Err(MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            format!("invalid worker label: {label}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(repo: &Path, args: &[&str]) {
        run_git(repo, args).expect("git command");
    }

    #[test]
    fn parallel_feature_fixture_merges_and_verifies() {
        let directory = tempfile::tempdir().expect("tempdir");
        let repo = directory.path().join("repo");
        let worktrees = directory.path().join("worktrees");
        fs::create_dir(&repo).expect("repo");
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.name", "Medusa Test"]);
        git(&repo, &["config", "user.email", "medusa@example.invalid"]);
        fs::write(repo.join("base.txt"), "base\n").expect("base");
        fs::write(
            repo.join("verify.sh"),
            "#!/bin/sh\nset -eu\ntest \"$(cat feature-a.txt)\" = alpha\ntest \"$(cat feature-b.txt)\" = beta\necho combined-verification-ok\n",
        )
        .expect("verify");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "base"]);

        let manager = WorkerManager::new(&repo, &worktrees).expect("manager");
        let worker_a = manager.create_worker("feature-a").expect("worker a");
        let worker_b = manager.create_worker("feature-b").expect("worker b");
        let workers = manager
            .delegate_parallel(vec![
                (
                    worker_a,
                    DelegatedTask {
                        program: "sh".into(),
                        args: vec!["-c".into(), "printf alpha > feature-a.txt".into()],
                        commit_message: "add feature a".into(),
                    },
                ),
                (
                    worker_b,
                    DelegatedTask {
                        program: "sh".into(),
                        args: vec!["-c".into(), "printf beta > feature-b.txt".into()],
                        commit_message: "add feature b".into(),
                    },
                ),
            ])
            .expect("delegate");
        assert!(workers.iter().all(|worker| worker.state == WorkerState::Succeeded));
        assert_eq!(manager.merge_successful(&workers).expect("merge").len(), 2);
        assert!(
            manager
                .verify_combined()
                .expect("verify")
                .contains("combined-verification-ok")
        );
        assert_eq!(fs::read_to_string(repo.join("feature-a.txt")).expect("a"), "alpha");
        assert_eq!(fs::read_to_string(repo.join("feature-b.txt")).expect("b"), "beta");
        manager.cleanup(&workers).expect("cleanup");
    }
}
