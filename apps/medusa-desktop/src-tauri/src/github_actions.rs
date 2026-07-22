use std::process::{Command, Output};

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubActionsRetryPreview {
    pub kind: GithubActionsMutationKind,
    pub repository: String,
    pub branch: String,
    pub title: String,
    pub body: Option<String>,
    pub recipients: Vec<String>,
    pub affected_resources: Vec<String>,
    pub destructive: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GithubActionsMutationKind {
    ActionsRetry,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubActionsRetryConfirmation {
    pub preview_fingerprint: String,
    pub confirmed_at: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubActionsRetryResult {
    pub repository: String,
    pub run_id: u64,
    pub job_id: u64,
    pub commit_sha: String,
    pub audit: GithubMutationAuditRecord,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubMutationAuditRecord {
    pub operation: String,
    pub repository: String,
    pub run_id: u64,
    pub job_id: u64,
    pub commit_sha: String,
    pub preview_fingerprint: String,
    pub confirmed_at: String,
    pub outcome: String,
}

#[derive(Debug, Deserialize)]
struct ActionsJobResponse {
    id: u64,
    run_id: u64,
    name: String,
    status: String,
    conclusion: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ActionsRunResponse {
    id: u64,
    head_sha: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Fingerprint<'a> {
    kind: GithubActionsMutationKind,
    repository: &'a str,
    branch: &'a str,
    title: &'a str,
    body: &'a str,
    recipients: Vec<String>,
    affected_resources: Vec<String>,
    destructive: bool,
}

#[tauri::command]
pub fn runtime_retry_github_actions_job(
    repository: String,
    hostname: Option<String>,
    run_id: u64,
    job_id: u64,
    commit_sha: String,
    preview: GithubActionsRetryPreview,
    confirmation: GithubActionsRetryConfirmation,
) -> Result<GithubActionsRetryResult, String> {
    let repository = repository.trim().to_owned();
    let hostname = hostname
        .as_deref()
        .unwrap_or("github.com")
        .trim()
        .to_owned();
    let commit_sha = commit_sha.trim().to_ascii_lowercase();

    validate_repository(&repository)?;
    validate_hostname(&hostname)?;
    validate_commit_sha(&commit_sha)?;
    require_confirmation(&preview, &confirmation)?;
    require_preview_target(&preview, &repository, &commit_sha, run_id, job_id)?;

    let job_endpoint = format!("repos/{repository}/actions/jobs/{job_id}");
    let job_output = run_gh_api(&hostname, "GET", &job_endpoint)?;
    let job = parse_job(&job_output)?;
    if job.id != job_id || job.run_id != run_id {
        return Err("GitHub Actions job does not match the requested run and job".to_owned());
    }
    require_retryable_job(&job)?;

    let run_endpoint = format!("repos/{repository}/actions/runs/{run_id}");
    let run_output = run_gh_api(&hostname, "GET", &run_endpoint)?;
    let run = parse_run(&run_output)?;
    if run.id != run_id || !run.head_sha.eq_ignore_ascii_case(&commit_sha) {
        return Err("GitHub Actions run does not match the requested commit".to_owned());
    }

    let retry_endpoint = format!("repos/{repository}/actions/jobs/{job_id}/rerun");
    run_gh_api(&hostname, "POST", &retry_endpoint)?;

    let fingerprint = mutation_fingerprint(&preview)?;
    Ok(GithubActionsRetryResult {
        repository: repository.clone(),
        run_id,
        job_id,
        commit_sha: commit_sha.clone(),
        audit: GithubMutationAuditRecord {
            operation: "actionsRetry".to_owned(),
            repository,
            run_id,
            job_id,
            commit_sha,
            preview_fingerprint: fingerprint,
            confirmed_at: confirmation.confirmed_at.trim().to_owned(),
            outcome: "requested".to_owned(),
        },
    })
}

fn run_gh_api(hostname: &str, method: &str, endpoint: &str) -> Result<Output, String> {
    let output = Command::new("gh")
        .args([
            "api",
            "--method",
            method,
            "--hostname",
            hostname,
            endpoint,
        ])
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "GitHub CLI is not installed".to_owned()
            } else {
                "cannot execute GitHub Actions operation".to_owned()
            }
        })?;
    if output.status.success() {
        Ok(output)
    } else {
        Err("GitHub Actions operation failed".to_owned())
    }
}

fn parse_job(output: &Output) -> Result<ActionsJobResponse, String> {
    serde_json::from_slice(&output.stdout)
        .map_err(|_| "GitHub Actions job response could not be read".to_owned())
}

fn parse_run(output: &Output) -> Result<ActionsRunResponse, String> {
    serde_json::from_slice(&output.stdout)
        .map_err(|_| "GitHub Actions run response could not be read".to_owned())
}

fn require_retryable_job(job: &ActionsJobResponse) -> Result<(), String> {
    if job.status != "completed" {
        return Err(format!("GitHub Actions job '{}' is not completed", job.name));
    }
    let retryable = matches!(
        job.conclusion.as_deref(),
        Some("failure" | "cancelled" | "timed_out")
    );
    if !retryable {
        return Err(format!("GitHub Actions job '{}' is not retryable", job.name));
    }
    Ok(())
}

fn require_confirmation(
    preview: &GithubActionsRetryPreview,
    confirmation: &GithubActionsRetryConfirmation,
) -> Result<(), String> {
    validate_preview(preview)?;
    if confirmation.confirmed_at.trim().is_empty() {
        return Err("mutation confirmation timestamp is required".to_owned());
    }
    let expected = mutation_fingerprint(preview)?;
    if confirmation.preview_fingerprint != expected {
        return Err("mutation confirmation does not match the active preview".to_owned());
    }
    Ok(())
}

fn require_preview_target(
    preview: &GithubActionsRetryPreview,
    repository: &str,
    commit_sha: &str,
    run_id: u64,
    job_id: u64,
) -> Result<(), String> {
    if preview.kind != GithubActionsMutationKind::ActionsRetry {
        return Err("mutation preview kind does not match requested operation".to_owned());
    }
    if preview.repository.trim() != repository {
        return Err("mutation preview repository does not match requested repository".to_owned());
    }
    if !preview.branch.trim().eq_ignore_ascii_case(commit_sha) {
        return Err("mutation preview commit does not match requested commit".to_owned());
    }
    let run_resource = format!("actionsRun:{run_id}");
    let job_resource = format!("actionsJob:{job_id}");
    if !preview
        .affected_resources
        .iter()
        .any(|value| value.trim() == run_resource)
        || !preview
            .affected_resources
            .iter()
            .any(|value| value.trim() == job_resource)
    {
        return Err("mutation preview does not identify the requested run and job".to_owned());
    }
    Ok(())
}

fn validate_preview(preview: &GithubActionsRetryPreview) -> Result<(), String> {
    if preview.repository.trim().is_empty()
        || preview.branch.trim().is_empty()
        || preview.title.trim().is_empty()
    {
        return Err("mutation preview repository, commit, and title are required".to_owned());
    }
    if preview.affected_resources.is_empty()
        || preview
            .affected_resources
            .iter()
            .any(|value| value.trim().is_empty())
    {
        return Err("mutation preview affected resources are required".to_owned());
    }
    Ok(())
}

fn mutation_fingerprint(preview: &GithubActionsRetryPreview) -> Result<String, String> {
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

fn validate_hostname(value: &str) -> Result<(), String> {
    if value.is_empty() || value.starts_with('-') || value.chars().any(char::is_whitespace) {
        Err("invalid GitHub hostname".to_owned())
    } else {
        Ok(())
    }
}

fn validate_repository(value: &str) -> Result<(), String> {
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    let valid = !owner.is_empty()
        && !name.is_empty()
        && parts.next().is_none()
        && !owner.starts_with('-')
        && !name.starts_with('-')
        && owner.chars().all(valid_repository_character)
        && name.chars().all(valid_repository_character);
    if valid {
        Ok(())
    } else {
        Err("invalid GitHub repository".to_owned())
    }
}

fn valid_repository_character(value: char) -> bool {
    value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.')
}

fn validate_commit_sha(value: &str) -> Result<(), String> {
    if (7..=64).contains(&value.len()) && value.chars().all(|value| value.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("invalid commit SHA".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview() -> GithubActionsRetryPreview {
        GithubActionsRetryPreview {
            kind: GithubActionsMutationKind::ActionsRetry,
            repository: "octo/repo".to_owned(),
            branch: "abcdef1".to_owned(),
            title: "Retry failed GitHub Actions job".to_owned(),
            body: Some("Retry job 99 from run 42".to_owned()),
            recipients: Vec::new(),
            affected_resources: vec!["actionsRun:42".to_owned(), "actionsJob:99".to_owned()],
            destructive: false,
        }
    }

    #[test]
    fn fingerprint_matches_frontend_contract() {
        let value = mutation_fingerprint(&preview()).expect("fingerprint");
        assert_eq!(
            value,
            r#"{"kind":"actionsRetry","repository":"octo/repo","branch":"abcdef1","title":"Retry failed GitHub Actions job","body":"Retry job 99 from run 42","recipients":[],"affectedResources":["actionsJob:99","actionsRun:42"],"destructive":false}"#
        );
    }

    #[test]
    fn rejects_stale_confirmation() {
        let error = require_confirmation(
            &preview(),
            &GithubActionsRetryConfirmation {
                preview_fingerprint: "stale".to_owned(),
                confirmed_at: "2026-07-22T00:00:00Z".to_owned(),
            },
        )
        .expect_err("stale confirmation should fail");
        assert!(error.contains("does not match"));
    }

    #[test]
    fn accepts_only_failed_completed_jobs() {
        let failed = ActionsJobResponse {
            id: 99,
            run_id: 42,
            name: "test".to_owned(),
            status: "completed".to_owned(),
            conclusion: Some("failure".to_owned()),
        };
        assert!(require_retryable_job(&failed).is_ok());
        let successful = ActionsJobResponse {
            conclusion: Some("success".to_owned()),
            ..failed
        };
        assert!(require_retryable_job(&successful).is_err());
    }

    #[test]
    fn requires_run_and_job_resources() {
        let mut value = preview();
        value.affected_resources.pop();
        assert!(require_preview_target(&value, "octo/repo", "abcdef1", 42, 99).is_err());
    }
}
