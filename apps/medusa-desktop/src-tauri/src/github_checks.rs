use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubCommitChecks {
    pub repository: String,
    pub commit_sha: String,
    pub checks: Vec<GithubCheck>,
    pub message: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubCheck {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub details_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CheckRunsResponse {
    check_runs: Vec<CheckRunResponse>,
}

#[derive(Debug, Deserialize)]
struct CheckRunResponse {
    name: String,
    status: String,
    conclusion: Option<String>,
    details_url: Option<String>,
}

#[tauri::command]
pub fn runtime_github_commit_checks(
    repository: String,
    commit_sha: String,
    hostname: Option<String>,
) -> Result<GithubCommitChecks, String> {
    let repository = repository.trim().to_owned();
    let commit_sha = commit_sha.trim().to_owned();
    let hostname = hostname
        .unwrap_or_else(|| "github.com".to_owned())
        .trim()
        .to_owned();
    if !valid_repository(&repository) || !valid_sha(&commit_sha) || !valid_hostname(&hostname) {
        return Err("invalid GitHub repository, commit SHA, or hostname".to_owned());
    }

    let endpoint = format!("repos/{repository}/commits/{commit_sha}/check-runs");
    let output = Command::new("gh")
        .args([
            "api",
            "--hostname",
            hostname.as_str(),
            "-H",
            "Accept: application/vnd.github+json",
            endpoint.as_str(),
        ])
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "GitHub CLI is not installed".to_owned()
            } else {
                "cannot inspect GitHub checks".to_owned()
            }
        })?;
    if !output.status.success() {
        return Err("GitHub checks could not be read".to_owned());
    }
    parse_checks(
        &repository,
        &commit_sha,
        &String::from_utf8_lossy(&output.stdout),
    )
}

fn parse_checks(
    repository: &str,
    commit_sha: &str,
    stdout: &str,
) -> Result<GithubCommitChecks, String> {
    let response: CheckRunsResponse = serde_json::from_str(stdout)
        .map_err(|_| "GitHub checks response could not be read".to_owned())?;
    let mut checks = response
        .check_runs
        .into_iter()
        .map(|check| GithubCheck {
            name: check.name,
            status: check.status,
            conclusion: check.conclusion,
            details_url: check
                .details_url
                .filter(|value| value.starts_with("https://")),
        })
        .collect::<Vec<_>>();
    checks.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(GithubCommitChecks {
        repository: repository.to_owned(),
        commit_sha: commit_sha.to_owned(),
        checks,
        message: "GitHub commit checks are ready".to_owned(),
    })
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
        && owner.chars().all(valid_repository_character)
        && name.chars().all(valid_repository_character)
}

fn valid_repository_character(value: char) -> bool {
    value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.')
}

fn valid_sha(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.chars().all(|value| value.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_sorts_commit_checks() {
        let result = parse_checks(
            "octo/repo",
            "abcdef1",
            r#"{"check_runs":[{"name":"Tests","status":"completed","conclusion":"success","details_url":"https://github.com/octo/repo/actions/runs/1"},{"name":"Clippy","status":"completed","conclusion":"failure","details_url":null}]}"#,
        )
        .unwrap();
        assert_eq!(result.checks[0].name, "Clippy");
        assert_eq!(result.checks[0].conclusion.as_deref(), Some("failure"));
    }

    #[test]
    fn rejects_credential_bearing_or_non_https_details_urls() {
        let result = parse_checks(
            "octo/repo",
            "abcdef1",
            r#"{"check_runs":[{"name":"Tests","status":"completed","conclusion":"success","details_url":"http://token@example.com"}]}"#,
        )
        .unwrap();
        assert!(result.checks[0].details_url.is_none());
    }
}
