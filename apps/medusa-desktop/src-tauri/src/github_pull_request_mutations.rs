use std::process::Command;

use serde::{Deserialize, Serialize};

const MAX_TITLE_BYTES: usize = 1024;
const MAX_BODY_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum PullRequestMutationKind {
    PullRequestUpdate,
    PullRequestReview,
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
    mutation_title: Option<String>,
    mutation_body: Option<String>,
    mutation_state: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestMutationConfirmation {
    preview_fingerprint: String,
    confirmed_at: String,
}

#[derive(Debug, Deserialize)]
struct PullRequestResponse {
    number: u64,
    title: String,
    state: String,
    html_url: String,
}

#[derive(Debug, Deserialize)]
struct ReviewResponse {
    id: u64,
    state: String,
    html_url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestMutationAudit {
    operation: String,
    repository: String,
    pull_request_number: u64,
    preview_fingerprint: String,
    confirmed_at: String,
    outcome: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestMutationResult {
    repository: String,
    pull_request_number: u64,
    title: Option<String>,
    state: String,
    url: String,
    review_id: Option<u64>,
    audit: PullRequestMutationAudit,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    mutation_title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mutation_body: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mutation_state: Option<&'a str>,
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn runtime_update_github_pull_request(
    repository: String,
    pull_request_number: u64,
    title: Option<String>,
    body: Option<String>,
    state: Option<String>,
    base: Option<String>,
    hostname: Option<String>,
    preview: PullRequestMutationPreview,
    confirmation: PullRequestMutationConfirmation,
) -> Result<PullRequestMutationResult, String> {
    let repository = repository.trim().to_owned();
    let title = normalized_option(title);
    let body = normalized_option(body);
    let state = normalized_option(state);
    let base = normalized_option(base);
    let hostname = normalize_hostname(hostname);

    validate_repository(&repository)?;
    validate_hostname(&hostname)?;
    validate_number(pull_request_number)?;
    if title.is_none() && body.is_none() && state.is_none() && base.is_none() {
        return Err("GitHub pull request update requires at least one changed field".to_owned());
    }
    if let Some(value) = title.as_deref() {
        validate_title(value)?;
    }
    if let Some(value) = body.as_deref() {
        validate_body(value)?;
    }
    if let Some(value) = state.as_deref() {
        validate_pull_request_state(value)?;
    }
    if let Some(value) = base.as_deref() {
        validate_ref(value)?;
    }

    let destructive = state.as_deref() == Some("closed");
    let resource = format!("pullRequest:{pull_request_number}");
    let encoded_state = encode_update_state(state.as_deref(), base.as_deref());
    validate_preview(
        &preview,
        &confirmation,
        PullRequestMutationKind::PullRequestUpdate,
        &repository,
        &resource,
        destructive,
        title.as_deref(),
        body.as_deref(),
        encoded_state.as_deref(),
    )?;

    let endpoint = format!("repos/{repository}/pulls/{pull_request_number}");
    let mut args = vec!["--method".to_owned(), "PATCH".to_owned()];
    append_field(&mut args, "title", title);
    append_field(&mut args, "body", body);
    append_field(&mut args, "state", state);
    append_field(&mut args, "base", base);
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let response = gh_json::<PullRequestResponse>(&hostname, &endpoint, &refs)?;
    let url = safe_https_url(response.html_url)?;

    Ok(PullRequestMutationResult {
        repository: repository.clone(),
        pull_request_number: response.number,
        title: Some(response.title),
        state: response.state,
        url,
        review_id: None,
        audit: audit(
            "pullRequestUpdate",
            repository,
            pull_request_number,
            confirmation,
            "updated",
        ),
    })
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn runtime_review_github_pull_request(
    repository: String,
    pull_request_number: u64,
    action: String,
    body: Option<String>,
    commit_id: Option<String>,
    hostname: Option<String>,
    preview: PullRequestMutationPreview,
    confirmation: PullRequestMutationConfirmation,
) -> Result<PullRequestMutationResult, String> {
    let repository = repository.trim().to_owned();
    let action = action.trim().to_ascii_lowercase();
    let body = normalized_option(body);
    let commit_id = normalized_option(commit_id);
    let hostname = normalize_hostname(hostname);

    validate_repository(&repository)?;
    validate_hostname(&hostname)?;
    validate_number(pull_request_number)?;
    validate_review_action(&action)?;
    if matches!(action.as_str(), "comment" | "request_changes") && body.is_none() {
        return Err("GitHub pull request review body is required for this action".to_owned());
    }
    if let Some(value) = body.as_deref() {
        validate_body(value)?;
    }
    if let Some(value) = commit_id.as_deref() {
        validate_commit_sha(value)?;
    }

    let resource = format!("pullRequest:{pull_request_number}");
    let encoded_state = encode_review_state(&action, commit_id.as_deref());
    validate_preview(
        &preview,
        &confirmation,
        PullRequestMutationKind::PullRequestReview,
        &repository,
        &resource,
        false,
        None,
        body.as_deref(),
        Some(encoded_state.as_str()),
    )?;

    let endpoint = format!("repos/{repository}/pulls/{pull_request_number}/reviews");
    let mut args = vec!["--method".to_owned(), "POST".to_owned()];
    append_field(&mut args, "event", Some(review_event(&action).to_owned()));
    append_field(&mut args, "body", body);
    append_field(&mut args, "commit_id", commit_id);
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let response = gh_json::<ReviewResponse>(&hostname, &endpoint, &refs)?;
    let url = safe_https_url(response.html_url)?;

    Ok(PullRequestMutationResult {
        repository: repository.clone(),
        pull_request_number,
        title: None,
        state: response.state,
        url,
        review_id: Some(response.id),
        audit: audit(
            "pullRequestReview",
            repository,
            pull_request_number,
            confirmation,
            "submitted",
        ),
    })
}

fn audit(
    operation: &str,
    repository: String,
    pull_request_number: u64,
    confirmation: PullRequestMutationConfirmation,
    outcome: &str,
) -> PullRequestMutationAudit {
    PullRequestMutationAudit {
        operation: operation.to_owned(),
        repository,
        pull_request_number,
        preview_fingerprint: confirmation.preview_fingerprint,
        confirmed_at: confirmation.confirmed_at,
        outcome: outcome.to_owned(),
    }
}

fn append_field(args: &mut Vec<String>, name: &str, value: Option<String>) {
    if let Some(value) = value {
        args.push("-f".to_owned());
        args.push(format!("{name}={value}"));
    }
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
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "GitHub CLI is not installed".to_owned()
            } else {
                "cannot run GitHub pull request mutation".to_owned()
            }
        })?;
    if !output.status.success() {
        return Err(
            "GitHub pull request mutation failed; verify authentication, repository permissions, and the active pull request head"
                .to_owned(),
        );
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| "GitHub pull request response could not be read".to_owned())
}

#[allow(clippy::too_many_arguments)]
fn validate_preview(
    preview: &PullRequestMutationPreview,
    confirmation: &PullRequestMutationConfirmation,
    kind: PullRequestMutationKind,
    repository: &str,
    resource: &str,
    destructive: bool,
    mutation_title: Option<&str>,
    mutation_body: Option<&str>,
    mutation_state: Option<&str>,
) -> Result<(), String> {
    if std::mem::discriminant(&preview.kind) != std::mem::discriminant(&kind) {
        return Err(
            "mutation preview kind does not match requested pull request operation".to_owned(),
        );
    }
    if preview.repository.trim() != repository {
        return Err("mutation preview repository does not match requested repository".to_owned());
    }
    if preview.destructive != destructive {
        return Err(
            "mutation preview destructive flag does not match pull request operation".to_owned(),
        );
    }
    if !preview
        .affected_resources
        .iter()
        .any(|value| value.trim() == resource)
    {
        return Err("mutation preview does not identify the requested pull request".to_owned());
    }
    if normalized_str(preview.mutation_title.as_deref()) != mutation_title
        || normalized_str(preview.mutation_body.as_deref()) != mutation_body
        || normalized_str(preview.mutation_state.as_deref()) != mutation_state
    {
        return Err(
            "mutation preview content does not match requested pull request mutation".to_owned(),
        );
    }
    if confirmation.confirmed_at.trim().is_empty() {
        return Err("mutation confirmation timestamp is required".to_owned());
    }
    if confirmation.preview_fingerprint != mutation_fingerprint(preview)? {
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
        mutation_title: normalized_str(preview.mutation_title.as_deref()),
        mutation_body: normalized_str(preview.mutation_body.as_deref()),
        mutation_state: normalized_str(preview.mutation_state.as_deref()),
    })
    .map_err(|error| format!("cannot encode mutation preview: {error}"))
}

fn normalized_option(value: Option<String>) -> Option<String> {
    value.map(|value| value.trim().to_owned())
}

fn normalized_str(value: Option<&str>) -> Option<&str> {
    value.map(str::trim)
}

fn encode_update_state(state: Option<&str>, base: Option<&str>) -> Option<String> {
    match (state, base) {
        (None, None) => None,
        (state, base) => Some(format!(
            "state={};base={}",
            state.unwrap_or(""),
            base.unwrap_or("")
        )),
    }
}

fn encode_review_state(action: &str, commit_id: Option<&str>) -> String {
    format!("action={action};commit={}", commit_id.unwrap_or(""))
}

fn review_event(action: &str) -> &'static str {
    match action {
        "approve" => "APPROVE",
        "request_changes" => "REQUEST_CHANGES",
        _ => "COMMENT",
    }
}

