//! Parallel worker orchestration with Git worktrees and deterministic merge coordination.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_evidence::{ChangeKind, ChangedComponent, normalize_components};
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

/// Durable evidence for one accepted worktree commit.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IntegrationReceipt {
    pub worker_id: String,
    pub branch: String,
    pub commit: String,
    pub base_head: String,
    pub integrated_head: String,
    pub changed_paths: Vec<String>,
    pub changed_components: Vec<ChangedComponent>,
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

    /// Returns the primary repository managed by this coordinator.
    #[must_use]
    pub fn repository_path(&self) -> &Path {
        &self.repo
    }

    /// Returns the primary repository HEAD used as a worktree integration boundary.
    pub fn repository_head(&self) -> MedusaResult<String> {
        git_stdout(&self.repo, &["rev-parse", "HEAD"])
    }

    /// Fails closed unless the primary repository has no tracked or untracked edits.
    pub fn require_clean(&self) -> MedusaResult<()> {
        ensure_clean(&self.repo)
    }

    /// Creates an isolated worktree from the current repository HEAD.
    pub fn create_worker(&self, label: &str) -> MedusaResult<Worker> {
        self.create_worker_with_id(label, &format!("wrk-{}", Ulid::new()))
    }

    /// Opens a crash-surviving worktree when it still matches the primary HEAD, or creates it.
    ///
    /// This closes the restart window between `git worktree add` and the coordinator's first
    /// durable state write without silently rebasing partial worker changes onto a newer HEAD.
    pub fn open_or_create_worker(&self, label: &str, worker_id: &str) -> MedusaResult<Worker> {
        validate_label(label)?;
        validate_worker_id(worker_id)?;
        fs::create_dir_all(&self.worktree_root)?;
        let branch = format!("medusa/{label}-{worker_id}");
        let worktree = self.worktree_root.join(worker_id);
        let branch_present = branch_exists(&self.repo, &branch)?;
        match (worktree.is_dir(), branch_present) {
            (false, false) => self.create_worker_with_id(label, worker_id),
            (true, true) => {
                let actual_branch =
                    git_stdout(&worktree, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
                if actual_branch != branch {
                    return Err(MedusaError::new(
                        ErrorCode::PolicyDenied,
                        ErrorCategory::Policy,
                        format!(
                            "existing worker worktree is on {actual_branch}, expected {branch}"
                        ),
                    ));
                }
                let worktree_head = git_stdout(&worktree, &["rev-parse", "HEAD"])?;
                let repository_head = self.repository_head()?;
                if worktree_head != repository_head {
                    return Err(MedusaError::new(
                        ErrorCode::PolicyDenied,
                        ErrorCategory::Policy,
                        format!(
                            "existing worker base {worktree_head} does not match primary HEAD {repository_head}"
                        ),
                    ));
                }
                Ok(Worker {
                    id: worker_id.to_owned(),
                    branch,
                    worktree,
                    state: WorkerState::Ready,
                    commit: None,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
            _ => Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                format!("worker branch/worktree resources are inconsistent for {worker_id}"),
            )),
        }
    }

    /// Creates a deterministic worker identity for durable restart and retry flows.
    pub fn create_worker_with_id(&self, label: &str, worker_id: &str) -> MedusaResult<Worker> {
        validate_label(label)?;
        validate_worker_id(worker_id)?;
        fs::create_dir_all(&self.worktree_root)?;
        let branch = format!("medusa/{label}-{worker_id}");
        let worktree = self.worktree_root.join(worker_id);
        if worktree.exists() || branch_exists(&self.repo, &branch)? {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                format!("worker resources already exist for {worker_id}"),
            ));
        }
        run_git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                path_text(&worktree)?,
                "HEAD",
            ],
        )?;
        Ok(Worker {
            id: worker_id.to_owned(),
            branch,
            worktree,
            state: WorkerState::Ready,
            commit: None,
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    /// Returns exact tracked and untracked components changed relative to the worker base commit.
    pub fn changed_components_since(
        &self,
        worker: &Worker,
        base_commit: &str,
    ) -> MedusaResult<Vec<ChangedComponent>> {
        if base_commit.trim().is_empty() {
            return Err(invalid("worker base commit cannot be empty"));
        }
        let mut components = git_changed_components(
            &worker.worktree,
            &["diff", "--name-status", "-M", "-C", "-z", base_commit, "--"],
        )?;
        for path in git_nul_paths(
            &worker.worktree,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )? {
            components.push(
                ChangedComponent::new(ChangeKind::Added, path)
                    .map_err(|error| invalid(error.to_string()))?,
            );
        }
        if components.is_empty() {
            return Ok(Vec::new());
        }
        normalize_components(&worker.worktree, &components)
            .map_err(|error| invalid(error.to_string()))
    }

    /// Compatibility projection of exact changed-component scope.
    pub fn changed_paths_since(
        &self,
        worker: &Worker,
        base_commit: &str,
    ) -> MedusaResult<Vec<String>> {
        self.changed_components_since(worker, base_commit)
            .map(|components| changed_component_paths(&components))
    }

    /// Squashes all worktree edits since `base_commit` into one deterministic worker commit.
    pub fn finalize_worker(
        &self,
        mut worker: Worker,
        base_commit: &str,
        commit_message: &str,
    ) -> MedusaResult<Worker> {
        if commit_message.trim().is_empty() {
            return Err(invalid("worker commit message cannot be empty"));
        }
        if !worker.worktree.is_dir() {
            return Err(invalid(format!(
                "worker worktree does not exist: {}",
                worker.worktree.display()
            )));
        }
        let changed_paths = self.changed_paths_since(&worker, base_commit)?;
        if changed_paths.is_empty() {
            return Err(MedusaError::new(
                ErrorCode::ToolExecutionFailed,
                ErrorCategory::Execution,
                format!("worker {} completed without repository changes", worker.id),
            ));
        }
        if !git_success(
            &worker.worktree,
            &["merge-base", "--is-ancestor", base_commit, "HEAD"],
        )? {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                "worker branch no longer descends from its execution base",
            ));
        }
        run_git(&worker.worktree, &["reset", "--soft", base_commit])?;
        run_git(&worker.worktree, &["add", "-A"])?;
        run_git(&worker.worktree, &["diff", "--cached", "--check"])?;
        run_git(
            &worker.worktree,
            &[
                "-c",
                "user.name=Medusa",
                "-c",
                "user.email=medusa@users.noreply.github.com",
                "commit",
                "-m",
                commit_message,
            ],
        )?;
        worker.commit = Some(git_stdout(&worker.worktree, &["rev-parse", "HEAD"])?);
        worker.state = WorkerState::Succeeded;
        Ok(worker)
    }

    /// Runs command tasks concurrently in isolated worktrees and commits successful changes.
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
    ///
    /// Overlapping changed paths are rejected before integration. If any cherry-pick fails, every
    /// commit from the batch is rolled back to the clean pre-integration HEAD.
    pub fn integrate_successful(
        &self,
        workers: &[Worker],
    ) -> MedusaResult<Vec<IntegrationReceipt>> {
        ensure_clean(&self.repo)?;
        let mut ordered = workers
            .iter()
            .filter(|worker| worker.state == WorkerState::Succeeded)
            .collect::<Vec<_>>();
        ordered.sort_by(|left, right| left.id.cmp(&right.id));

        let mut path_owners = BTreeMap::<String, String>::new();
        let mut prepared = Vec::with_capacity(ordered.len());
        for worker in ordered {
            let commit = worker.commit.as_deref().ok_or_else(|| {
                MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Internal,
                    format!("successful worker {} has no commit", worker.id),
                )
            })?;
            let components = changed_components_for_commit(&self.repo, commit)?;
            let paths = changed_component_paths(&components);
            if paths.is_empty() {
                return Err(invalid(format!(
                    "worker {} commit contains no changed paths",
                    worker.id
                )));
            }
            for path in &paths {
                if let Some(owner) = path_owners.insert(path.clone(), worker.id.clone()) {
                    return Err(MedusaError::new(
                        ErrorCode::PolicyDenied,
                        ErrorCategory::Policy,
                        format!(
                            "worker path overlap rejected before integration: {path} ({owner}, {})",
                            worker.id
                        ),
                    ));
                }
            }
            prepared.push((worker, commit.to_owned(), paths, components));
        }

        let base_head = self.repository_head()?;
        let mut receipts = Vec::with_capacity(prepared.len());
        for (worker, commit, changed_paths, changed_components) in prepared {
            if let Err(error) = run_git(&self.repo, &["cherry-pick", &commit]) {
                let _ = run_git(&self.repo, &["cherry-pick", "--abort"]);
                let rollback = run_git(&self.repo, &["reset", "--hard", &base_head]);
                return match rollback {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(MedusaError::new(
                        ErrorCode::InternalInvariant,
                        ErrorCategory::Internal,
                        format!(
                            "integration failed and rollback also failed: {error}; rollback={rollback_error}"
                        ),
                    )),
                };
            }
            receipts.push(IntegrationReceipt {
                worker_id: worker.id.clone(),
                branch: worker.branch.clone(),
                commit,
                base_head: base_head.clone(),
                integrated_head: self.repository_head()?,
                changed_paths,
                changed_components,
            });
        }
        Ok(receipts)
    }

    /// Compatibility wrapper returning only accepted commit identifiers.
    pub fn merge_successful(&self, workers: &[Worker]) -> MedusaResult<Vec<String>> {
        self.integrate_successful(workers)
            .map(|receipts| receipts.into_iter().map(|receipt| receipt.commit).collect())
    }

    /// Returns whether a prepared worker commit is already integrated into the primary HEAD.
    pub fn commit_is_integrated(&self, commit: &str) -> MedusaResult<bool> {
        if commit.trim().is_empty() {
            return Err(invalid("worker commit cannot be empty"));
        }
        git_success(&self.repo, &["merge-base", "--is-ancestor", commit, "HEAD"])
    }

    /// Returns whether the prepared commit tree is the current primary repository tree.
    ///
    /// Cherry-pick creates a new commit identifier, so restart recovery cannot rely only on
    /// ancestry to detect that an isolated worker was integrated before state persistence.
    pub fn commit_tree_matches_head(&self, commit: &str) -> MedusaResult<bool> {
        if commit.trim().is_empty() {
            return Err(invalid("worker commit cannot be empty"));
        }
        let commit_tree = git_stdout(&self.repo, &["rev-parse", &format!("{commit}^{{tree}}")])?;
        let head_tree = git_stdout(&self.repo, &["rev-parse", "HEAD^{tree}"])?;
        Ok(commit_tree == head_tree)
    }

    /// Returns the immutable tree identifier for a prepared commit.
    pub fn commit_tree(&self, commit: &str) -> MedusaResult<String> {
        if commit.trim().is_empty() {
            return Err(invalid("worker commit cannot be empty"));
        }
        git_stdout(&self.repo, &["rev-parse", &format!("{commit}^{{tree}}")])
    }

    /// Returns the full binary-safe patch reviewed for a prepared commit.
    pub fn commit_patch(&self, base_commit: &str, commit: &str) -> MedusaResult<String> {
        if base_commit.trim().is_empty() || commit.trim().is_empty() {
            return Err(invalid("commit patch requires base and prepared commit"));
        }
        git_stdout(
            &self.repo,
            &[
                "diff",
                "--binary",
                "--full-index",
                base_commit,
                commit,
                "--",
            ],
        )
    }

    /// Returns the exact changed components encoded by a prepared commit.
    pub fn commit_changed_components(&self, commit: &str) -> MedusaResult<Vec<ChangedComponent>> {
        if commit.trim().is_empty() {
            return Err(invalid("worker commit cannot be empty"));
        }
        changed_components_for_commit(&self.repo, commit)
    }

    /// Compatibility projection of exact prepared-commit scope.
    pub fn commit_changed_paths(&self, commit: &str) -> MedusaResult<Vec<String>> {
        self.commit_changed_components(commit)
            .map(|components| changed_component_paths(&components))
    }

    /// Materializes an immutable prepared commit in a detached verification worktree.
    pub fn materialize_detached_commit(&self, commit: &str, path: &Path) -> MedusaResult<()> {
        if commit.trim().is_empty() || path.as_os_str().is_empty() || path.exists() {
            return Err(invalid(
                "detached verification worktree requires a new path and commit",
            ));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        run_git(
            &self.repo,
            &["worktree", "add", "--detach", path_text(path)?, commit],
        )
    }

    /// Removes a detached verification worktree without touching implementation resources.
    pub fn remove_detached_worktree(&self, path: &Path) -> MedusaResult<()> {
        if path.exists() {
            run_git(
                &self.repo,
                &["worktree", "remove", "--force", path_text(path)?],
            )?;
        }
        run_git(&self.repo, &["worktree", "prune"])
    }

    /// Integrates exactly one commit that has a durable review and verification authorization.
    pub fn integrate_authorized(
        &self,
        worker: &Worker,
        expected_base: &str,
        authorized_commit: &str,
    ) -> MedusaResult<IntegrationReceipt> {
        if expected_base.trim().is_empty() || authorized_commit.trim().is_empty() {
            return Err(invalid("authorized integration requires base and commit"));
        }
        if worker.commit.as_deref() != Some(authorized_commit) {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                "worker commit does not match durable integration authorization",
            ));
        }
        if self.commit_is_integrated(authorized_commit)?
            || self.commit_tree_matches_head(authorized_commit)?
        {
            return Ok(IntegrationReceipt {
                worker_id: worker.id.clone(),
                branch: worker.branch.clone(),
                commit: authorized_commit.to_owned(),
                base_head: expected_base.to_owned(),
                integrated_head: self.repository_head()?,
                changed_paths: self.commit_changed_paths(authorized_commit)?,
                changed_components: self.commit_changed_components(authorized_commit)?,
            });
        }
        let actual_head = self.repository_head()?;
        if actual_head != expected_base {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                format!(
                    "primary repository drifted before authorized integration: expected {expected_base}, got {actual_head}"
                ),
            ));
        }
        self.integrate_successful(std::slice::from_ref(worker))?
            .into_iter()
            .next()
            .ok_or_else(|| invalid("authorized worker produced no integration receipt"))
    }

    /// Removes per-session runtime state and generated verification caches/logs from a worktree.
    ///
    /// Agent sessions persist under `.medusa`, supervised Python verification can create bytecode
    /// or test caches, and npm verification can create diagnostic logs under `.npm/_logs`.
    /// These files are execution residue, not product changes, and must never enter a worker
    /// commit. Files tracked by the base commit remain untouched and are still subject to ordinary
    /// scope validation.
    pub fn discard_untracked_runtime_state(
        &self,
        worker: &Worker,
        base_commit: &str,
    ) -> MedusaResult<()> {
        if base_commit.trim().is_empty() {
            return Err(invalid("worker base commit cannot be empty"));
        }
        let added_runtime_paths = git_nul_paths(
            &worker.worktree,
            &[
                "diff",
                "--name-only",
                "--diff-filter=A",
                "-z",
                base_commit,
                "--",
            ],
        )?
        .into_iter()
        .filter(|path| is_runtime_residue(path))
        .collect::<Vec<_>>();
        for path in added_runtime_paths {
            run_git(
                &worker.worktree,
                &["rm", "-f", "--ignore-unmatch", "--", &path],
            )?;
        }
        run_git(
            &worker.worktree,
            &[
                "clean",
                "-fdx",
                "--",
                ".medusa",
                ":(glob).npm/_logs/**",
                ":(glob)**/__pycache__/**",
                ":(glob)**/.pytest_cache/**",
                ":(glob)**/*.pyc",
                ":(glob)**/*.pyo",
            ],
        )
    }

    /// Runs combined repository verification after all worker commits merge.
    pub fn verify_combined(&self) -> MedusaResult<String> {
        #[cfg(windows)]
        let output = if self.repo.join("verify.ps1").is_file() {
            Command::new("powershell.exe")
                .args(["-NoProfile", "-File", "verify.ps1"])
                .current_dir(&self.repo)
                .output()?
        } else {
            Command::new("cargo")
                .args(["test", "--workspace", "--all-features"])
                .current_dir(&self.repo)
                .output()?
        };
        #[cfg(not(windows))]
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

    /// Removes worktrees and their temporary branches after acceptance or rejection.
    pub fn cleanup(&self, workers: &[Worker]) -> MedusaResult<()> {
        let mut first_error = None;
        for worker in workers {
            if worker.worktree.exists()
                && let Err(error) = run_git(
                    &self.repo,
                    &[
                        "worktree",
                        "remove",
                        "--force",
                        path_text(&worker.worktree)?,
                    ],
                )
                && first_error.is_none()
            {
                first_error = Some(error);
            }
            match branch_exists(&self.repo, &worker.branch) {
                Ok(true) => {
                    if let Err(error) = run_git(&self.repo, &["branch", "-D", &worker.branch])
                        && first_error.is_none()
                    {
                        first_error = Some(error);
                    }
                }
                Ok(false) => {}
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        if let Err(error) = run_git(&self.repo, &["worktree", "prune"])
            && first_error.is_none()
        {
            first_error = Some(error);
        }
        if self.worktree_root.is_dir() && fs::read_dir(&self.worktree_root)?.next().is_none() {
            fs::remove_dir(&self.worktree_root)?;
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn execute_worker(mut worker: Worker, task: DelegatedTask) -> MedusaResult<Worker> {
    worker.state = WorkerState::Running;
    let output = Command::new(&task.program)
        .args(&task.args)
        .current_dir(&worker.worktree)
        .output()?;
    worker.stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    worker.stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        worker.state = WorkerState::Failed;
        return Ok(worker);
    }
    run_git(&worker.worktree, &["add", "-A"])?;
    run_git(
        &worker.worktree,
        &[
            "-c",
            "user.name=Medusa",
            "-c",
            "user.email=medusa@users.noreply.github.com",
            "commit",
            "-m",
            &task.commit_message,
        ],
    )?;
    worker.commit = Some(git_stdout(&worker.worktree, &["rev-parse", "HEAD"])?);
    worker.state = WorkerState::Succeeded;
    Ok(worker)
}

fn changed_components_for_commit(repo: &Path, commit: &str) -> MedusaResult<Vec<ChangedComponent>> {
    let components = git_changed_components(
        repo,
        &[
            "diff-tree",
            "--root",
            "--no-commit-id",
            "--name-status",
            "-M",
            "-C",
            "-r",
            "-z",
            commit,
        ],
    )?;
    normalize_components(repo, &components).map_err(|error| invalid(error.to_string()))
}

fn changed_component_paths(components: &[ChangedComponent]) -> Vec<String> {
    let mut paths = components
        .iter()
        .flat_map(ChangedComponent::all_paths)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn git_changed_components(repo: &Path, args: &[&str]) -> MedusaResult<Vec<ChangedComponent>> {
    let output = git_command(repo).args(args).output()?;
    if !output.status.success() {
        return output_result(&format!("git {}", args.join(" ")), output).map(|_| Vec::new());
    }
    let fields = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| {
            String::from_utf8(field.to_vec()).map_err(|error| {
                invalid(format!("Git returned non-UTF-8 change metadata: {error}"))
            })
        })
        .collect::<MedusaResult<Vec<_>>>()?;
    let mut components = Vec::new();
    let mut index = 0;
    while index < fields.len() {
        let status = fields[index].as_str();
        index += 1;
        let code = status.chars().next().unwrap_or('X');
        let component = match code {
            'R' | 'C' => {
                let previous = fields
                    .get(index)
                    .ok_or_else(|| invalid("Git rename source missing"))?;
                let path = fields
                    .get(index + 1)
                    .ok_or_else(|| invalid("Git rename target missing"))?;
                index += 2;
                if code == 'R' {
                    ChangedComponent::renamed(previous.clone(), path.clone())
                } else {
                    let mut component = ChangedComponent::new(ChangeKind::Copied, path.clone())
                        .map_err(|error| invalid(error.to_string()))?;
                    component.previous_path = Some(previous.clone());
                    Ok(component)
                }
            }
            _ => {
                let path = fields
                    .get(index)
                    .ok_or_else(|| invalid("Git change path missing"))?;
                index += 1;
                ChangedComponent::new(
                    match code {
                        'A' => ChangeKind::Added,
                        'M' => ChangeKind::Modified,
                        'D' => ChangeKind::Deleted,
                        'T' => ChangeKind::TypeChanged,
                        'U' => ChangeKind::Unmerged,
                        _ => ChangeKind::Unknown,
                    },
                    path.clone(),
                )
            }
        }
        .map_err(|error| invalid(error.to_string()))?;
        components.push(component);
    }
    Ok(components)
}

fn is_runtime_residue(path: &str) -> bool {
    path == ".medusa"
        || path.starts_with(".medusa/")
        || path.starts_with(".npm/_logs/")
        || path
            .split('/')
            .any(|component| matches!(component, "__pycache__" | ".pytest_cache"))
        || path.ends_with(".pyc")
        || path.ends_with(".pyo")
}

fn ensure_clean(repo: &Path) -> MedusaResult<()> {
    let status = git_stdout(repo, &["status", "--porcelain", "--untracked-files=all"])?;
    let dirty = status
        .lines()
        .filter(|line| {
            let path = line.get(3..).unwrap_or_default().trim_matches('"');
            !(line.starts_with("?? ") && (path == ".medusa" || path.starts_with(".medusa/")))
        })
        .collect::<Vec<_>>();
    if !dirty.is_empty() {
        Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!(
                "merge coordinator requires a clean repository outside Medusa runtime state; dirty entries: {}",
                dirty.join(", ")
            ),
        ))
    } else {
        Ok(())
    }
}

fn branch_exists(repo: &Path, branch: &str) -> MedusaResult<bool> {
    git_success(
        repo,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
}

fn git_command(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .args(["-c", "core.longPaths=true"])
        .current_dir(repo);
    command
}

fn run_git(repo: &Path, args: &[&str]) -> MedusaResult<()> {
    let output = git_command(repo).args(args).output()?;
    output_result(&format!("git {}", args.join(" ")), output).map(|_| ())
}

fn git_stdout(repo: &Path, args: &[&str]) -> MedusaResult<String> {
    let output = git_command(repo).args(args).output()?;
    output_result(&format!("git {}", args.join(" ")), output).map(|text| text.trim().to_owned())
}

fn git_success(repo: &Path, args: &[&str]) -> MedusaResult<bool> {
    let output = git_command(repo).args(args).output()?;
    Ok(output.status.success())
}

fn git_nul_paths(repo: &Path, args: &[&str]) -> MedusaResult<Vec<String>> {
    let output = git_command(repo).args(args).output()?;
    if !output.status.success() {
        return output_result(&format!("git {}", args.join(" ")), output).map(|_| Vec::new());
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            String::from_utf8(path.to_vec()).map_err(|error| {
                MedusaError::new(
                    ErrorCode::InvalidConfiguration,
                    ErrorCategory::Validation,
                    format!("Git returned a non-UTF-8 repository path: {error}"),
                )
            })
        })
        .collect()
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
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        ))
    }
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
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
        Err(invalid(format!("invalid worker label: {label}")))
    }
}

