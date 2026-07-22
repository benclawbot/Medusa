use std::{fs, path::{Path, PathBuf}, process::{Command, Output}};

use serde::Serialize;

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GithubRepositoryTransferState {
    Ready,
    GhUnavailable,
    AuthenticationRequired,
    NotFound,
    Forbidden,
    InvalidTarget,
    DirtyWorktree,
    Failed,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubRepositoryTransferResult {
    pub state: GithubRepositoryTransferState,
    pub repository: String,
    pub path: String,
    pub operation: String,
    pub message: String,
}

#[tauri::command]
pub fn runtime_clone_github_repository(
    repository: String,
    destination: String,
    hostname: Option<String>,
) -> GithubRepositoryTransferResult {
    let repository = repository.trim().to_owned();
    let hostname = normalize_hostname(hostname);
    let destination = PathBuf::from(destination.trim());

    if !valid_repository(&repository) || !valid_hostname(&hostname) {
        return result(
            GithubRepositoryTransferState::InvalidTarget,
            repository,
            &destination,
            "clone",
            "invalid GitHub repository or hostname",
        );
    }
    if let Err(message) = validate_clone_destination(&destination) {
        return result(
            GithubRepositoryTransferState::InvalidTarget,
            repository,
            &destination,
            "clone",
            &message,
        );
    }
    if let Err(state) = verify_access(&repository, &hostname) {
        return result(
            state,
            repository,
            &destination,
            "clone",
            "GitHub repository access is not ready",
        );
    }

    let output = match Command::new("gh")
        .args([
            "repo",
            "clone",
            repository.as_str(),
            destination.to_string_lossy().as_ref(),
            "--",
            "--origin",
            "origin",
        ])
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return result(
                GithubRepositoryTransferState::GhUnavailable,
                repository,
                &destination,
                "clone",
                "GitHub CLI is not installed",
            );
        }
        Err(_) => {
            return result(
                GithubRepositoryTransferState::Failed,
                repository,
                &destination,
                "clone",
                "cannot clone GitHub repository",
            );
        }
    };
    if !output.status.success() {
        let state = classify_failure(&String::from_utf8_lossy(&output.stderr));
        return result(
            state,
            repository,
            &destination,
            "clone",
            "GitHub repository clone failed",
        );
    }

    result(
        GithubRepositoryTransferState::Ready,
        repository,
        &destination,
        "clone",
        "GitHub repository cloned",
    )
}

#[tauri::command]
pub fn runtime_fetch_github_repository(
    repository: String,
    local_path: String,
    hostname: Option<String>,
) -> GithubRepositoryTransferResult {
    let repository = repository.trim().to_owned();
    let hostname = normalize_hostname(hostname);
    let local_path = PathBuf::from(local_path.trim());

    if !valid_repository(&repository) || !valid_hostname(&hostname) {
        return result(
            GithubRepositoryTransferState::InvalidTarget,
            repository,
            &local_path,
            "fetch",
            "invalid GitHub repository or hostname",
        );
    }
    let local_path = match canonical_repository(&local_path) {
        Ok(path) => path,
        Err(message) => {
            return result(
                GithubRepositoryTransferState::InvalidTarget,
                repository,
                &local_path,
                "fetch",
                &message,
            );
        }
    };
    if let Err(state) = verify_access(&repository, &hostname) {
        return result(
            state,
            repository,
            &local_path,
            "fetch",
            "GitHub repository access is not ready",
        );
    }
    if has_in_progress_git_operation(&local_path) {
        return result(
            GithubRepositoryTransferState::DirtyWorktree,
            repository,
            &local_path,
            "fetch",
            "repository has an in-progress merge, rebase, or cherry-pick",
        );
    }

    let output = match Command::new("git")
        .current_dir(&local_path)
        .args(["fetch", "--prune", "--no-tags", "origin"])
        .output()
    {
        Ok(output) => output,
        Err(_) => {
            return result(
                GithubRepositoryTransferState::Failed,
                repository,
                &local_path,
                "fetch",
                "cannot fetch GitHub repository",
            );
        }
    };
    if !output.status.success() {
        return result(
            classify_failure(&String::from_utf8_lossy(&output.stderr)),
            repository,
            &local_path,
            "fetch",
            "GitHub repository fetch failed",
        );
    }

    result(
        GithubRepositoryTransferState::Ready,
        repository,
        &local_path,
        "fetch",
        "GitHub repository fetched",
    )
}