fn normalize_hostname(value: Option<String>) -> String {
    value.as_deref().unwrap_or("github.com").trim().to_owned()
}

fn validate_number(value: u64) -> Result<(), String> {
    if value == 0 {
        Err("GitHub pull request number must be positive".to_owned())
    } else {
        Ok(())
    }
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
        Err("invalid GitHub hostname".to_owned())
    } else {
        Ok(())
    }
}

fn validate_title(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("GitHub pull request title cannot be blank".to_owned());
    }
    if value.len() > MAX_TITLE_BYTES {
        return Err("GitHub pull request title is too large".to_owned());
    }
    Ok(())
}

fn validate_body(value: &str) -> Result<(), String> {
    if value.len() > MAX_BODY_BYTES {
        Err("GitHub pull request body is too large".to_owned())
    } else {
        Ok(())
    }
}

fn validate_pull_request_state(value: &str) -> Result<(), String> {
    if matches!(value, "open" | "closed") {
        Ok(())
    } else {
        Err("GitHub pull request state must be open or closed".to_owned())
    }
}

fn validate_review_action(value: &str) -> Result<(), String> {
    if matches!(value, "approve" | "comment" | "request_changes") {
        Ok(())
    } else {
        Err("GitHub pull request review action is invalid".to_owned())
    }
}

