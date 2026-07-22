use std::process::Command;

use serde::{Deserialize, Serialize};

const MAX_TITLE_BYTES: usize = 1024;
const MAX_BODY_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum IssueMutationKind {
    IssueCreate,
    IssueUpdate,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueMutationPreview {
    kind: IssueMutationKind,
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
pub struct IssueMutationConfirmation {
    preview_fingerprint: String,
    confirmed_at: String,
}

#[derive(Debug, Deserialize)]
struct IssueResponse {
    number: u64,
    title: String,
    state: String,
    html_url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueMutationAudit {
    pub operation: String,
    pub repository: String,
    pub issue_number: u64,
    pub preview_fingerprint: String,
    pub confirmed_at: String,
    pub outcome: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueMutationResult {
    pub repository: String,
    pub issue_number: u64,
    pub title: String,
    pub state: String,
    pub url: String,
    pub audit: IssueMutationAudit,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Fingerprint<'a> {
    kind: IssueMutationKind,
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
pub fn runtime_create_github_issue(
    repository: String,
    title: String,
    body: Option<String>,
    hostname: Option<String>,
    preview: IssueMutationPreview,
    confirmation: IssueMutationConfirmation,
) -> Result<IssueMutationResult, String> {
    let repository = repository.trim().to_owned();
    let title = title.trim().to_owned();
    let body = body.unwrap_or_default().trim().to_owned();
    let hostname = normalize_hostname(hostname);

    validate_repository(&repository)?;
    validate_hostname(&hostname)?;
    validate_title(&title)?;
    validate_body(&body)?;
    validate_preview(
        &preview,
        &confirmation,
        IssueMutationKind::IssueCreate,
        &repository,
        "issue:new",
        false,
        Some(title.as_str()),
        Some(body.as_str()),
        None,
    )?;

    let endpoint = format!("repos/{repository}/issues");
    let title_field = format!("title={title}");
    let body_field = format!("body={body}");
    let response = gh_json::<IssueResponse>(
        &hostname,
        &endpoint,
        &[
            "--method",
            "POST",
            "-f",
            title_field.as_str(),
            "-f",
            body_field.as_str(),
        ],
    )?;

    result_from_response(repository, response, "issueCreate", confirmation, "created")
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn runtime_update_github_issue(
    repository: String,
    issue_number: u64,
    title: Option<String>,
    body: Option<String>,
    state: Option<String>,
    hostname: Option<String>,
    preview: IssueMutationPreview,
    confirmation: IssueMutationConfirmation,
) -> Result<IssueMutationResult, String> {
    let repository = repository.trim().to_owned();
    let title = title.map(|value| value.trim().to_owned());
    let body = body.map(|value| value.trim().to_owned());
    let state = state.map(|value| value.trim().to_owned());
    let hostname = normalize_hostname(hostname);

    validate_repository(&repository)?;
    validate_hostname(&hostname)?;
    if issue_number == 0 {
        return Err("GitHub issue number must be positive".to_owned());
    }
    if title.is_none() && body.is_none() && state.is_none() {
        return Err("GitHub issue update requires at least one changed field".to_owned());
    }
    if let Some(value) = title.as_deref() {
        validate_title(value)?;
    }
    if let Some(value) = body.as_deref() {
        validate_body(value)?;
    }
    if let Some(value) = state.as_deref() {
        validate_state(value)?;
    }

    let destructive = state.as_deref() == Some("closed");
    let resource = format!("issue:{issue_number}");
    validate_preview(
        &preview,
        &confirmation,
        IssueMutationKind::IssueUpdate,
        &repository,
        &resource,
        destructive,
        title.as_deref(),
        body.as_deref(),
        state.as_deref(),
    )?;

    let endpoint = format!("repos/{repository}/issues/{issue_number}");
    let mut owned_args = vec!["--method".to_owned(), "PATCH".to_owned()];
    for field in [
        title.map(|value| format!("title={value}")),
        body.map(|value| format!("body={value}")),
        state.map(|value| format!("state={value}")),
    ]
    .into_iter()
    .flatten()
    {
        owned_args.push("-f".to_owned());
        owned_args.push(field);
    }
    let args = owned_args.iter().map(String::as_str).collect::<Vec<_>>();
    let response = gh_json::<IssueResponse>(&hostname, &endpoint, &args)?;

    result_from_response(repository, response, "issueUpdate", confirmation, "updated")
}

fn normalize_hostname(hostname: Option<String>) -> String {
    hostname
        .as_deref()
        .unwrap_or("github.com")
        .trim()
        .to_owned()
}

fn result_from_response(
    repository: String,
    response: IssueResponse,
    operation: &str,
    confirmation: IssueMutationConfirmation,
    outcome: &str,
) -> Result<IssueMutationResult, String> {
    let url = safe_https_url(response.html_url)?;
    Ok(IssueMutationResult {
        repository: repository.clone(),
        issue_number: response.number,
        title: response.title,
        state: response.state,
        url,
        audit: IssueMutationAudit {
            operation: operation.to_owned(),
            repository,
            issue_number: response.number,
            preview_fingerprint: confirmation.preview_fingerprint,
            confirmed_at: confirmation.confirmed_at,
            outcome: outcome.to_owned(),
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
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "GitHub CLI is not installed".to_owned()
            } else {
                "cannot run GitHub issue mutation".to_owned()
            }
        })?;
    if !output.status.success() {
        return Err(
            "GitHub issue mutation failed; verify authentication and repository permissions"
                .to_owned(),
        );
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| "GitHub issue response could not be read".to_owned())
}

#[allow(clippy::too_many_arguments)]
fn validate_preview(
    preview: &IssueMutationPreview,
    confirmation: &IssueMutationConfirmation,
    kind: IssueMutationKind,
    repository: &str,
    resource: &str,
    destructive: bool,
    mutation_title: Option<&str>,
    mutation_body: Option<&str>,
    mutation_state: Option<&str>,
) -> Result<(), String> {
    if std::mem::discriminant(&preview.kind) != std::mem::discriminant(&kind) {
        return Err("mutation preview kind does not match requested issue operation".to_owned());
    }
    if preview.repository.trim() != repository {
        return Err("mutation preview repository does not match requested repository".to_owned());
    }
    if preview.destructive != destructive {
        return Err("mutation preview destructive flag does not match issue operation".to_owned());
    }
    if !preview
        .affected_resources
        .iter()
        .any(|value| value.trim() == resource)
    {
        return Err("mutation preview does not identify the requested issue resource".to_owned());
    }
    if normalized_optional(preview.mutation_title.as_deref()) != mutation_title
        || normalized_optional(preview.mutation_body.as_deref()) != mutation_body
        || normalized_optional(preview.mutation_state.as_deref()) != mutation_state
    {
        return Err("mutation preview content does not match requested issue mutation".to_owned());
    }
    if confirmation.confirmed_at.trim().is_empty() {
        return Err("mutation confirmation timestamp is required".to_owned());
    }
    let expected = mutation_fingerprint(preview)?;
    if confirmation.preview_fingerprint != expected {
        return Err("mutation confirmation does not match the active preview".to_owned());
    }
    Ok(())
}

fn normalized_optional(value: Option<&str>) -> Option<&str> {
    value.map(str::trim)
}

fn mutation_fingerprint(preview: &IssueMutationPreview) -> Result<String, String> {
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
        mutation_title: normalized_optional(preview.mutation_title.as_deref()),
        mutation_body: normalized_optional(preview.mutation_body.as_deref()),
        mutation_state: normalized_optional(preview.mutation_state.as_deref()),
    })
    .map_err(|error| format!("cannot encode mutation preview: {error}"))
}

fn validate_repository(value: &str) -> Result<(), String> {
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if owner.is_empty()
        || name.is_empty()
        || parts.next().is_some()
        || owner.starts_with('-')
        || name.starts_with('-')
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

fn validate_title(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("GitHub issue title is required".to_owned());
    }
    if value.len() > MAX_TITLE_BYTES {
        return Err("GitHub issue title is too large".to_owned());
    }
    Ok(())
}

fn validate_body(value: &str) -> Result<(), String> {
    if value.len() > MAX_BODY_BYTES {
        return Err("GitHub issue body is too large".to_owned());
    }
    Ok(())
}

fn validate_state(value: &str) -> Result<(), String> {
    if !matches!(value, "open" | "closed") {
        return Err("GitHub issue state must be open or closed".to_owned());
    }
    Ok(())
}

fn safe_https_url(value: String) -> Result<String, String> {
    if value.starts_with("https://")
        && !value.contains('@')
        && !value.to_ascii_lowercase().contains("token=")
    {
        Ok(value)
    } else {
        Err("GitHub returned an unsafe issue URL".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(kind: IssueMutationKind, resource: &str, destructive: bool) -> IssueMutationPreview {
        IssueMutationPreview {
            kind,
            repository: "octo/repo".to_owned(),
            branch: "main".to_owned(),
            title: "Issue mutation".to_owned(),
            body: Some("Details".to_owned()),
            recipients: Vec::new(),
            affected_resources: vec![resource.to_owned()],
            destructive,
            mutation_title: None,
            mutation_body: None,
            mutation_state: None,
        }
    }

    fn confirmation(preview: &IssueMutationPreview) -> IssueMutationConfirmation {
        IssueMutationConfirmation {
            preview_fingerprint: mutation_fingerprint(preview).expect("fingerprint"),
            confirmed_at: "2026-07-22T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn fingerprints_match_frontend_normalization() {
        let mut value = preview(IssueMutationKind::IssueCreate, "issue:new", false);
        value.mutation_title = Some("Bug".to_owned());
        value.mutation_body = Some("Details".to_owned());
        assert_eq!(
            mutation_fingerprint(&value).expect("fingerprint"),
            r#"{"kind":"issueCreate","repository":"octo/repo","branch":"main","title":"Issue mutation","body":"Details","recipients":[],"affectedResources":["issue:new"],"destructive":false,"mutationTitle":"Bug","mutationBody":"Details"}"#
        );
    }

    #[test]
    fn binds_confirmation_to_mutated_content() {
        let mut value = preview(IssueMutationKind::IssueCreate, "issue:new", false);
        value.mutation_title = Some("Safe title".to_owned());
        value.mutation_body = Some("Safe body".to_owned());
        let confirmed = confirmation(&value);
        let error = validate_preview(
            &value,
            &confirmed,
            IssueMutationKind::IssueCreate,
            "octo/repo",
            "issue:new",
            false,
            Some("Tampered title"),
            Some("Safe body"),
            None,
        )
        .expect_err("tampered content must fail");
        assert!(error.contains("content does not match"));
    }

    #[test]
    fn requires_destructive_confirmation_for_closing_issue() {
        let mut value = preview(IssueMutationKind::IssueUpdate, "issue:42", false);
        value.mutation_state = Some("closed".to_owned());
        let confirmed = confirmation(&value);
        let error = validate_preview(
            &value,
            &confirmed,
            IssueMutationKind::IssueUpdate,
            "octo/repo",
            "issue:42",
            true,
            None,
            None,
            Some("closed"),
        )
        .expect_err("close must require destructive preview");
        assert!(error.contains("destructive"));
    }

    #[test]
    fn rejects_stale_confirmation_and_invalid_fields() {
        let value = preview(IssueMutationKind::IssueUpdate, "issue:42", false);
        let error = validate_preview(
            &value,
            &IssueMutationConfirmation {
                preview_fingerprint: "stale".to_owned(),
                confirmed_at: "2026-07-22T00:00:00Z".to_owned(),
            },
            IssueMutationKind::IssueUpdate,
            "octo/repo",
            "issue:42",
            false,
            None,
            None,
            None,
        )
        .expect_err("stale confirmation must fail");
        assert!(error.contains("does not match"));
        assert!(validate_title("").is_err());
        assert!(validate_state("deleted").is_err());
        assert!(validate_repository("octo/../secret").is_err());
    }

    #[test]
    fn rejects_unsafe_urls() {
        assert!(safe_https_url("https://github.com/octo/repo/issues/1".to_owned()).is_ok());
        assert!(
            safe_https_url("https://user@example.com/issues/1?token=secret".to_owned()).is_err()
        );
    }
}
