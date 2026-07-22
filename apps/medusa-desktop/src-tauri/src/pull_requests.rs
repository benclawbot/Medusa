use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum PullRequestMutationKind {
    PullRequest,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestMutationPreview {
    kind: PullRequestMutationKind,
    repository: String,
    branch: String,
    title: String,
    body: Option<String>,
    recipients: Vec<String>,
    affected_resources: Vec<String>,
    destructive: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestMutationConfirmation {
    preview_fingerprint: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftPullRequestResult {
    pub branch: String,
    pub commit_sha: String,
    pub pull_request_url: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Fingerprint<'a> {
    kind: PullRequestMutationKind,
    repository: &'a str,
    branch: &'a str,
    title: &'a str,
    body: &'a str,
    recipients: Vec<String>,
    affected_resources: Vec<String>,
    destructive: bool,
}

#[tauri::command]
pub fn runtime_create_draft_pull_request(
    repo: String,
    base: String,
    preview: PullRequestMutationPreview,
    confirmation: PullRequestMutationConfirmation,
) -> Result<DraftPullRequestResult, String> {
    let repo = canonical_repo(&repo)?;
    validate_preview(&preview, &confirmation, &repo)?;
    let branch = current_branch(&repo)?;
    if preview.branch.trim() != branch {
        return Err("mutation preview branch does not match active branch".to_owned());
    }
    let body = preview.body.as_deref().unwrap_or("").trim();
    if body.is_empty() {
        return Err("pull request body is required".to_owned());
    }
    let base = base.trim();
    if base.is_empty() || base.starts_with('-') || base.chars().any(char::is_whitespace) {
        return Err("invalid pull request base branch".to_owned());
    }
    require_upstream(&repo, &branch)?;

    let mut args = vec![
        "pr".to_owned(),
        "create".to_owned(),
        "--draft".to_owned(),
        "--title".to_owned(),
        preview.title.trim().to_owned(),
        "--body".to_owned(),
        body.to_owned(),
        "--base".to_owned(),
        base.to_owned(),
        "--head".to_owned(),
        branch.clone(),
    ];
    for reviewer in preview
        .recipients
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        args.push("--reviewer".to_owned());
        args.push(reviewer.to_owned());
    }
    let output = Command::new("gh")
        .args(&args)
        .current_dir(&repo)
        .output()
        .map_err(|error| format!("cannot run GitHub CLI: {error}"))?;
    if !output.status.success() {
        let details = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(format!("gh pr create failed: {details}"));
    }
    let pull_request_url = String::from_utf8(output.stdout)
        .map_err(|_| "GitHub CLI output was not UTF-8".to_owned())?
        .lines()
        .rev()
        .find(|line| line.trim().starts_with("https://"))
        .map(str::trim)
        .map(str::to_owned)
        .ok_or_else(|| "GitHub CLI did not return a pull request URL".to_owned())?;
    let commit_sha = git_stdout(&repo, ["rev-parse", "HEAD"])?;
    Ok(DraftPullRequestResult {
        branch,
        commit_sha,
        pull_request_url,
    })
}

fn canonical_repo(repo: &str) -> Result<PathBuf, String> {
    let repo = fs::canonicalize(Path::new(repo))
        .map_err(|error| format!("cannot open {repo}: {error}"))?;
    if !repo.is_dir() {
        return Err(format!("{} is not a directory", repo.display()));
    }
    git_stdout(&repo, ["rev-parse", "--is-inside-work-tree"])?;
    Ok(repo)
}

fn validate_preview(
    preview: &PullRequestMutationPreview,
    confirmation: &PullRequestMutationConfirmation,
    repo: &Path,
) -> Result<(), String> {
    let preview_repo = fs::canonicalize(preview.repository.trim())
        .map_err(|error| format!("cannot open preview repository: {error}"))?;
    if preview_repo != repo {
        return Err("mutation preview repository does not match requested repository".to_owned());
    }
    if preview.title.trim().is_empty()
        || preview.body.as_deref().unwrap_or("").trim().is_empty()
        || preview.affected_resources.is_empty()
        || preview
            .affected_resources
            .iter()
            .any(|value| value.trim().is_empty())
    {
        return Err("pull request preview is incomplete".to_owned());
    }
    let expected = mutation_fingerprint(preview)?;
    if confirmation.preview_fingerprint != expected {
        return Err("mutation confirmation does not match the active preview".to_owned());
    }
    Ok(())
}

fn mutation_fingerprint(preview: &PullRequestMutationPreview) -> Result<String, String> {
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
    let branch = git_stdout(repo, ["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    if branch.is_empty() {
        return Err("repository is in detached HEAD state".to_owned());
    }
    Ok(branch)
}

fn require_upstream(repo: &Path, branch: &str) -> Result<(), String> {
    let upstream = git_stdout(
        repo,
        [
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )?;
    if upstream.is_empty() || !upstream.ends_with(branch) {
        return Err("active branch must be pushed before creating a pull request".to_owned());
    }
    Ok(())
}

fn git_stdout<const N: usize>(repo: &Path, args: [&str; N]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    if !output.status.success() {
        let details = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(format!("git command failed: {details}"));
    }
    String::from_utf8(output.stdout)
        .map_err(|_| "git output was not UTF-8".to_owned())
        .map(|value| value.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_matches_frontend_normalization() {
        let preview = PullRequestMutationPreview {
            kind: PullRequestMutationKind::PullRequest,
            repository: "/repo".to_owned(),
            branch: "feature/safe".to_owned(),
            title: "Open draft".to_owned(),
            body: Some("Body".to_owned()),
            recipients: vec!["reviewer".to_owned()],
            affected_resources: vec!["branch:feature/safe".to_owned()],
            destructive: false,
        };
        assert_eq!(
            mutation_fingerprint(&preview).expect("fingerprint"),
            r#"{"kind":"pullRequest","repository":"/repo","branch":"feature/safe","title":"Open draft","body":"Body","recipients":["reviewer"],"affectedResources":["branch:feature/safe"],"destructive":false}"#
        );
    }
}