fn validate_ref(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.starts_with('-')
        || value.contains("..")
        || value.chars().any(char::is_whitespace)
    {
        Err("invalid GitHub branch ref".to_owned())
    } else {
        Ok(())
    }
}

fn validate_commit_sha(value: &str) -> Result<(), String> {
    if value.len() == 40 && value.chars().all(|character| character.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("GitHub review commit id must be a full commit SHA".to_owned())
    }
}

fn safe_https_url(value: String) -> Result<String, String> {
    if value.starts_with("https://")
        && !value.contains('@')
        && !value.to_ascii_lowercase().contains("token=")
    {
        Ok(value)
    } else {
        Err("GitHub returned an unsafe pull request URL".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(kind: PullRequestMutationKind, destructive: bool) -> PullRequestMutationPreview {
        PullRequestMutationPreview {
            kind,
            repository: "octo/repo".to_owned(),
            branch: "feature".to_owned(),
            title: "Pull request mutation".to_owned(),
            body: Some("Details".to_owned()),
            recipients: Vec::new(),
            affected_resources: vec!["pullRequest:42".to_owned()],
            destructive,
            mutation_title: None,
            mutation_body: None,
            mutation_state: None,
        }
    }

    fn confirmation(preview: &PullRequestMutationPreview) -> PullRequestMutationConfirmation {
        PullRequestMutationConfirmation {
            preview_fingerprint: mutation_fingerprint(preview).expect("fingerprint"),
            confirmed_at: "2026-07-22T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn update_confirmation_is_bound_to_exact_payload() {
        let mut value = preview(PullRequestMutationKind::PullRequestUpdate, false);
        value.mutation_title = Some("Updated".to_owned());
        value.mutation_state = Some("state=open;base=main".to_owned());
        let confirmed = confirmation(&value);
        assert!(
            validate_preview(
                &value,
                &confirmed,
                PullRequestMutationKind::PullRequestUpdate,
                "octo/repo",
                "pullRequest:42",
                false,
                Some("Updated"),
                None,
                Some("state=open;base=main"),
            )
            .is_ok()
        );
        assert!(
            validate_preview(
                &value,
                &confirmed,
                PullRequestMutationKind::PullRequestUpdate,
                "octo/repo",
                "pullRequest:42",
                false,
                Some("Tampered"),
                None,
                Some("state=open;base=main"),
            )
            .is_err()
        );
    }

    #[test]
    fn closing_requires_destructive_preview() {
        let value = preview(PullRequestMutationKind::PullRequestUpdate, false);
        let confirmed = confirmation(&value);
        assert!(
            validate_preview(
                &value,
                &confirmed,
                PullRequestMutationKind::PullRequestUpdate,
                "octo/repo",
                "pullRequest:42",
                true,
                None,
                None,
                Some("state=closed;base="),
            )
            .is_err()
        );
    }

    #[test]
    fn review_actions_and_commit_ids_are_strict() {
        assert!(validate_review_action("approve").is_ok());
        assert!(validate_review_action("delete").is_err());
        assert!(validate_commit_sha(&"a".repeat(40)).is_ok());
        assert!(validate_commit_sha("abc").is_err());
    }

    #[test]
    fn unsafe_urls_and_refs_are_rejected() {
        assert!(safe_https_url("https://github.com/octo/repo/pull/42".to_owned()).is_ok());
        assert!(
            safe_https_url("https://user@example.com/pull/42?token=secret".to_owned()).is_err()
        );
        assert!(validate_ref("feature/safe").is_ok());
        assert!(validate_ref("../main").is_err());
    }
}