fn normalize_hostname(hostname: Option<String>) -> String {
    hostname.as_deref().unwrap_or("github.com").trim().to_owned()
}

fn verify_access(repository: &str, hostname: &str) -> Result<(), GithubRepositoryTransferState> {
    let output = Command::new("gh")
        .args(["repo", "view", repository, "--hostname", hostname, "--json", "nameWithOwner"])
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                GithubRepositoryTransferState::GhUnavailable
            } else {
                GithubRepositoryTransferState::Failed
            }
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(classify_failure(&String::from_utf8_lossy(&output.stderr)))
    }
}

fn classify_failure(stderr: &str) -> GithubRepositoryTransferState {
    let value = stderr.to_ascii_lowercase();
    if value.contains("not logged") || value.contains("authentication") || value.contains("bad credentials") {
        GithubRepositoryTransferState::AuthenticationRequired
    } else if value.contains("not found") || value.contains("404") {
        GithubRepositoryTransferState::NotFound
    } else if value.contains("forbidden") || value.contains("403") || value.contains("permission denied") {
        GithubRepositoryTransferState::Forbidden
    } else {
        GithubRepositoryTransferState::Failed
    }
}

fn validate_clone_destination(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err("clone destination is required".to_owned());
    }
    if path.exists() {
        if !path.is_dir() {
            return Err("clone destination exists and is not a directory".to_owned());
        }
        if fs::read_dir(path)
            .map_err(|_| "cannot inspect clone destination".to_owned())?
            .next()
            .is_some()
        {
            return Err("clone destination must be empty".to_owned());
        }
    } else {
        let parent = path.parent().ok_or_else(|| "clone destination parent is required".to_owned())?;
        fs::canonicalize(parent).map_err(|_| "clone destination parent does not exist".to_owned())?;
    }
    Ok(())
}

fn canonical_repository(path: &Path) -> Result<PathBuf, String> {
    let path = fs::canonicalize(path).map_err(|_| "local repository path does not exist".to_owned())?;
    if !path.is_dir() {
        return Err("local repository path is not a directory".to_owned());
    }
    let output = Command::new("git")
        .current_dir(&path)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map_err(|_| "cannot inspect local repository".to_owned())?;
    if !output.status.success() {
        return Err("local path is not a Git repository".to_owned());
    }
    Ok(path)
}

fn has_in_progress_git_operation(path: &Path) -> bool {
    let git_dir = Command::new("git")
        .current_dir(path)
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()
        .filter(Output::status_success)
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| path.join(value.trim()));
    let Some(git_dir) = git_dir else { return true; };
    ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REBASE_HEAD", "rebase-merge", "rebase-apply"]
        .iter()
        .any(|name| git_dir.join(name).exists())
}

trait OutputStatus {
    fn status_success(&self) -> bool;
}
impl OutputStatus for Output {
    fn status_success(&self) -> bool { self.status.success() }
}

fn valid_hostname(value: &str) -> bool {
    !value.is_empty() && !value.starts_with('-') && !value.chars().any(char::is_whitespace)
}

fn valid_repository(value: &str) -> bool {
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    !owner.is_empty()
        && !name.is_empty()
        && parts.next().is_none()
        && !owner.starts_with('-')
        && !name.starts_with('-')
        && owner.chars().all(valid_repository_character)
        && name.chars().all(valid_repository_character)
}

fn valid_repository_character(value: char) -> bool {
    value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.')
}

fn result(
    state: GithubRepositoryTransferState,
    repository: String,
    path: &Path,
    operation: &str,
    message: &str,
) -> GithubRepositoryTransferResult {
    GithubRepositoryTransferResult {
        state,
        repository,
        path: path.to_string_lossy().into_owned(),
        operation: operation.to_owned(),
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_auth_and_permission_failures_without_returning_secret_text() {
        assert_eq!(classify_failure("HTTP 401 bad credentials token ghp_secret"), GithubRepositoryTransferState::AuthenticationRequired);
        assert_eq!(classify_failure("HTTP 403 permission denied"), GithubRepositoryTransferState::Forbidden);
        assert_eq!(classify_failure("HTTP 404 not found"), GithubRepositoryTransferState::NotFound);
    }

    #[test]
    fn validates_repository_and_hostname() {
        assert!(valid_repository("octo/private-repo"));
        assert!(!valid_repository("octo/../secret"));
        assert!(valid_hostname("github.com"));
        assert!(!valid_hostname("-bad host"));
    }
}
