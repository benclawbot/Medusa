use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitMutationPreview {
    pub kind: GitMutationKind,
    pub repository: String,
    pub branch: String,
    pub title: String,
    pub body: Option<String>,
    pub recipients: Vec<String>,
    pub affected_resources: Vec<String>,
    pub destructive: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GitMutationKind {
    Branch,
    Checkpoint,
    Commit,
    Push,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitMutationConfirmation {
    pub preview_fingerprint: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitMutationResult {
    pub branch: String,
    pub commit_sha: String,
    pub checkpoint_ref: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Fingerprint<'a> {
    kind: GitMutationKind,
    repository: &'a str,
    branch: &'a str,
    title: &'a str,
    body: &'a str,
    recipients: Vec<String>,
    affected_resources: Vec<String>,
    destructive: bool,
}

#[tauri::command]
pub fn runtime_create_branch(
    repo: String,
    branch: String,
    preview: GitMutationPreview,
    confirmation: GitMutationConfirmation,
) -> Result<GitMutationResult, String> {
    let repo = canonical_repo(&repo)?;
    require_confirmation(&preview, &confirmation)?;
    require_repository_and_kind(&preview, GitMutationKind::Branch, &repo)?;
    if preview.branch.trim() != branch.trim() {
        return Err("mutation preview branch does not match requested branch".to_owned());
    }
    validate_branch_name(&repo, &branch)?;
    run_git(&repo, ["switch", "-c", branch.as_str()])?;
    mutation_result(&repo, None)
}

#[tauri::command]
pub fn runtime_create_checkpoint(
    repo: String,
    checkpoint_ref: String,
    preview: GitMutationPreview,
    confirmation: GitMutationConfirmation,
) -> Result<GitMutationResult, String> {
    let repo = canonical_repo(&repo)?;
    require_confirmation(&preview, &confirmation)?;
    let branch = current_branch(&repo)?;
    require_active_branch_preview(&preview, GitMutationKind::Checkpoint, &repo, &branch)?;
    let full_ref = format!("refs/medusa/checkpoints/{checkpoint_ref}");
    validate_full_ref(&repo, &full_ref)?;
    run_git(&repo, ["update-ref", full_ref.as_str(), "HEAD"])?;
    mutation_result(&repo, Some(full_ref))
}

#[tauri::command]
pub fn runtime_commit_changes(
    repo: String,
    message: String,
    paths: Vec<String>,
    preview: GitMutationPreview,
    confirmation: GitMutationConfirmation,
) -> Result<GitMutationResult, String> {
    let repo = canonical_repo(&repo)?;
    require_confirmation(&preview, &confirmation)?;
    let branch = current_branch(&repo)?;
    require_active_branch_preview(&preview, GitMutationKind::Commit, &repo, &branch)?;
    let message = message.trim();
    if message.is_empty() {
        return Err("commit message is required".to_owned());
    }
    if paths.is_empty() {
        return Err("at least one explicit path is required for commit".to_owned());
    }
    let normalized = validate_paths(&paths)?;
    let mut add_args = vec!["add".to_owned(), "--".to_owned()];
    add_args.extend(normalized);
    run_git_vec(&repo, &add_args)?;
    run_git(&repo, ["commit", "-m", message])?;
    mutation_result(&repo, None)
}

#[tauri::command]
pub fn runtime_push_branch(
    repo: String,
    remote: Option<String>,
    preview: GitMutationPreview,
    confirmation: GitMutationConfirmation,
) -> Result<GitMutationResult, String> {
    let repo = canonical_repo(&repo)?;
    require_confirmation(&preview, &confirmation)?;
    let branch = current_branch(&repo)?;
    require_active_branch_preview(&preview, GitMutationKind::Push, &repo, &branch)?;
    let remote = remote.as_deref().unwrap_or("origin").trim();
    if remote.is_empty() || remote.starts_with('-') || remote.chars().any(char::is_whitespace) {
        return Err("invalid remote name".to_owned());
    }
    run_git(&repo, ["push", "--set-upstream", remote, branch.as_str()])?;
    mutation_result(&repo, None)
}

fn canonical_repo(repo: &str) -> Result<PathBuf, String> {
    let repo = fs::canonicalize(Path::new(repo))
        .map_err(|error| format!("cannot open {repo}: {error}"))?;
    if !repo.is_dir() {
        return Err(format!("{} is not a directory", repo.display()));
    }
    run_git(&repo, ["rev-parse", "--is-inside-work-tree"])?;
    Ok(repo)
}

fn require_confirmation(
    preview: &GitMutationPreview,
    confirmation: &GitMutationConfirmation,
) -> Result<(), String> {
    validate_preview(preview)?;
    let expected = mutation_fingerprint(preview)?;
    if confirmation.preview_fingerprint != expected {
        return Err("mutation confirmation does not match the active preview".to_owned());
    }
    Ok(())
}

fn require_repository_and_kind(
    preview: &GitMutationPreview,
    expected_kind: GitMutationKind,
    repo: &Path,
) -> Result<(), String> {
    if std::mem::discriminant(&preview.kind) != std::mem::discriminant(&expected_kind) {
        return Err("mutation preview kind does not match requested operation".to_owned());
    }
    let canonical_preview_repo = fs::canonicalize(preview.repository.trim())
        .map_err(|error| format!("cannot open preview repository: {error}"))?;
    if canonical_preview_repo != repo {
        return Err("mutation preview repository does not match requested repository".to_owned());
    }
    Ok(())
}

fn require_active_branch_preview(
    preview: &GitMutationPreview,
    expected_kind: GitMutationKind,
    repo: &Path,
    branch: &str,
) -> Result<(), String> {
    require_repository_and_kind(preview, expected_kind, repo)?;
    if preview.branch.trim() != branch {
        return Err("mutation preview branch does not match active branch".to_owned());
    }
    Ok(())
}

fn validate_preview(preview: &GitMutationPreview) -> Result<(), String> {
    if preview.repository.trim().is_empty() {
        return Err("preview repository is required".to_owned());
    }
    if preview.branch.trim().is_empty() {
        return Err("preview branch is required".to_owned());
    }
    if preview.title.trim().is_empty() {
        return Err("preview title is required".to_owned());
    }
    if preview.affected_resources.is_empty()
        || preview
            .affected_resources
            .iter()
            .any(|value| value.trim().is_empty())
    {
        return Err("preview affected resources are required".to_owned());
    }
    Ok(())
}

fn mutation_fingerprint(preview: &GitMutationPreview) -> Result<String, String> {
    let mut recipients = preview
        .recipients
        .iter()
        .map(|value| value.trim().to_owned())
        .collect::<Vec<_>>();
    recipients.sort();
    let mut affected_resources = preview
        .affected_resources
        .iter()
        .map(|value| value.trim().to_owned())
        .collect::<Vec<_>>();
    affected_resources.sort();
    serde_json::to_string(&Fingerprint {
        kind: preview.kind,
        repository: preview.repository.trim(),
        branch: preview.branch.trim(),
        title: preview.title.trim(),
        body: preview.body.as_deref().unwrap_or("").trim(),
        recipients,
        affected_resources,
        destructive: preview.destructive,
    })
    .map_err(|error| format!("cannot encode mutation preview: {error}"))
}

fn current_branch(repo: &Path) -> Result<String, String> {
    let output = run_git(repo, ["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let branch = String::from_utf8(output.stdout)
        .map_err(|_| "git branch output was not UTF-8".to_owned())?
        .trim()
        .to_owned();
    if branch.is_empty() {
        return Err("repository is in detached HEAD state".to_owned());
    }
    Ok(branch)
}

fn validate_branch_name(repo: &Path, name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.starts_with('-') {
        return Err("invalid branch name".to_owned());
    }
    run_git(repo, ["check-ref-format", "--branch", name])?;
    Ok(())
}

fn validate_full_ref(repo: &Path, name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.starts_with('-') {
        return Err("invalid git reference name".to_owned());
    }
    run_git(repo, ["check-ref-format", name])?;
    Ok(())
}

fn validate_paths(paths: &[String]) -> Result<Vec<String>, String> {
    paths
        .iter()
        .map(|path| {
            let path = path.trim();
            if path.is_empty()
                || Path::new(path).is_absolute()
                || path.split('/').any(|part| part == "..")
                || path.starts_with('-')
            {
                return Err(format!("invalid repository-relative path: {path}"));
            }
            Ok(path.to_owned())
        })
        .collect()
}

fn mutation_result(
    repo: &Path,
    checkpoint_ref: Option<String>,
) -> Result<GitMutationResult, String> {
    let branch = current_branch(repo)?;
    let output = run_git(repo, ["rev-parse", "HEAD"])?;
    let commit_sha = String::from_utf8(output.stdout)
        .map_err(|_| "git revision output was not UTF-8".to_owned())?
        .trim()
        .to_owned();
    Ok(GitMutationResult {
        branch,
        commit_sha,
        checkpoint_ref,
    })
}

fn run_git<const N: usize>(repo: &Path, args: [&str; N]) -> Result<Output, String> {
    let owned = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
    run_git_vec(repo, &owned)
}

fn run_git_vec(repo: &Path, args: &[String]) -> Result<Output, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    if !output.status.success() {
        let details = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(format!("git {} failed: {details}", args.join(" ")));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview() -> GitMutationPreview {
        GitMutationPreview {
            kind: GitMutationKind::Commit,
            repository: "/repo".to_owned(),
            branch: "feature/safe".to_owned(),
            title: "Commit verified changes".to_owned(),
            body: None,
            recipients: vec!["reviewers".to_owned()],
            affected_resources: vec!["file:src/main.rs".to_owned()],
            destructive: false,
        }
    }

    #[test]
    fn fingerprint_matches_frontend_normalization() {
        let fingerprint = mutation_fingerprint(&preview()).expect("fingerprint");
        assert_eq!(
            fingerprint,
            r#"{"kind":"commit","repository":"/repo","branch":"feature/safe","title":"Commit verified changes","body":"","recipients":["reviewers"],"affectedResources":["file:src/main.rs"],"destructive":false}"#
        );
    }

    #[test]
    fn confirmation_rejects_changed_preview() {
        let mut value = preview();
        let confirmation = GitMutationConfirmation {
            preview_fingerprint: mutation_fingerprint(&value).expect("fingerprint"),
        };
        value.title = "Changed after confirmation".to_owned();
        assert!(require_confirmation(&value, &confirmation).is_err());
    }

    #[test]
    fn path_validation_allows_deleted_files_but_rejects_escape_paths() {
        assert_eq!(
            validate_paths(&["src/deleted.rs".to_owned()]).expect("valid path"),
            vec!["src/deleted.rs"]
        );
        assert!(validate_paths(&["/etc/passwd".to_owned()]).is_err());
        assert!(validate_paths(&["../secret".to_owned()]).is_err());
    }
}
