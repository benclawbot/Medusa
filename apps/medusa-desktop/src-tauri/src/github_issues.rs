use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubIssueSummary {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub author: Option<String>,
    pub labels: Vec<String>,
    pub url: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubIssueList {
    pub repository: String,
    pub issues: Vec<GithubIssueSummary>,
}

#[derive(Debug, Deserialize)]
struct IssueResponse {
    number: u64,
    title: String,
    state: String,
    html_url: Option<String>,
    user: Option<IssueUser>,
    labels: Vec<IssueLabel>,
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct IssueUser {
    login: String,
}

#[derive(Debug, Deserialize)]
struct IssueLabel {
    name: String,
}

#[tauri::command]
pub fn runtime_github_issues(
    repository: String,
    hostname: Option<String>,
    state: Option<String>,
) -> Result<GithubIssueList, String> {
    let repository = repository.trim().to_owned();
    let hostname = hostname
        .as_deref()
        .unwrap_or("github.com")
        .trim()
        .to_owned();
    let state = state.as_deref().unwrap_or("open").trim().to_owned();

    validate_hostname(&hostname)?;
    validate_repository(&repository)?;
    if !matches!(state.as_str(), "open" | "closed" | "all") {
        return Err("invalid GitHub issue state".to_owned());
    }

    let endpoint = format!("repos/{repository}/issues?state={state}&per_page=100");
    let output = Command::new("gh")
        .args(["api", "--hostname", hostname.as_str(), endpoint.as_str()])
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "GitHub CLI is not installed".to_owned()
            } else {
                "cannot inspect GitHub issues".to_owned()
            }
        })?;

    if !output.status.success() {
        return Err("cannot inspect GitHub issues".to_owned());
    }

    parse_issues(&repository, &String::from_utf8_lossy(&output.stdout))
}

fn parse_issues(repository: &str, stdout: &str) -> Result<GithubIssueList, String> {
    let mut issues = serde_json::from_str::<Vec<IssueResponse>>(stdout)
        .map_err(|_| "GitHub issue response could not be read".to_owned())?
        .into_iter()
        .filter(|issue| issue.pull_request.is_none())
        .map(|issue| {
            let mut labels = issue
                .labels
                .into_iter()
                .map(|label| label.name)
                .filter(|name| !name.trim().is_empty())
                .collect::<Vec<_>>();
            labels.sort();
            labels.dedup();
            GithubIssueSummary {
                number: issue.number,
                title: issue.title,
                state: issue.state,
                author: issue.user.map(|user| user.login),
                labels,
                url: safe_https_url(issue.html_url),
            }
        })
        .collect::<Vec<_>>();
    issues.sort_by_key(|issue| issue.number);
    Ok(GithubIssueList {
        repository: repository.to_owned(),
        issues,
    })
}

fn safe_https_url(value: Option<String>) -> Option<String> {
    value.filter(|url| {
        url.starts_with("https://")
            && !url.contains('@')
            && !url.to_ascii_lowercase().contains("token=")
    })
}

fn validate_hostname(value: &str) -> Result<(), String> {
    if value.is_empty() || value.starts_with('-') || value.chars().any(char::is_whitespace) {
        return Err("invalid GitHub hostname".to_owned());
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_issues_and_excludes_pull_requests() {
        let result = parse_issues(
            "octo/repo",
            r#"[
                {"number":2,"title":"Bug","state":"open","html_url":"https://github.com/octo/repo/issues/2","user":{"login":"octocat"},"labels":[{"name":"bug"},{"name":"urgent"}],"pull_request":null},
                {"number":1,"title":"PR","state":"open","html_url":"https://github.com/octo/repo/pull/1","user":{"login":"octocat"},"labels":[],"pull_request":{"url":"https://api.github.com/repos/octo/repo/pulls/1"}}
            ]"#,
        )
        .expect("issues should parse");
        assert_eq!(result.issues.len(), 1);
        assert_eq!(result.issues[0].number, 2);
        assert_eq!(result.issues[0].labels, vec!["bug", "urgent"]);
    }

    #[test]
    fn removes_unsafe_issue_urls() {
        let result = parse_issues(
            "octo/repo",
            r#"[{"number":3,"title":"Secret","state":"open","html_url":"https://user@example.com/issues/3?token=secret","user":null,"labels":[],"pull_request":null}]"#,
        )
        .expect("issues should parse");
        assert_eq!(result.issues[0].url, None);
    }

    #[test]
    fn validates_issue_state_and_repository_shape() {
        assert!(validate_repository("octo/repo").is_ok());
        assert!(validate_repository("octo/../secret").is_err());
        assert!(validate_hostname("github.com").is_ok());
        assert!(validate_hostname("--help").is_err());
    }
}