fn validate_worker_id(worker_id: &str) -> MedusaResult<()> {
    if !worker_id.is_empty()
        && worker_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        Ok(())
    } else {
        Err(invalid(format!("invalid worker identifier: {worker_id}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(repo: &Path, args: &[&str]) {
        run_git(repo, args).expect("git command");
    }

    fn repository() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let directory = tempfile::tempdir().expect("tempdir");
        let repo = directory.path().join("repo");
        let worktrees = directory.path().join("worktrees");
        fs::create_dir(&repo).expect("repo");
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "core.autocrlf", "false"]);
        git(&repo, &["config", "user.name", "Medusa Test"]);
        git(&repo, &["config", "user.email", "medusa@example.invalid"]);
        fs::write(repo.join("base.txt"), "base\n").expect("base");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "base"]);
        (directory, repo, worktrees)
    }

    #[test]
    fn exact_scope_preserves_rename_delete_generated_and_owner() {
        let (_directory, repo, worktrees) = repository();
        fs::create_dir_all(repo.join("apps/web/src")).expect("source directory");
        fs::write(repo.join("apps/web/package.json"), "{}\n").expect("package");
        fs::write(repo.join("apps/web/src/old.tsx"), "old\n").expect("old source");
        fs::write(repo.join("apps/web/src/delete.css"), "delete\n").expect("deleted source");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "fixture"]);
        let base = git_stdout(&repo, &["rev-parse", "HEAD"]).expect("base");
        let manager = WorkerManager::new(&repo, &worktrees).expect("manager");
        let worker = manager.create_worker("scope").expect("worker");
        git(
            &worker.worktree,
            &["mv", "apps/web/src/old.tsx", "apps/web/src/new.tsx"],
        );
        fs::remove_file(worker.worktree.join("apps/web/src/delete.css")).expect("delete");
        fs::create_dir_all(worker.worktree.join("apps/web/generated")).expect("generated");
        fs::write(
            worker.worktree.join("apps/web/generated/schema.json"),
            "{}\n",
        )
        .expect("generated artifact");
        let components = manager
            .changed_components_since(&worker, &base)
            .expect("components");
        assert!(components.iter().any(|component| {
            component.kind == ChangeKind::Renamed
                && component.previous_path.as_deref() == Some("apps/web/src/old.tsx")
                && component.path == "apps/web/src/new.tsx"
        }));
        assert!(
            components
                .iter()
                .any(|component| component.kind == ChangeKind::Deleted)
        );
        assert!(components.iter().any(|component| {
            component.generated && component.package_owner.as_deref() == Some("apps/web")
        }));
        manager.cleanup(&[worker]).expect("cleanup");
    }

    #[test]
    fn parallel_feature_fixture_merges_and_verifies() {
        let (_directory, repo, worktrees) = repository();
        #[cfg(windows)]
        fs::write(
            repo.join("verify.ps1"),
            "$ErrorActionPreference = 'Stop'\nif ((Get-Content -Raw feature-a.txt) -ne 'alpha') { exit 1 }\nif ((Get-Content -Raw feature-b.txt) -ne 'beta') { exit 1 }\nWrite-Output 'combined-verification-ok'\n",
        )
        .expect("verify");
        #[cfg(not(windows))]
        fs::write(
            repo.join("verify.sh"),
            "#!/bin/sh\nset -eu\ntest \"$(cat feature-a.txt)\" = alpha\ntest \"$(cat feature-b.txt)\" = beta\necho combined-verification-ok\n",
        )
        .expect("verify");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "verification"]);

        let manager = WorkerManager::new(&repo, &worktrees).expect("manager");
        let worker_a = manager.create_worker("feature-a").expect("worker a");
        let worker_b = manager.create_worker("feature-b").expect("worker b");
        #[cfg(windows)]
        let tasks = vec![
            (
                worker_a,
                DelegatedTask {
                    program: "powershell.exe".into(),
                    args: vec![
                        "-NoProfile".into(),
                        "-Command".into(),
                        "[IO.File]::WriteAllText('feature-a.txt','alpha')".into(),
                    ],
                    commit_message: "add feature a".into(),
                },
            ),
            (
                worker_b,
                DelegatedTask {
                    program: "powershell.exe".into(),
                    args: vec![
                        "-NoProfile".into(),
                        "-Command".into(),
                        "[IO.File]::WriteAllText('feature-b.txt','beta')".into(),
                    ],
                    commit_message: "add feature b".into(),
                },
            ),
        ];
        #[cfg(not(windows))]
        let tasks = vec![
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
        ];
        let workers = manager.delegate_parallel(tasks).expect("delegate");
        assert!(
            workers
                .iter()
                .all(|worker| worker.state == WorkerState::Succeeded)
        );
        assert_eq!(manager.merge_successful(&workers).expect("merge").len(), 2);
        assert!(
            manager
                .verify_combined()
                .expect("verify")
                .contains("combined-verification-ok")
        );
        manager.cleanup(&workers).expect("cleanup");
    }

    #[test]
    fn isolated_worktrees_do_not_share_uncommitted_changes() {
        let (_directory, repo, worktrees) = repository();
        let manager = WorkerManager::new(&repo, &worktrees).expect("manager");
        let worker_a = manager.create_worker("a").expect("worker a");
        let worker_b = manager.create_worker("b").expect("worker b");
        fs::write(worker_a.worktree.join("private.txt"), "worker-a\n").expect("write a");
        assert!(!worker_b.worktree.join("private.txt").exists());
        assert!(!repo.join("private.txt").exists());
        manager.cleanup(&[worker_a, worker_b]).expect("cleanup");
    }

    #[test]
    fn clean_check_allows_only_untracked_medusa_runtime_state() {
        let (_directory, repo, worktrees) = repository();
        let manager = WorkerManager::new(&repo, &worktrees).expect("manager");
        fs::create_dir_all(repo.join(".medusa/sessions/session-1")).expect("runtime directory");
        fs::write(repo.join(".medusa/sessions/session-1/session.json"), "{}\n")
            .expect("runtime state");

        manager.require_clean().expect("runtime state is allowed");
    }

    #[test]
    fn clean_check_reports_every_dirty_entry() {
        let (_directory, repo, worktrees) = repository();
        let manager = WorkerManager::new(&repo, &worktrees).expect("manager");
        fs::write(repo.join("base.txt"), "changed\n").expect("tracked change");
        fs::write(repo.join("unexpected.txt"), "untracked\n").expect("untracked change");

        let error = manager.require_clean().expect_err("dirty repository");
        let message = error.to_string();
        assert!(message.contains("base.txt"));
        assert!(message.contains("unexpected.txt"));
    }

    #[test]
    fn runtime_residue_classification_is_narrow() {
        for path in [
            ".medusa/session.json",
            ".npm/_logs/2026-08-18T07_40_00_000Z-debug-0.log",
            "src/__pycache__/slugify.cpython-312.pyc",
            "src/.pytest_cache/state",
            "generated.pyc",
            "generated.pyo",
        ] {
            assert!(is_runtime_residue(path), "{path} must be runtime residue");
        }
        for path in [
            "src/slugify.py",
            "src/cache.rs",
            "docs/pytest_cache.md",
            ".npm/product-state.json",
            "packages/web/.npm/_logs/product.log",
        ] {
            assert!(
                !is_runtime_residue(path),
                "{path} must remain product content"
            );
        }
    }

    #[test]
    fn worker_cleanup_discards_ignored_runtime_state_and_preserves_tracked_state() {
        let (_directory, repo, worktrees) = repository();
        fs::write(repo.join(".gitignore"), ".medusa/\n").expect("gitignore");
        fs::create_dir(repo.join(".medusa")).expect("runtime directory");
        fs::write(repo.join(".medusa/policy.json"), "{}\n").expect("tracked state");
        git(&repo, &["add", ".gitignore"]);
        git(&repo, &["add", "-f", ".medusa/policy.json"]);
        git(&repo, &["commit", "-m", "track runtime policy"]);

        let manager = WorkerManager::new(&repo, &worktrees).expect("manager");
        let base = manager.repository_head().expect("base");
        let worker = manager.create_worker("runtime-cleanup").expect("worker");
        fs::create_dir_all(worker.worktree.join(".medusa/artifacts")).expect("artifacts");
        fs::write(
            worker.worktree.join(".medusa/artifacts/fs_read.txt"),
            "runtime output\n",
        )
        .expect("runtime artifact");
        fs::write(
            worker
                .worktree
                .join(".medusa/artifacts/fs_read_committed.txt"),
            "committed runtime output\n",
        )
        .expect("committed runtime artifact");
        fs::create_dir_all(worker.worktree.join("src/__pycache__")).expect("python cache");
        fs::write(
            worker
                .worktree
                .join("src/__pycache__/slugify.cpython-312.pyc"),
            "generated bytecode\n",
        )
        .expect("python bytecode");
        git(
            &worker.worktree,
            &[
                "add",
                "-f",
                ".medusa/artifacts/fs_read_committed.txt",
                "src/__pycache__/slugify.cpython-312.pyc",
            ],
        );
        git(&worker.worktree, &["commit", "-m", "worker checkpoint"]);
        fs::create_dir_all(worker.worktree.join("src/.pytest_cache")).expect("pytest cache");
        fs::write(
            worker.worktree.join("src/.pytest_cache/state"),
            "generated test cache\n",
        )
        .expect("pytest state");

        manager
            .discard_untracked_runtime_state(&worker, &base)
            .expect("discard runtime state");

        assert!(worker.worktree.join(".medusa/policy.json").is_file());
        assert!(
            !worker
                .worktree
                .join(".medusa/artifacts/fs_read.txt")
                .exists()
        );
        assert!(
            !worker
                .worktree
                .join(".medusa/artifacts/fs_read_committed.txt")
                .exists()
        );
        assert!(!worker.worktree.join("src/__pycache__").exists());
        assert!(!worker.worktree.join("src/.pytest_cache").exists());
        assert!(
            manager
                .changed_paths_since(&worker, &base)
                .expect("changed paths")
                .is_empty()
        );
        manager.cleanup(&[worker]).expect("cleanup");
    }

    #[test]
    fn npm_log_cleanup_does_not_hide_other_npm_paths() {
        let (_directory, repo, worktrees) = repository();
        let manager = WorkerManager::new(&repo, &worktrees).expect("manager");
        let base = manager.repository_head().expect("base");
        let worker = manager.create_worker("npm-log-cleanup").expect("worker");
        fs::create_dir_all(worker.worktree.join(".npm/_logs")).expect("npm logs");
        fs::write(
            worker.worktree.join(".npm/_logs/2026-debug-0.log"),
            "npm diagnostic\n",
        )
        .expect("npm log");
        fs::write(worker.worktree.join(".npm/product-state.json"), "{}\n").expect("product state");

        manager
            .discard_untracked_runtime_state(&worker, &base)
            .expect("discard npm log");

        assert!(!worker.worktree.join(".npm/_logs/2026-debug-0.log").exists());
        assert!(worker.worktree.join(".npm/product-state.json").is_file());
        assert_eq!(
            manager
                .changed_paths_since(&worker, &base)
                .expect("changed paths"),
            vec![".npm/product-state.json"]
        );
        manager.cleanup(&[worker]).expect("cleanup");
    }

    #[test]
    fn overlapping_worker_paths_are_rejected_before_integration() {
        let (_directory, repo, worktrees) = repository();
        let manager = WorkerManager::new(&repo, &worktrees).expect("manager");
        let base = manager.repository_head().expect("base");
        let worker_a = manager.create_worker("a").expect("worker a");
        let worker_b = manager.create_worker("b").expect("worker b");
        fs::write(worker_a.worktree.join("shared.txt"), "a\n").expect("write a");
        fs::write(worker_b.worktree.join("shared.txt"), "b\n").expect("write b");
        let worker_a = manager
            .finalize_worker(worker_a, &base, "worker a")
            .expect("commit a");
        let worker_b = manager
            .finalize_worker(worker_b, &base, "worker b")
            .expect("commit b");
        let error = manager
            .integrate_successful(&[worker_a.clone(), worker_b.clone()])
            .expect_err("overlap must fail");
        assert!(error.to_string().contains("path overlap"));
        assert!(!repo.join("shared.txt").exists());
        manager.cleanup(&[worker_a, worker_b]).expect("cleanup");
    }

    #[test]
    fn integration_conflict_rolls_back_to_the_preintegration_head() {
        let (_directory, repo, worktrees) = repository();
        let manager = WorkerManager::new(&repo, &worktrees).expect("manager");
        let base = manager.repository_head().expect("base");
        let worker = manager.create_worker("conflict").expect("worker");
        fs::write(worker.worktree.join("base.txt"), "worker\n").expect("worker change");
        let worker = manager
            .finalize_worker(worker, &base, "worker change")
            .expect("worker commit");

        fs::write(repo.join("base.txt"), "primary\n").expect("primary change");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "primary change"]);
        let preintegration = manager.repository_head().expect("preintegration");
        assert!(
            manager
                .integrate_successful(std::slice::from_ref(&worker))
                .is_err()
        );
        assert_eq!(manager.repository_head().expect("head"), preintegration);
        assert_eq!(
            fs::read_to_string(repo.join("base.txt")).unwrap(),
            "primary\n"
        );
        manager.cleanup(&[worker]).expect("cleanup");
    }

    #[cfg(windows)]
    #[test]
    fn windows_cleanup_handles_deep_medusa_runtime_paths() {
        let (_directory, repo, worktrees) = repository();
        let manager = WorkerManager::new(&repo, &worktrees).expect("manager");
        let base = manager.repository_head().expect("base");
        let worker = manager.create_worker("long-path-cleanup").expect("worker");
        let worktree = worker.worktree.clone();
        let execution_id = "e".repeat(64);
        let session_id = "s".repeat(64);
        let request_id = "r".repeat(64);
        let artifact_id = "a".repeat(64);
        let deep = worker
            .worktree
            .join(".medusa")
            .join("executions")
            .join(execution_id)
            .join("sessions")
            .join(session_id)
            .join("request-manifests")
            .join(request_id)
            .join("artifacts")
            .join(artifact_id);
        fs::create_dir_all(&deep).expect("deep runtime directory");
        let manifest = deep.join("request-manifest.json");
        fs::write(&manifest, "{}\n").expect("deep runtime manifest");
        assert!(
            manifest.as_os_str().to_string_lossy().len() > 260,
            "fixture must exceed the legacy Windows path limit"
        );

        manager
            .discard_untracked_runtime_state(&worker, &base)
            .expect("discard deep runtime state");
        assert!(!manifest.exists(), "deep runtime manifest must be removed");

        manager.cleanup(&[worker]).expect("cleanup worktree");
        assert!(!worktree.exists());
    }

    #[test]
    fn cleanup_removes_worktree_and_temporary_branch() {
        let (_directory, repo, worktrees) = repository();
        let manager = WorkerManager::new(&repo, &worktrees).expect("manager");
        let worker = manager.create_worker("cleanup").expect("worker");
        let worktree = worker.worktree.clone();
        let branch = worker.branch.clone();
        assert!(worktree.is_dir());
        assert!(branch_exists(&repo, &branch).expect("branch exists"));
        manager.cleanup(&[worker]).expect("cleanup");
        assert!(!worktree.exists());
        assert!(!branch_exists(&repo, &branch).expect("branch removed"));
    }
}
