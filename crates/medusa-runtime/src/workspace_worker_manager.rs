//! Workspace-aware mutation backend used by the production runtime.
//!
//! Git repositories delegate to `medusa-workers` unchanged. Plain directories use immutable,
//! content-addressed snapshots so the existing parent-review, independent-verification,
//! authorization, integration, reconciliation, and recovery state machine can stay authoritative
//! without requiring a `.git` directory or a Git executable.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Output},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_evidence::{ChangeKind, ChangedComponent, normalize_components};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::git_workers::{
    DelegatedTask, IntegrationReceipt, Worker, WorkerManager as GitWorkerManager, WorkerState,
};

const DIRECTORY_REVISION_PREFIX: &str = "dir-";
const PATCH_TEXT_LIMIT: usize = 128 * 1024;

fn hidden_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    command
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceMutationBackend {
    Git,
    Directory,
}

#[derive(Clone, Debug)]
pub struct WorkspaceWorkerManager {
    repo: PathBuf,
    worktree_root: PathBuf,
    backend: WorkspaceMutationBackend,
    git: Option<GitWorkerManager>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct FileFingerprint {
    sha256: String,
    size_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DirectoryManifest {
    files: BTreeMap<String, FileFingerprint>,
    tree: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DirectorySnapshot {
    commit: String,
    tree: String,
    base: String,
    changed_components: Vec<ChangedComponent>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DirectoryWorkerState {
    base: String,
    branch: String,
}

impl WorkspaceWorkerManager {
    pub fn new(repo: impl Into<PathBuf>, worktree_root: impl Into<PathBuf>) -> MedusaResult<Self> {
        let repo = repo.into();
        let worktree_root = worktree_root.into();
        if !repo.is_dir() {
            return Err(invalid(format!(
                "workspace root does not exist or is not a directory: {}",
                repo.display()
            )));
        }
        fs::create_dir_all(&worktree_root)?;
        let backend = if is_git_repository(&repo) {
            WorkspaceMutationBackend::Git
        } else {
            WorkspaceMutationBackend::Directory
        };
        let git = if backend == WorkspaceMutationBackend::Git {
            Some(GitWorkerManager::new(&repo, &worktree_root)?)
        } else {
            None
        };
        Ok(Self {
            repo,
            worktree_root,
            backend,
            git,
        })
    }

    #[must_use]
    pub fn backend(&self) -> WorkspaceMutationBackend {
        self.backend
    }

    #[must_use]
    pub fn repository_path(&self) -> &Path {
        &self.repo
    }

    pub fn repository_head(&self) -> MedusaResult<String> {
        match &self.git {
            Some(manager) => manager.repository_head(),
            None => directory_revision(&self.repo),
        }
    }

    pub fn require_clean(&self) -> MedusaResult<()> {
        match &self.git {
            Some(manager) => manager.require_clean(),
            None => {
                // Plain directories have no staging/index cleanliness concept. Isolation binds the
                // worker to an immutable content revision, and integration rejects primary-workspace
                // drift against that revision before any file is replaced.
                directory_manifest(&self.repo).map(|_| ())
            }
        }
    }

    pub fn create_worker(&self, label: &str) -> MedusaResult<Worker> {
        match &self.git {
            Some(manager) => manager.create_worker(label),
            None => {
                let suffix = next_worker_suffix();
                self.create_directory_worker(label, &format!("wrk-{suffix}"))
            }
        }
    }

    pub fn create_worker_with_id(&self, label: &str, worker_id: &str) -> MedusaResult<Worker> {
        match &self.git {
            Some(manager) => manager.create_worker_with_id(label, worker_id),
            None => self.create_directory_worker(label, worker_id),
        }
    }

    pub fn open_or_create_worker(&self, label: &str, worker_id: &str) -> MedusaResult<Worker> {
        match &self.git {
            Some(manager) => manager.open_or_create_worker(label, worker_id),
            None => {
                validate_identifier(label, "worker label")?;
                validate_identifier(worker_id, "worker id")?;
                let worktree = self.worktree_root.join(worker_id);
                let state_path = self.directory_worker_state_path(worker_id);
                let branch = format!("workspace/{label}-{worker_id}");
                let current = self.repository_head()?;
                match (worktree.is_dir(), state_path.is_file()) {
                    (false, false) => self.create_directory_worker(label, worker_id),
                    (true, true) => {
                        let state: DirectoryWorkerState = read_json(&state_path)?;
                        if state.base != current || state.branch != branch {
                            return Err(policy(format!(
                                "existing directory worker is stale: expected base {current} and branch {branch}, found base {} and branch {}",
                                state.base, state.branch
                            )));
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
                    _ => Err(invalid(format!(
                        "directory worker resources are inconsistent for {worker_id}"
                    ))),
                }
            }
        }
    }

    pub fn changed_components_since(
        &self,
        worker: &Worker,
        base_commit: &str,
    ) -> MedusaResult<Vec<ChangedComponent>> {
        match &self.git {
            Some(manager) => manager.changed_components_since(worker, base_commit),
            None => {
                let baseline = self.load_baseline_manifest(base_commit)?;
                let current = directory_manifest(&worker.worktree)?;
                let components = changed_components(&baseline, &current)?;
                if components.is_empty() {
                    return Ok(Vec::new());
                }
                normalize_components(&worker.worktree, &components)
                    .map_err(|error| invalid(error.to_string()))
            }
        }
    }

    pub fn changed_paths_since(
        &self,
        worker: &Worker,
        base_commit: &str,
    ) -> MedusaResult<Vec<String>> {
        self.changed_components_since(worker, base_commit)
            .map(|components| component_paths(&components))
    }

    pub fn finalize_worker(
        &self,
        mut worker: Worker,
        base_commit: &str,
        commit_message: &str,
    ) -> MedusaResult<Worker> {
        match &self.git {
            Some(manager) => manager.finalize_worker(worker, base_commit, commit_message),
            None => {
                if commit_message.trim().is_empty() {
                    return Err(invalid("worker snapshot label cannot be empty"));
                }
                let changed_components = self.changed_components_since(&worker, base_commit)?;
                if changed_components.is_empty() {
                    return Err(execution(format!(
                        "worker {} completed without workspace changes",
                        worker.id
                    )));
                }
                let manifest = directory_manifest(&worker.worktree)?;
                let commit = format!("{DIRECTORY_REVISION_PREFIX}{}", manifest.tree);
                let snapshot_root = self.snapshot_root(&commit);
                let snapshot_tree = snapshot_root.join("tree");
                if !snapshot_tree.exists() {
                    fs::create_dir_all(&snapshot_root)?;
                    copy_manifest_tree(&worker.worktree, &snapshot_tree, &manifest)?;
                }
                write_json(
                    &snapshot_root.join("snapshot.json"),
                    &DirectorySnapshot {
                        commit: commit.clone(),
                        tree: manifest.tree,
                        base: base_commit.to_owned(),
                        changed_components,
                    },
                )?;
                worker.commit = Some(commit);
                worker.state = WorkerState::Succeeded;
                Ok(worker)
            }
        }
    }

    pub fn integrate_successful(
        &self,
        workers: &[Worker],
    ) -> MedusaResult<Vec<IntegrationReceipt>> {
        match &self.git {
            Some(manager) => manager.integrate_successful(workers),
            None => {
                let mut ordered = workers
                    .iter()
                    .filter(|worker| worker.state == WorkerState::Succeeded)
                    .collect::<Vec<_>>();
                ordered.sort_by(|left, right| left.id.cmp(&right.id));
                let mut receipts = Vec::with_capacity(ordered.len());
                let mut owned = BTreeMap::<String, String>::new();
                for worker in ordered {
                    let commit = worker
                        .commit
                        .as_deref()
                        .ok_or_else(|| invalid("successful directory worker has no snapshot"))?;
                    for path in self.commit_changed_paths(commit)? {
                        if let Some(previous) = owned.insert(path.clone(), worker.id.clone()) {
                            return Err(policy(format!(
                                "worker path overlap rejected before integration: {path} ({previous}, {})",
                                worker.id
                            )));
                        }
                    }
                }
                for worker in workers
                    .iter()
                    .filter(|worker| worker.state == WorkerState::Succeeded)
                {
                    let commit = worker
                        .commit
                        .as_deref()
                        .ok_or_else(|| invalid("successful directory worker has no snapshot"))?;
                    let snapshot = self.load_snapshot(commit)?;
                    receipts.push(self.integrate_directory(worker, &snapshot.base, commit)?);
                }
                Ok(receipts)
            }
        }
    }

    pub fn merge_successful(&self, workers: &[Worker]) -> MedusaResult<Vec<String>> {
        self.integrate_successful(workers)
            .map(|receipts| receipts.into_iter().map(|receipt| receipt.commit).collect())
    }

    pub fn commit_is_integrated(&self, commit: &str) -> MedusaResult<bool> {
        match &self.git {
            Some(manager) => manager.commit_is_integrated(commit),
            None => Ok(self.repository_head()? == commit),
        }
    }

    pub fn commit_tree_matches_head(&self, commit: &str) -> MedusaResult<bool> {
        match &self.git {
            Some(manager) => manager.commit_tree_matches_head(commit),
            None => {
                let snapshot = self.load_snapshot(commit)?;
                Ok(self.repository_head()?
                    == format!("{DIRECTORY_REVISION_PREFIX}{}", snapshot.tree))
            }
        }
    }

    pub fn commit_tree(&self, commit: &str) -> MedusaResult<String> {
        match &self.git {
            Some(manager) => manager.commit_tree(commit),
            None => Ok(self.load_snapshot(commit)?.tree),
        }
    }

    pub fn commit_patch(&self, base_commit: &str, commit: &str) -> MedusaResult<String> {
        match &self.git {
            Some(manager) => manager.commit_patch(base_commit, commit),
            None => self.directory_patch(base_commit, commit),
        }
    }

    pub fn commit_changed_components(&self, commit: &str) -> MedusaResult<Vec<ChangedComponent>> {
        match &self.git {
            Some(manager) => manager.commit_changed_components(commit),
            None => Ok(self.load_snapshot(commit)?.changed_components),
        }
    }

    pub fn commit_changed_paths(&self, commit: &str) -> MedusaResult<Vec<String>> {
        self.commit_changed_components(commit)
            .map(|components| component_paths(&components))
    }

    pub fn materialize_detached_commit(&self, commit: &str, path: &Path) -> MedusaResult<()> {
        match &self.git {
            Some(manager) => manager.materialize_detached_commit(commit, path),
            None => {
                if path.exists() {
                    return Err(invalid("verification workspace path already exists"));
                }
                let snapshot = self.load_snapshot(commit)?;
                let source = self.snapshot_root(&snapshot.commit).join("tree");
                let manifest = directory_manifest(&source)?;
                copy_manifest_tree(&source, path, &manifest)
            }
        }
    }

    pub fn remove_detached_worktree(&self, path: &Path) -> MedusaResult<()> {
        match &self.git {
            Some(manager) => manager.remove_detached_worktree(path),
            None => {
                if path.exists() {
                    fs::remove_dir_all(path)?;
                }
                Ok(())
            }
        }
    }

    pub fn integrate_authorized(
        &self,
        worker: &Worker,
        expected_base: &str,
        authorized_commit: &str,
    ) -> MedusaResult<IntegrationReceipt> {
        match &self.git {
            Some(manager) => manager.integrate_authorized(worker, expected_base, authorized_commit),
            None => self.integrate_directory(worker, expected_base, authorized_commit),
        }
    }

    pub fn discard_untracked_runtime_state(
        &self,
        worker: &Worker,
        base_commit: &str,
    ) -> MedusaResult<()> {
        match &self.git {
            Some(manager) => manager.discard_untracked_runtime_state(worker, base_commit),
            None => {
                if base_commit.trim().is_empty() {
                    return Err(invalid("worker base revision cannot be empty"));
                }
                remove_runtime_residue(&worker.worktree)
            }
        }
    }

    pub fn verify_combined(&self) -> MedusaResult<String> {
        match &self.git {
            Some(manager) => manager.verify_combined(),
            None => verify_directory_workspace(&self.repo),
        }
    }

    pub fn delegate_parallel(
        &self,
        assignments: Vec<(Worker, DelegatedTask)>,
    ) -> MedusaResult<Vec<Worker>> {
        match &self.git {
            Some(manager) => manager.delegate_parallel(assignments),
            None => Err(policy(
                "command-level parallel mutation is unavailable for directory workspaces; use the production isolated implementer path",
            )),
        }
    }

    pub fn cleanup(&self, workers: &[Worker]) -> MedusaResult<()> {
        match &self.git {
            Some(manager) => manager.cleanup(workers),
            None => {
                for worker in workers {
                    if worker.worktree.exists() {
                        fs::remove_dir_all(&worker.worktree)?;
                    }
                    let state = self.directory_worker_state_path(&worker.id);
                    if state.exists() {
                        fs::remove_file(state)?;
                    }
                }
                Ok(())
            }
        }
    }

    fn create_directory_worker(&self, label: &str, worker_id: &str) -> MedusaResult<Worker> {
        validate_identifier(label, "worker label")?;
        validate_identifier(worker_id, "worker id")?;
        let worktree = self.worktree_root.join(worker_id);
        let state_path = self.directory_worker_state_path(worker_id);
        if worktree.exists() || state_path.exists() {
            return Err(policy(format!(
                "worker resources already exist for {worker_id}"
            )));
        }
        let manifest = directory_manifest(&self.repo)?;
        let base = format!("{DIRECTORY_REVISION_PREFIX}{}", manifest.tree);
        self.persist_baseline(&base, &manifest)?;
        copy_manifest_tree(&self.repo, &worktree, &manifest)?;
        let branch = format!("workspace/{label}-{worker_id}");
        write_json(
            &state_path,
            &DirectoryWorkerState {
                base,
                branch: branch.clone(),
            },
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

    fn persist_baseline(&self, base: &str, manifest: &DirectoryManifest) -> MedusaResult<()> {
        let root = self.baseline_root(base);
        let tree = root.join("tree");
        if !tree.exists() {
            fs::create_dir_all(&root)?;
            copy_manifest_tree(&self.repo, &tree, manifest)?;
            write_json(&root.join("manifest.json"), manifest)?;
        }
        Ok(())
    }

    fn load_baseline_manifest(&self, base: &str) -> MedusaResult<DirectoryManifest> {
        read_json(&self.baseline_root(base).join("manifest.json"))
    }

    fn load_snapshot(&self, commit: &str) -> MedusaResult<DirectorySnapshot> {
        if !commit.starts_with(DIRECTORY_REVISION_PREFIX) {
            return Err(invalid("directory snapshot identifier is invalid"));
        }
        read_json(&self.snapshot_root(commit).join("snapshot.json"))
    }

    fn directory_patch(&self, base: &str, commit: &str) -> MedusaResult<String> {
        let snapshot = self.load_snapshot(commit)?;
        if snapshot.base != base {
            return Err(policy(format!(
                "snapshot base mismatch: expected {base}, found {}",
                snapshot.base
            )));
        }
        let before = self.baseline_root(base).join("tree");
        let after = self.snapshot_root(commit).join("tree");
        let mut patch = String::new();
        for component in &snapshot.changed_components {
            for path in component.all_paths() {
                if patch.len() >= PATCH_TEXT_LIMIT {
                    patch.push_str("\n[Medusa patch truncated at bounded review limit]\n");
                    break;
                }
                patch.push_str(&render_path_change(&before, &after, path)?);
            }
        }
        if patch.trim().is_empty() {
            return Err(invalid("directory snapshot patch is empty"));
        }
        Ok(patch)
    }

    fn integrate_directory(
        &self,
        worker: &Worker,
        expected_base: &str,
        authorized_commit: &str,
    ) -> MedusaResult<IntegrationReceipt> {
        if worker.commit.as_deref() != Some(authorized_commit) {
            return Err(policy(
                "worker snapshot does not match durable integration authorization",
            ));
        }
        let snapshot = self.load_snapshot(authorized_commit)?;
        if snapshot.base != expected_base {
            return Err(policy(
                "authorized directory snapshot has a different base revision",
            ));
        }
        if self.commit_is_integrated(authorized_commit)? {
            return Ok(IntegrationReceipt {
                worker_id: worker.id.clone(),
                branch: worker.branch.clone(),
                commit: authorized_commit.to_owned(),
                base_head: expected_base.to_owned(),
                integrated_head: self.repository_head()?,
                changed_paths: component_paths(&snapshot.changed_components),
                changed_components: snapshot.changed_components,
            });
        }
        let actual = self.repository_head()?;
        if actual != expected_base {
            return Err(policy(format!(
                "primary directory workspace drifted before integration: expected {expected_base}, got {actual}"
            )));
        }

        let snapshot_tree = self.snapshot_root(authorized_commit).join("tree");
        let rollback = self.worktree_root.join("rollback").join(format!(
            "{}-{}",
            worker.id,
            next_worker_suffix()
        ));
        fs::create_dir_all(&rollback)?;
        let mut existed = BTreeSet::new();
        let changed_paths = component_paths(&snapshot.changed_components);
        for path in &changed_paths {
            let source = self.repo.join(path);
            if source.is_file() {
                existed.insert(path.clone());
                let destination = rollback.join(path);
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(source, destination)?;
            }
        }

        let apply = (|| -> MedusaResult<()> {
            for path in &changed_paths {
                let prepared = snapshot_tree.join(path);
                let destination = self.repo.join(path);
                if prepared.is_file() {
                    copy_file_atomic(&prepared, &destination)?;
                } else {
                    remove_path(&destination)?;
                }
            }
            let integrated = self.repository_head()?;
            if integrated != authorized_commit {
                return Err(invalid(format!(
                    "integrated directory tree {integrated} does not match authorized snapshot {authorized_commit}"
                )));
            }
            Ok(())
        })();

        if let Err(error) = apply {
            let rollback_error = restore_paths(&self.repo, &rollback, &changed_paths, &existed);
            let _ = fs::remove_dir_all(&rollback);
            return match rollback_error {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Internal,
                    format!(
                        "directory integration failed and rollback also failed: {error}; rollback={rollback_error}"
                    ),
                )),
            };
        }
        fs::remove_dir_all(&rollback)?;
        Ok(IntegrationReceipt {
            worker_id: worker.id.clone(),
            branch: worker.branch.clone(),
            commit: authorized_commit.to_owned(),
            base_head: expected_base.to_owned(),
            integrated_head: self.repository_head()?,
            changed_paths,
            changed_components: snapshot.changed_components,
        })
    }

    fn baseline_root(&self, base: &str) -> PathBuf {
        self.worktree_root.join("baselines").join(base)
    }

    fn snapshot_root(&self, commit: &str) -> PathBuf {
        self.worktree_root.join("snapshots").join(commit)
    }

    fn directory_worker_state_path(&self, worker_id: &str) -> PathBuf {
        self.worktree_root
            .join("worker-state")
            .join(format!("{worker_id}.json"))
    }
}

#[must_use]
pub fn is_git_repository(path: &Path) -> bool {
    let Ok(output) = hidden_command("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(path)
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let top_level = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let Ok(selected) = path.canonicalize() else {
        return false;
    };
    let Ok(top_level) = top_level.canonicalize() else {
        return false;
    };
    selected == top_level
}

fn directory_revision(path: &Path) -> MedusaResult<String> {
    directory_manifest(path).map(|manifest| format!("{DIRECTORY_REVISION_PREFIX}{}", manifest.tree))
}

fn directory_manifest(root: &Path) -> MedusaResult<DirectoryManifest> {
    fn visit(
        root: &Path,
        directory: &Path,
        files: &mut BTreeMap<String, FileFingerprint>,
    ) -> MedusaResult<()> {
        let mut entries = fs::read_dir(directory)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|error| invalid(error.to_string()))?;
            let normalized = relative.to_string_lossy().replace('\\', "/");
            if excluded_workspace_path(&normalized) {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(policy(format!(
                    "directory workspace isolation fails closed on symlink `{normalized}`; use a Git workspace for symlink-bearing mutation"
                )));
            }
            if metadata.is_dir() {
                visit(root, &path, files)?;
            } else if metadata.is_file() {
                let bytes = fs::read(&path)?;
                files.insert(
                    normalized,
                    FileFingerprint {
                        sha256: format!("{:x}", Sha256::digest(&bytes)),
                        size_bytes: bytes.len() as u64,
                    },
                );
            }
        }
        Ok(())
    }

    let mut files = BTreeMap::new();
    visit(root, root, &mut files)?;
    let encoded = serde_json::to_vec(&files).map_err(|error| invalid(error.to_string()))?;
    Ok(DirectoryManifest {
        tree: format!("{:x}", Sha256::digest(encoded)),
        files,
    })
}

fn changed_components(
    before: &DirectoryManifest,
    after: &DirectoryManifest,
) -> MedusaResult<Vec<ChangedComponent>> {
    let keys = before
        .files
        .keys()
        .chain(after.files.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut changes = Vec::new();
    for path in keys {
        let kind = match (before.files.get(&path), after.files.get(&path)) {
            (None, Some(_)) => Some(ChangeKind::Added),
            (Some(_), None) => Some(ChangeKind::Deleted),
            (Some(left), Some(right)) if left != right => Some(ChangeKind::Modified),
            _ => None,
        };
        if let Some(kind) = kind {
            changes.push(
                ChangedComponent::new(kind, path).map_err(|error| invalid(error.to_string()))?,
            );
        }
    }
    Ok(changes)
}

fn component_paths(components: &[ChangedComponent]) -> Vec<String> {
    let mut paths = components
        .iter()
        .flat_map(ChangedComponent::all_paths)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn copy_manifest_tree(
    source: &Path,
    destination: &Path,
    manifest: &DirectoryManifest,
) -> MedusaResult<()> {
    fs::create_dir_all(destination)?;
    for relative in manifest.files.keys() {
        let source_path = source.join(relative);
        let destination_path = destination.join(relative);
        if let Some(parent) = destination_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source_path, destination_path)?;
    }
    Ok(())
}

fn render_path_change(before_root: &Path, after_root: &Path, path: &str) -> MedusaResult<String> {
    let before = read_bounded_text(&before_root.join(path))?;
    let after = read_bounded_text(&after_root.join(path))?;
    let mut rendered = format!("diff --medusa a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n");
    match (before, after) {
        (Some(before), Some(after)) => {
            rendered.push_str("@@ directory-snapshot @@\n");
            for line in before.lines() {
                rendered.push('-');
                rendered.push_str(line);
                rendered.push('\n');
            }
            for line in after.lines() {
                rendered.push('+');
                rendered.push_str(line);
                rendered.push('\n');
            }
        }
        (None, Some(after)) => {
            rendered.push_str("@@ added @@\n");
            for line in after.lines() {
                rendered.push('+');
                rendered.push_str(line);
                rendered.push('\n');
            }
        }
        (Some(before), None) => {
            rendered.push_str("@@ deleted @@\n");
            for line in before.lines() {
                rendered.push('-');
                rendered.push_str(line);
                rendered.push('\n');
            }
        }
        (None, None) => {
            return Err(invalid(format!(
                "changed path `{path}` is absent from both snapshots"
            )));
        }
    }
    Ok(rendered)
}

fn read_bounded_text(path: &Path) -> MedusaResult<Option<String>> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(path)?;
    if bytes.len() > PATCH_TEXT_LIMIT {
        return Ok(Some(format!(
            "[binary-or-large artifact: {} bytes, sha256={:x}]",
            bytes.len(),
            Sha256::digest(&bytes)
        )));
    }
    Ok(Some(match String::from_utf8(bytes.clone()) {
        Ok(text) => text,
        Err(_) => format!(
            "[binary artifact: {} bytes, sha256={:x}]",
            bytes.len(),
            Sha256::digest(&bytes)
        ),
    }))
}

fn remove_path(path: &Path) -> MedusaResult<()> {
    if !path.exists() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn copy_file_atomic(source: &Path, destination: &Path) -> MedusaResult<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("workspace destination has no parent"))?;
    if destination.is_dir() {
        fs::remove_dir_all(destination)?;
    }
    if parent.exists() && !parent.is_dir() {
        remove_path(parent)?;
    }
    fs::create_dir_all(parent)?;
    let temporary = destination.with_extension("medusa-tmp");
    remove_path(&temporary)?;
    fs::copy(source, &temporary)?;
    #[cfg(windows)]
    if destination.exists() {
        fs::remove_file(destination)?;
    }
    fs::rename(temporary, destination)?;
    Ok(())
}

fn restore_paths(
    repo: &Path,
    rollback: &Path,
    paths: &[String],
    existed: &BTreeSet<String>,
) -> MedusaResult<()> {
    let mut removal = paths.to_vec();
    removal.sort_by(|left, right| {
        Path::new(right)
            .components()
            .count()
            .cmp(&Path::new(left).components().count())
            .then_with(|| right.cmp(left))
    });
    for path in removal {
        remove_path(&repo.join(path))?;
    }
    for path in paths {
        if existed.contains(path) {
            copy_file_atomic(&rollback.join(path), &repo.join(path))?;
        }
    }
    Ok(())
}

fn remove_runtime_residue(root: &Path) -> MedusaResult<()> {
    fn visit(directory: &Path) -> MedusaResult<()> {
        let mut entries = fs::read_dir(directory)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let metadata = fs::symlink_metadata(&path)?;
            if name == ".medusa" || name == "__pycache__" || name == ".pytest_cache" {
                if metadata.is_dir() {
                    fs::remove_dir_all(path)?;
                } else {
                    fs::remove_file(path)?;
                }
                continue;
            }
            if metadata.is_dir() {
                visit(&path)?;
            } else if name.ends_with(".pyc") || name.ends_with(".pyo") {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }
    visit(root)
}

fn verify_directory_workspace(root: &Path) -> MedusaResult<String> {
    if root.join("verify.ps1").is_file() && cfg!(windows) {
        return output_result(
            "directory workspace verification",
            hidden_command("powershell.exe")
                .args(["-NoProfile", "-File", "verify.ps1"])
                .current_dir(root)
                .output()?,
        );
    }
    if root.join("verify.sh").is_file() && !cfg!(windows) {
        return output_result(
            "directory workspace verification",
            hidden_command("sh")
                .arg("verify.sh")
                .current_dir(root)
                .output()?,
        );
    }
    if root.join("Cargo.toml").is_file() {
        return output_result(
            "directory workspace Cargo verification",
            hidden_command("cargo")
                .args(["test", "--workspace", "--all-features"])
                .current_dir(root)
                .output()?,
        );
    }
    Ok("directory workspace artifact verification passed; no project-level verification command was declared".to_owned())
}

fn output_result(context: &str, output: Output) -> MedusaResult<String> {
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(execution(format!(
            "{context} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn excluded_workspace_path(path: &str) -> bool {
    matches!(
        path.split('/').next(),
        Some(".git" | ".medusa" | "target" | "node_modules")
    )
}

fn validate_identifier(value: &str, label: &str) -> MedusaResult<()> {
    if value.is_empty()
        || value.len() > 128
        || value.chars().any(|character| {
            !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
        })
    {
        return Err(invalid(format!("invalid {label}: {value}")));
    }
    Ok(())
}

fn next_worker_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    format!(
        "{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn write_json(path: &Path, value: &impl Serialize) -> MedusaResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(value).map_err(|error| invalid(error.to_string()))?,
    )?;
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> MedusaResult<T> {
    serde_json::from_slice(&fs::read(path)?).map_err(|error| invalid(error.to_string()))
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        message,
    )
}

fn policy(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::PolicyDenied, ErrorCategory::Policy, message)
}

fn execution(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_worker_prepares_verifies_and_integrates_without_git() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).expect("workspace");
        fs::write(workspace.join("notes.md"), "# Draft\n").expect("base artifact");
        let manager = WorkspaceWorkerManager::new(
            &workspace,
            workspace.join(".medusa/executions/test/worktrees"),
        )
        .expect("manager");
        assert_eq!(manager.backend(), WorkspaceMutationBackend::Directory);
        let base = manager.repository_head().expect("base");
        let worker = manager
            .open_or_create_worker("docs", "worker-docs")
            .expect("worker");
        fs::write(
            worker.worktree.join("notes.md"),
            "# Final\nVerified documentation.\n",
        )
        .expect("edit");
        let changed = manager
            .changed_components_since(&worker, &base)
            .expect("changes");
        assert_eq!(component_paths(&changed), vec!["notes.md"]);
        let worker = manager
            .finalize_worker(worker, &base, "documentation artifact")
            .expect("snapshot");
        let commit = worker.commit.clone().expect("commit");
        assert!(commit.starts_with(DIRECTORY_REVISION_PREFIX));
        assert!(
            manager
                .commit_patch(&base, &commit)
                .expect("patch")
                .contains("Verified documentation")
        );
        let verification = root.path().join("verification");
        manager
            .materialize_detached_commit(&commit, &verification)
            .expect("verification copy");
        assert_eq!(
            fs::read_to_string(verification.join("notes.md")).expect("verified file"),
            "# Final\nVerified documentation.\n"
        );
        manager
            .remove_detached_worktree(&verification)
            .expect("cleanup verification");
        let receipt = manager
            .integrate_authorized(&worker, &base, &commit)
            .expect("integrate");
        assert_eq!(receipt.integrated_head, commit);
        assert_eq!(
            fs::read_to_string(workspace.join("notes.md")).expect("integrated file"),
            "# Final\nVerified documentation.\n"
        );
        manager.cleanup(&[worker]).expect("worker cleanup");
    }

    #[test]
    fn directory_integration_rejects_primary_drift_before_writing() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).expect("workspace");
        fs::write(workspace.join("report.md"), "base\n").expect("base");
        let manager = WorkspaceWorkerManager::new(
            &workspace,
            workspace.join(".medusa/executions/drift/worktrees"),
        )
        .expect("manager");
        let base = manager.repository_head().expect("base");
        let worker = manager
            .open_or_create_worker("report", "worker-report")
            .expect("worker");
        fs::write(worker.worktree.join("report.md"), "prepared\n").expect("prepare");
        let worker = manager
            .finalize_worker(worker, &base, "report artifact")
            .expect("snapshot");
        let commit = worker.commit.clone().expect("commit");
        fs::write(workspace.join("report.md"), "user edit\n").expect("drift");
        let error = manager
            .integrate_authorized(&worker, &base, &commit)
            .expect_err("drift must reject");
        assert!(error.to_string().contains("drifted before integration"));
        assert_eq!(
            fs::read_to_string(workspace.join("report.md")).expect("primary"),
            "user edit\n"
        );
    }

    #[test]
    fn directory_components_are_normalized_for_package_scope() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        fs::create_dir_all(workspace.join("src")).expect("workspace");
        fs::write(
            workspace.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("manifest");
        fs::write(workspace.join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n").expect("source");
        let manager = WorkspaceWorkerManager::new(
            &workspace,
            workspace.join(".medusa/executions/package-scope/worktrees"),
        )
        .expect("manager");
        let base = manager.repository_head().expect("base");
        let worker = manager
            .open_or_create_worker("package", "worker-package")
            .expect("worker");
        fs::write(
            worker.worktree.join("src/lib.rs"),
            "pub fn value() -> u8 { 2 }\n",
        )
        .expect("edit");
        let changed = manager
            .changed_components_since(&worker, &base)
            .expect("changes");
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].path, "src/lib.rs");
        assert_eq!(changed[0].package_owner.as_deref(), Some("."));
        assert!(changed[0].content_hash.is_some());
    }

    #[test]
    fn nested_git_directory_uses_bounded_directory_backend() {
        let root = tempfile::tempdir().expect("root");
        let repository = root.path().join("repository");
        fs::create_dir(&repository).expect("repository");
        let output = Command::new("git")
            .arg("init")
            .arg(&repository)
            .output()
            .expect("git init");
        assert!(output.status.success());
        let nested = repository.join("nested");
        fs::create_dir(&nested).expect("nested");
        assert!(is_git_repository(&repository));
        assert!(!is_git_repository(&nested));
        let manager = WorkspaceWorkerManager::new(
            &nested,
            nested.join(".medusa/executions/nested/worktrees"),
        )
        .expect("manager");
        assert_eq!(manager.backend(), WorkspaceMutationBackend::Directory);
    }

    #[test]
    fn directory_rollback_restores_file_after_file_to_directory_change() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).expect("workspace");
        fs::write(workspace.join("config"), "original\n").expect("base file");
        let manager = WorkspaceWorkerManager::new(
            &workspace,
            workspace.join(".medusa/executions/type-change/worktrees"),
        )
        .expect("manager");
        let base = manager.repository_head().expect("base");
        let worker = manager
            .open_or_create_worker("type-change", "worker-type-change")
            .expect("worker");
        fs::remove_file(worker.worktree.join("config")).expect("remove file");
        fs::create_dir(worker.worktree.join("config")).expect("create directory");
        fs::write(worker.worktree.join("config/value"), "prepared\n").expect("prepare");
        let worker = manager
            .finalize_worker(worker, &base, "type change")
            .expect("snapshot");
        let commit = worker.commit.clone().expect("commit");
        fs::write(
            manager.snapshot_root(&commit).join("tree/config/value"),
            "tampered\n",
        )
        .expect("tamper snapshot");
        let error = manager
            .integrate_authorized(&worker, &base, &commit)
            .expect_err("tree mismatch must roll back");
        assert!(
            error
                .to_string()
                .contains("does not match authorized snapshot")
        );
        assert!(workspace.join("config").is_file());
        assert_eq!(
            fs::read_to_string(workspace.join("config")).expect("restored file"),
            "original\n"
        );
        assert!(!workspace.join("config/value").exists());
    }
}
