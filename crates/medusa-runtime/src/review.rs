use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use medusa_review_model::{
    ChangeKind, ChangeOrigin, ReviewActionRequest, ReviewAuditExport, ReviewFile, ReviewFilter,
    ReviewHistoryError, ReviewHunk, ReviewProvenance, ReviewSessionHistory, ReviewSnapshot,
    ReviewState, VerificationState, record_authorized_action,
};
use medusa_core::hidden_command;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const REVIEW_DIR: &str = ".medusa/review";
const BASELINE_FILE: &str = "baseline.json";
const STATE_FILE: &str = "state.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewWorkspace {
    pub snapshot: ReviewSnapshot,
    pub files: Vec<ReviewDiffFile>,
    pub completion: medusa_review_model::CompletionState,
    pub history: ReviewSessionHistory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewDiffFile {
    pub path: String,
    pub previous_path: Option<String>,
    pub patch: String,
    pub hunks: Vec<ReviewDiffHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewDiffHunk {
    pub id: String,
    pub header: String,
    pub patch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReviewBaseline {
    repository_fingerprint: String,
    paths: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PersistedReviewState {
    snapshot: ReviewSnapshot,
    history: ReviewSessionHistory,
}

#[derive(Debug, Error)]
pub enum ReviewWorkflowError {
    #[error("review repository is invalid: {0}")]
    InvalidRepository(String),
    #[error("git operation failed: {0}")]
    Git(String),
    #[error("review state is stale; refresh before applying this action")]
    StaleState,
    #[error("review state is unavailable: {0}")]
    State(String),
    #[error("review action was rejected: {0}")]
    Rejected(String),
    #[error("review history failed: {0}")]
    History(#[from] ReviewHistoryError),
}

pub fn capture_review_baseline(repo: &Path) -> Result<(), ReviewWorkflowError> {
    let repo = fs::canonicalize(repo)
        .map_err(|error| ReviewWorkflowError::InvalidRepository(error.to_string()))?;
    let path = repo.join(REVIEW_DIR).join(BASELINE_FILE);
    // Rotate the baseline before every new task so only changes present before that task
    // are classified as pre-existing user work. Pending-question resumes skip this call.
    if path.exists() {
        fs::remove_file(&path).map_err(|error| ReviewWorkflowError::State(error.to_string()))?;
    }

    let baseline = if is_git_work_tree(&repo) {
        ReviewBaseline {
            repository_fingerprint: repository_fingerprint(&repo)?,
            paths: changed_paths(&repo)?.into_iter().collect(),
        }
    } else {
        ReviewBaseline {
            repository_fingerprint: fingerprint(b""),
            paths: BTreeSet::new(),
        }
    };
    write_json(&path, &baseline)
}

pub fn read_review_workspace(repo: &Path) -> Result<ReviewWorkspace, ReviewWorkflowError> {
    let repo = canonical_repo(repo)?;
    let baseline = read_baseline(&repo)?;
    let source = git_diff(&repo)?;
    let parsed = parse_diff(&source)?;
    let repository_fingerprint = fingerprint(source.as_bytes());
    let snapshot_id = fingerprint(format!("{repository_fingerprint}:{}", now_unix_ms()).as_bytes());
    let persisted = read_state(&repo).ok();
    let prior_states = persisted
        .as_ref()
        .map(|state| {
            state
                .snapshot
                .files
                .iter()
                .map(|file| {
                    (
                        file.path.clone(),
                        (file.current_fingerprint.clone(), file.review_state),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    let mut files = Vec::with_capacity(parsed.len());
    let mut diff_files = Vec::with_capacity(parsed.len());
    for file in parsed {
        let path = file.path.clone();
        let origin = if generated_path(&path) {
            ChangeOrigin::Generated
        } else if baseline.paths.contains(&path)
            || file
                .previous_path
                .as_ref()
                .is_some_and(|previous| baseline.paths.contains(previous))
        {
            ChangeOrigin::PreExistingUser
        } else {
            ChangeOrigin::Medusa
        };
        let current_fingerprint = fingerprint(file.patch.as_bytes());
        let review_hunks = file
            .hunks
            .iter()
            .map(|hunk| ReviewHunk {
                id: hunk.id.clone(),
                base_fingerprint: hunk.id.clone(),
                current_fingerprint: hunk.id.clone(),
                ambiguous: false,
                // Without per-write provenance, a tracked-file hunk can contain edits made
                // by the user while the task was running. Fail closed instead of allowing a
                // destructive selective revert. Newly added files remain safely revertible.
                overlaps_later_edits: file.kind != ChangeKind::Added,
                review_state: ReviewState::Unreviewed,
                provenance: ReviewProvenance {
                    task_step_id: None,
                    tool_execution_id: None,
                    rationale: Some("captured from the runtime working-tree review".to_owned()),
                    verification_event_ids: Vec::new(),
                },
            })
            .collect();
        files.push(ReviewFile {
            path: path.clone(),
            previous_path: file.previous_path.clone(),
            kind: file.kind,
            origin,
            binary: file.binary,
            policy_sensitive: policy_sensitive_path(&path),
            verification: VerificationState::Unverified,
            review_state: prior_states
                .get(&path)
                .filter(|(fingerprint, _)| fingerprint == &current_fingerprint)
                .map(|(_, state)| *state)
                .unwrap_or(ReviewState::Unreviewed),
            snapshot_fingerprint: current_fingerprint.clone(),
            current_fingerprint,
            hunks: review_hunks,
            provenance: ReviewProvenance {
                task_step_id: None,
                tool_execution_id: None,
                rationale: Some("captured from the runtime working-tree review".to_owned()),
                verification_event_ids: Vec::new(),
            },
        });
        diff_files.push(ReviewDiffFile {
            path,
            previous_path: file.previous_path,
            patch: file.patch,
            hunks: file.hunks,
        });
    }

    let snapshot = ReviewSnapshot {
        id: snapshot_id,
        repository_fingerprint,
        created_at_unix_ms: now_unix_ms(),
        files,
    };
    let history = persisted
        .map(|state| state.history)
        .unwrap_or(ReviewSessionHistory::new("working-tree-review")?);
    let completion = snapshot.completion_state();
    let state = PersistedReviewState {
        snapshot: snapshot.clone(),
        history: history.clone(),
    };
    write_json(&repo.join(REVIEW_DIR).join(STATE_FILE), &state)?;
    Ok(ReviewWorkspace {
        snapshot,
        files: diff_files,
        completion,
        history,
    })
}

pub fn apply_review_action(
    repo: &Path,
    request: ReviewActionRequest,
    actor: &str,
) -> Result<ReviewWorkspace, ReviewWorkflowError> {
    let repo = canonical_repo(repo)?;
    let mut state = read_state(&repo)?;
    let current = git_diff(&repo)?;
    if fingerprint(current.as_bytes()) != state.snapshot.repository_fingerprint {
        return Err(ReviewWorkflowError::StaleState);
    }
    if let ReviewActionRequest::RevertFile { path, .. } = &request {
        let file = state.snapshot.file(path).ok_or_else(|| {
            ReviewWorkflowError::Rejected(
                "changed file is not present in the review snapshot".to_owned(),
            )
        })?;
        if file
            .hunks
            .iter()
            .any(|hunk| hunk.ambiguous || hunk.overlaps_later_edits)
        {
            return Err(ReviewWorkflowError::Rejected(
                "whole-file revert is unsafe because tracked-file write provenance is ambiguous"
                    .to_owned(),
            ));
        }
    }
    let authorized = state
        .snapshot
        .authorize(request.clone())
        .map_err(|error| ReviewWorkflowError::Rejected(error.to_string()))?;

    // Validate the state transition on a clone before touching the worktree.
    // This prevents an error such as Accepted -> Reverted from being reported only
    // after a destructive mutation has already happened.
    let mut validation_snapshot = state.snapshot.clone();
    record_authorized_action(
        &mut validation_snapshot,
        authorized.clone(),
        actor,
        now_unix_ms(),
        state.snapshot.repository_fingerprint.clone(),
    )
    .map_err(|error| ReviewWorkflowError::Rejected(error.to_string()))?;

    match &request {
        ReviewActionRequest::RevertFile { path, .. } => revert_file(&repo, path)?,
        ReviewActionRequest::RevertHunk { path, hunk_id, .. } => {
            let workspace = read_review_workspace(&repo)?;
            let hunk = workspace
                .files
                .iter()
                .find(|file| &file.path == path)
                .and_then(|file| file.hunks.iter().find(|hunk| &hunk.id == hunk_id))
                .ok_or_else(|| ReviewWorkflowError::State("review hunk is missing".to_owned()))?;
            reverse_patch(&repo, &hunk.patch)?;
        }
        ReviewActionRequest::AcceptFile { .. } | ReviewActionRequest::AcceptTask { .. } => {}
    }

    let after = git_diff(&repo)?;
    let event = record_authorized_action(
        &mut state.snapshot,
        authorized.clone(),
        actor,
        now_unix_ms(),
        fingerprint(after.as_bytes()),
    )
    .map_err(|error| ReviewWorkflowError::Rejected(error.to_string()))?;
    state.history.append(event)?;
    write_json(&repo.join(REVIEW_DIR).join(STATE_FILE), &state)?;
    read_review_workspace(&repo)
}

pub fn export_review_audit(
    repo: &Path,
    generated_at_unix_ms: i64,
) -> Result<ReviewAuditExport, ReviewWorkflowError> {
    let state = read_state(&canonical_repo(repo)?)?;
    let mut export = state.history.export(generated_at_unix_ms)?;
    export.resulting_repository_fingerprint = state
        .history
        .events
        .last()
        .map(|event| event.repository_fingerprint_after.clone());
    Ok(export)
}

pub fn filtered_paths(workspace: &ReviewWorkspace, filter: &ReviewFilter) -> Vec<String> {
    workspace
        .snapshot
        .filtered(filter)
        .into_iter()
        .map(|file| file.path.clone())
        .collect()
}

fn revert_file(repo: &Path, path: &str) -> Result<(), ReviewWorkflowError> {
    let tracked = hidden_command("git")
        .args(["ls-files", "--error-unmatch", "--", path])
        .current_dir(repo)
        .output()
        .map_err(|error| ReviewWorkflowError::Git(error.to_string()))?
        .status
        .success();
    if tracked && git_has_head(repo) {
        return git_status(
            hidden_command("git")
                .args(["checkout", "HEAD", "--", path])
                .current_dir(repo),
        );
    }
    if tracked {
        git_status(
            hidden_command("git")
                .args(["rm", "--cached", "--force", "--ignore-unmatch", "--", path])
                .current_dir(repo),
        )?;
    }
    remove_worktree_path(repo, path)
}

fn remove_worktree_path(repo: &Path, path: &str) -> Result<(), ReviewWorkflowError> {
    let target = repo.join(path);
    if target.is_dir() {
        fs::remove_dir_all(target).map_err(|error| ReviewWorkflowError::Git(error.to_string()))?;
    } else if target.exists() {
        fs::remove_file(target).map_err(|error| ReviewWorkflowError::Git(error.to_string()))?;
    }
    Ok(())
}

fn reverse_patch(repo: &Path, patch: &str) -> Result<(), ReviewWorkflowError> {
    let mut child = hidden_command("git")
        .args([
            "apply",
            "--reverse",
            "--recount",
            "--whitespace=nowarn",
            "-",
        ])
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| ReviewWorkflowError::Git(error.to_string()))?;
    use std::io::Write;
    child
        .stdin
        .take()
        .ok_or_else(|| ReviewWorkflowError::Git("cannot open git apply stdin".to_owned()))?
        .write_all(patch.as_bytes())
        .map_err(|error| ReviewWorkflowError::Git(error.to_string()))?;
    let output = child
        .wait_with_output()
        .map_err(|error| ReviewWorkflowError::Git(error.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(ReviewWorkflowError::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

fn canonical_repo(repo: &Path) -> Result<PathBuf, ReviewWorkflowError> {
    let path = fs::canonicalize(repo)
        .map_err(|error| ReviewWorkflowError::InvalidRepository(error.to_string()))?;
    if path.join(".git").exists() {
        Ok(path)
    } else {
        Err(ReviewWorkflowError::InvalidRepository(
            "repository does not contain .git".to_owned(),
        ))
    }
}

fn read_baseline(repo: &Path) -> Result<ReviewBaseline, ReviewWorkflowError> {
    let path = repo.join(REVIEW_DIR).join(BASELINE_FILE);
    if !path.exists() {
        capture_review_baseline(repo)?;
    }
    read_json(&path)
}

fn read_state(repo: &Path) -> Result<PersistedReviewState, ReviewWorkflowError> {
    read_json(&repo.join(REVIEW_DIR).join(STATE_FILE))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, ReviewWorkflowError> {
    let bytes = fs::read(path).map_err(|error| ReviewWorkflowError::State(error.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|error| ReviewWorkflowError::State(error.to_string()))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), ReviewWorkflowError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| ReviewWorkflowError::State(error.to_string()))?;
    }
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| ReviewWorkflowError::State(error.to_string()))?;
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes).map_err(|error| ReviewWorkflowError::State(error.to_string()))?;
    fs::rename(temporary, path).map_err(|error| ReviewWorkflowError::State(error.to_string()))
}

fn repository_fingerprint(repo: &Path) -> Result<String, ReviewWorkflowError> {
    Ok(fingerprint(git_diff(repo)?.as_bytes()))
}

fn changed_paths(repo: &Path) -> Result<Vec<String>, ReviewWorkflowError> {
    let mut paths = if git_has_head(repo) {
        let output = hidden_command("git")
            .args(["diff", "--name-only", "HEAD", "--"])
            .current_dir(repo)
            .output()
            .map_err(|error| ReviewWorkflowError::Git(error.to_string()))?;
        if !output.status.success() {
            return Err(ReviewWorkflowError::Git(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        let mut paths = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        paths.extend(untracked_paths(repo)?);
        paths
    } else {
        current_worktree_paths(repo)?
    };
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn git_diff(repo: &Path) -> Result<String, ReviewWorkflowError> {
    let has_head = git_has_head(repo);
    let mut source = if has_head {
        let output = hidden_command("git")
            .args([
                "diff",
                "--no-ext-diff",
                "--binary",
                "--find-renames",
                "--unified=3",
                "HEAD",
                "--",
            ])
            .current_dir(repo)
            .output()
            .map_err(|error| ReviewWorkflowError::Git(error.to_string()))?;
        if !output.status.success() {
            return Err(ReviewWorkflowError::Git(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        String::from_utf8(output.stdout)
            .map_err(|_| ReviewWorkflowError::Git("git diff output was not UTF-8".to_owned()))?
    } else {
        String::new()
    };

    let added_paths = if has_head {
        untracked_paths(repo)?
    } else {
        current_worktree_paths(repo)?
    };
    for path in added_paths {
        append_added_file_diff(repo, &path, &mut source)?;
    }
    Ok(source)
}

fn append_added_file_diff(
    repo: &Path,
    path: &str,
    source: &mut String,
) -> Result<(), ReviewWorkflowError> {
    let output = hidden_command("git")
        .args([
            "diff",
            "--no-index",
            "--binary",
            "--unified=3",
            "--",
            null_device(),
            path,
        ])
        .current_dir(repo)
        .output()
        .map_err(|error| ReviewWorkflowError::Git(error.to_string()))?;
    if !matches!(output.status.code(), Some(0 | 1)) {
        return Err(ReviewWorkflowError::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let patch = String::from_utf8(output.stdout)
        .map_err(|_| ReviewWorkflowError::Git("untracked diff was not UTF-8".to_owned()))?;
    source.push_str(&patch);
    Ok(())
}

fn is_git_work_tree(repo: &Path) -> bool {
    let Ok(repo) = repo.canonicalize() else {
        return false;
    };
    let Ok(output) = hidden_command("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&repo)
        .output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }

    let Ok(discovered_root) = String::from_utf8(output.stdout) else {
        return false;
    };
    let Ok(discovered_root) = Path::new(discovered_root.trim()).canonicalize() else {
        return false;
    };
    discovered_root == repo
}

fn git_has_head(repo: &Path) -> bool {
    hidden_command("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(repo)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn current_worktree_paths(repo: &Path) -> Result<Vec<String>, ReviewWorkflowError> {
    let output = hidden_command("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .current_dir(repo)
        .output()
        .map_err(|error| ReviewWorkflowError::Git(error.to_string()))?;
    if !output.status.success() {
        return Err(ReviewWorkflowError::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|path| !review_metadata_path(path))
        .filter(|path| {
            fs::symlink_metadata(repo.join(path)).is_ok_and(|metadata| !metadata.is_dir())
        })
        .map(str::to_owned)
        .collect())
}

fn untracked_paths(repo: &Path) -> Result<Vec<String>, ReviewWorkflowError> {
    let output = hidden_command("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(repo)
        .output()
        .map_err(|error| ReviewWorkflowError::Git(error.to_string()))?;
    if !output.status.success() {
        return Err(ReviewWorkflowError::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|path| !review_metadata_path(path))
        .map(str::to_owned)
        .collect())
}

fn review_metadata_path(path: &str) -> bool {
    path.strip_prefix(REVIEW_DIR)
        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('/'))
}

#[cfg(windows)]
fn null_device() -> &'static str {
    "NUL"
}

#[cfg(not(windows))]
fn null_device() -> &'static str {
    "/dev/null"
}

fn git_status(command: &mut Command) -> Result<(), ReviewWorkflowError> {
    let output = command
        .output()
        .map_err(|error| ReviewWorkflowError::Git(error.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(ReviewWorkflowError::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

#[derive(Debug)]
struct ParsedFile {
    path: String,
    previous_path: Option<String>,
    kind: ChangeKind,
    binary: bool,
    patch: String,
    hunks: Vec<ReviewDiffHunk>,
}

fn parse_diff(source: &str) -> Result<Vec<ParsedFile>, ReviewWorkflowError> {
    let mut files = Vec::new();
    let mut chunks = source
        .split("diff --git ")
        .filter(|chunk| !chunk.trim().is_empty());
    for chunk in chunks.by_ref() {
        let patch = format!("diff --git {chunk}");
        let header = patch.lines().next().unwrap_or_default();
        let paths = header
            .strip_prefix("diff --git a/")
            .and_then(|paths| paths.split_once(" b/"))
            .ok_or_else(|| ReviewWorkflowError::Git(format!("invalid diff header: {header}")))?;
        let mut old_path = paths.0.to_owned();
        let mut new_path = paths.1.to_owned();
        let mut kind = ChangeKind::Modified;
        let mut binary = false;
        for line in patch.lines() {
            if line.starts_with("new file mode ") {
                kind = ChangeKind::Added;
            } else if line.starts_with("deleted file mode ") {
                kind = ChangeKind::Deleted;
            } else if let Some(value) = line.strip_prefix("rename from ") {
                old_path = value.to_owned();
                kind = ChangeKind::Renamed;
            } else if let Some(value) = line.strip_prefix("rename to ") {
                new_path = value.to_owned();
                kind = ChangeKind::Renamed;
            } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
                binary = true;
            }
        }
        let path = if kind == ChangeKind::Deleted {
            old_path.clone()
        } else {
            new_path.clone()
        };
        let previous_path = (kind == ChangeKind::Renamed).then_some(old_path);
        let hunks = parse_hunks(&patch, &path);
        files.push(ParsedFile {
            path,
            previous_path,
            kind,
            binary,
            patch,
            hunks,
        });
    }
    Ok(files)
}

fn parse_hunks(file_patch: &str, path: &str) -> Vec<ReviewDiffHunk> {
    let prefix = file_patch
        .lines()
        .take_while(|line| !line.starts_with("@@ "))
        .collect::<Vec<_>>()
        .join("\n");
    let mut hunks = Vec::new();
    let mut current = Vec::new();
    let mut started = false;
    for line in file_patch.lines() {
        if line.starts_with("@@ ") {
            if !current.is_empty() {
                push_hunk(&mut hunks, path, &prefix, &current);
                current.clear();
            }
            started = true;
        }
        if started {
            current.push(line);
        }
    }
    if !current.is_empty() {
        push_hunk(&mut hunks, path, &prefix, &current);
    }
    hunks
}

fn push_hunk(hunks: &mut Vec<ReviewDiffHunk>, path: &str, prefix: &str, lines: &[&str]) {
    let body = lines.join("\n");
    let id = fingerprint(format!("{path}\n{body}").as_bytes());
    hunks.push(ReviewDiffHunk {
        id,
        header: lines.first().copied().unwrap_or_default().to_owned(),
        patch: format!("{prefix}\n{body}\n"),
    });
}

fn generated_path(path: &str) -> bool {
    [
        "target/",
        "dist/",
        "build/",
        "node_modules/",
        ".generated/",
        "generated/",
    ]
    .iter()
    .any(|prefix| path.starts_with(prefix))
        || path.ends_with(".lock")
        || path.contains("generated")
}

fn policy_sensitive_path(path: &str) -> bool {
    path.starts_with(".github/")
        || path.ends_with("Cargo.toml")
        || path.ends_with("package.json")
        || path.contains("security")
        || path.contains("auth")
        || path.contains("permission")
}

fn fingerprint(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "review_tests.rs"]
mod tests;
