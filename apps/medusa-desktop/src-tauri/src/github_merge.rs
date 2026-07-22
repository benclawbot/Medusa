use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum MergeMutationKind {
    PullRequestMerge,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeMutationPreview {
    kind: MergeMutationKind,
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
pub struct MergeMutationConfirmation {
    preview_fingerprint: String,
    confirmed_at: String,
}

#[derive(Debug, Deserialize)]
struct PullRequestResponse {
    state: String,
    draft: Option<bool>,
    head: PullRequestHead,
}

#[derive(Debug, Deserialize)]
struct PullRequestHead {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct MergeResponse {
    sha: Option<String>,
    merged: bool,
    message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestMergeResult {
    pub repository: String,
    pub pull_request_number: u64,
    pub expected_head_sha: String,
    pub merge_commit_sha: String,
    pub merge_method: String,
    pub audit: PullRequestMergeAudit,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestMergeAudit {
    pub operation: String,
    pub repository: String,
    pub pull_request_number: u64,
    pub expected_head_sha: String,
    pub preview_fingerprint: String,
    pub confirmed_at: String,
    pub outcome: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Fingerprint<'a> {
    kind: MergeMutationKind,
    repository: &'a str,
    branch: &'a str,
    title: &'a str,
    body: &'a str,
    recipients: Vec<String>,
    affected_resources: Vec<String>,
    destructive: bool,
}

#[tauri::command]
pub fn runtime_merge_github_pull_request(
    repository: String,
    pull_request_number: u64,
    expected_head_sha: String,
    merge_method: Option<String>,
    hostname: Option<String>,
    preview: MergeMutationPreview,
    confirmation: MergeMutationConfirmation,
) -> Result<PullRequestMergeResult, String> {
    let repository = repository.trim().to_owned();
    let expected_head_sha = expected_head_sha.trim().to_owned();
    let hostname = hostname.as_deref().unwrap_or("github.com").trim().to_owned();
    let merge_method = merge_method.as_deref().unwrap_or("squash").trim().to_owned();

    validate_repository(&repository)?;
    validate_hostname(&hostname)?;
    validate_sha(&expected_head_sha)?;
    validate_merge_method(&merge_method)?;
    validate_preview(
        &preview,
        &confirmation,
        &repository,
        pull_request_number,
        &expected_head_sha,
    )?;

    let endpoint = format!("repos/{repository}/pulls/{pull_request_number}");
    let pull_request = gh_json::<PullRequestResponse>(&hostname, &endpoint, &["--method", "GET"])?;
    if pull_request.state != "open" {
        return Err("pull request must be open before merge".to_owned());
    }
    if pull_request.draft.unwrap_or(false) {
        return Err("draft pull requests cannot be merged".to_owned());
    }
    if pull_request.head.sha != expected_head_sha {
        return Err("pull request head changed after confirmation".to_owned());
    }

    let merge_endpoint = format!("{endpoint}/merge");
    let merge_response = gh_json::<MergeResponse>(
        &hostname,
        &merge_endpoint,
        &[
            "--method",
            "PUT",
            "-f",
            expected_head_sha_field(&expected_head_sha).as_str(),
            "-f",
            merge_method_field(&merge_method).as_str(),
        ],
    )?;
    if !merge_response.merged {
        return Err(format!("GitHub did not merge the pull request: {}", merge_response.message));
    }
    let merge_commit_sha = merge_response
        .sha
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "GitHub did not return a merge commit SHA".to_owned())?;

    Ok(PullRequestMergeResult {
        repository: repository.clone(),
        pull_request_number,
        expected_head_sha: expected_head_sha.clone(),
        merge_commit_sha,
        merge_method: merge_method.clone(),
        audit: PullRequestMergeAudit {
            operation: "pullRequestMerge".to_owned(),
            repository,
            pull_request_number,
            expected_head_sha,
            preview_fingerprint: confirmation.preview_fingerprint,
            confirmed_at: confirmation.confirmed_at,
            outcome: "merged".to_owned(),
        },
    })
}

fn gh_json<T: for<'de> Deserialize<'de>>(
    hostname: &str,
    endpoint: &str,
    extra_args: &[&str],
) -> Result<T, String> {
    let output = Command::new("gh")
        .args(["api", "--hostname", hostname, endpoint])
        .args(extra_args)
        .output()
        .map_err(|error| format!("cannot run GitHub CLI: {error}"))?;
    if !output.status.success() {
        return Err("GitHub operation failed; verify authentication and repository permissions".to_owned());
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| "GitHub response could not be read".to_owned())
}

fn validate_preview(
    preview: &MergeMutationPreview,
    confirmation: &MergeMutationConfirmation,
    repository: &str,
    pull_request_number: u64,
    expected_head_sha: &str,
) -> Result<(), String> {
    if preview.repository.trim() != repository {
        return Err("mutation preview repository does not match requested repository".to_owned());
    }
    if preview.branch.trim() != expected_head_sha {
        return Err("mutation preview head SHA does not match requested head SHA".to_owned());
    }
    if !preview.destructive {
        return Err("pull request merge preview must be marked destructive".to_owned());
    }
    let resource = format!("pull-request:{pull_request_number}");
    if !preview
        .affected_resources
        .iter()
        .any(|value| value.trim() == resource)
    {
        return Err("mutation preview does not identify the requested pull request".to_owned());
    }
    let expected = mutation_fingerprint(preview)?;
    if confirmation.preview_fingerprint != expected {
        return Err("mutation confirmation does not match the active preview".to_owned());
    }
    Ok(())
}

fn mutation_fingerprint(preview: &MergeMutationPreview) -> Result<String, String> {
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

fn expected_head_sha_field(value: &str) -> String {
    format!("sha={value}")
}

fn merge_method_field(value: &str) -> String {
    format!("merge_method={value}")
}

fn validate_repository(value: &str) -> Result<(), String> {
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if owner.is_empty()
        || name.is_empty()
        || parts.next().is_some()
        || !owner.chars().all(valid_repository_character)
        || !name.chars().all(valid_repository_character)
    {
        return Err("invalid GitHub repository".to_owned());
    }
    Ok(())
}

fn valid_repository_character(value: char) -> bool {
    value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.')
}

fn validate_hostname(value: &str) -> Result<(), String> {
    if value.is_empty() || value.starts_with('-') || value.chars().any(char::is_whitespace) {
        return Err("invalid GitHub hostname".to_owned());
    }
    Ok(())
}

fn validate_sha(value: &str) -> Result<(), String> {
    if !(7..=64).contains(&value.len()) || !value.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err("invalid pull request head SHA".to_owned());
    }
    Ok(())
}

fn validate_merge_method(value: &str) -> Result<(), String> {
    if !matches!(value, "merge" | "squash" | "rebase") {
        return Err("merge method must be merge, squash, or rebase".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview() -> MergeMutationPreview {
        MergeMutationPreview {
            kind: MergeMutationKind::PullRequestMerge,
            repository: "octo/repo".to_owned(),
            branch: "abcdef1".to_owned(),
            title: "Merge pull request #42".to_owned(),
            body: None,
            recipients: Vec::new(),
            affected_resources: vec!["pull-request:42".to_owned()],
            destructive: true,
        }
    }

    #[test]
    fn fingerprint_matches_frontend_normalization() {
        assert_eq!(
            mutation_fingerprint(&preview()).expect("fingerprint"),
            r#"{"kind":"pullRequestMerge","repository":"octo/repo","branch":"abcdef1","title":"Merge pull request #42","body":"","recipients":[],"affectedResources":["pull-request:42"],"destructive":true}"#
        );
    }

    #[test]
    fn rejects_stale_confirmation() {
        let error = validate_preview(
            &preview(),
            &MergeMutationConfirmation {
                preview_fingerprint: "stale".to_owned(),
                confirmed_at: "2026-07-22T00:00:00Z".to_owned(),
            },
            "octo/repo",
            42,
            "abcdef1",
        )
        .expect_err("stale confirmation must fail");
        assert!(error.contains("does not match"));
    }

    #[test]
    fn rejects_non_destructive_preview() {
        let mut value = preview();
        value.destructive = false;
        let fingerprint = mutation_fingerprint(&value).expect("fingerprint");
        let error = validate_preview(
            &value,
            &MergeMutationConfirmation {
                preview_fingerprint: fingerprint,
                confirmed_at: "2026-07-22T00:00:00Z".to_owned(),
            },
            "octo/repo",
            42,
            "abcdef1",
        )
        .expect_err("non-destructive merge preview must fail");
        assert!(error.contains("destructive"));
    }
}
