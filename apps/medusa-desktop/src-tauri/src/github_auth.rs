use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;

const REQUIRED_SCOPES: [&str; 3] = ["repo", "read:org", "workflow"];

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubAuthStatus {
    pub state: GithubAuthState,
    pub hostname: String,
    pub account: Option<String>,
    pub scopes: Vec<String>,
    pub missing_scopes: Vec<String>,
    pub message: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GithubAuthState {
    Ready,
    GhUnavailable,
    Unauthenticated,
    InvalidCredentials,
    MissingScopes,
    UnknownFailure,
}

#[tauri::command]
pub fn runtime_github_auth_status(hostname: Option<String>) -> GithubAuthStatus {
    let hostname = hostname
        .as_deref()
        .unwrap_or("github.com")
        .trim()
        .to_owned();
    if hostname.is_empty() || hostname.starts_with('-') || hostname.chars().any(char::is_whitespace)
    {
        return status(
            GithubAuthState::UnknownFailure,
            hostname,
            None,
            Vec::new(),
            REQUIRED_SCOPES.iter().map(|value| (*value).to_owned()).collect(),
            "invalid GitHub hostname".to_owned(),
        );
    }

    let output = match Command::new("gh")
        .args([
            "auth",
            "status",
            "--active",
            "--hostname",
            hostname.as_str(),
            "--json",
            "hosts",
        ])
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return status(
                GithubAuthState::GhUnavailable,
                hostname,
                None,
                Vec::new(),
                REQUIRED_SCOPES.iter().map(|value| (*value).to_owned()).collect(),
                "GitHub CLI is not installed".to_owned(),
            );
        }
        Err(_) => {
            return status(
                GithubAuthState::UnknownFailure,
                hostname,
                None,
                Vec::new(),
                REQUIRED_SCOPES.iter().map(|value| (*value).to_owned()).collect(),
                "cannot inspect GitHub authentication".to_owned(),
            );
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    parse_auth_status(&hostname, output.status.success(), &stdout, &stderr)
}

fn parse_auth_status(
    hostname: &str,
    success: bool,
    stdout: &str,
    stderr: &str,
) -> GithubAuthStatus {
    let parsed = serde_json::from_str::<Value>(stdout).ok();
    let account = parsed
        .as_ref()
        .and_then(|value| value.get("hosts"))
        .and_then(|hosts| hosts.get(hostname))
        .and_then(active_account);
    let mut scopes = parsed
        .as_ref()
        .and_then(|value| value.get("hosts"))
        .and_then(|hosts| hosts.get(hostname))
        .and_then(active_scopes)
        .unwrap_or_default();
    scopes.sort();
    scopes.dedup();

    let missing_scopes = REQUIRED_SCOPES
        .iter()
        .filter(|required| !scopes.iter().any(|scope| scope == *required))
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();

    if success && account.is_some() && missing_scopes.is_empty() {
        return status(
            GithubAuthState::Ready,
            hostname.to_owned(),
            account,
            scopes,
            missing_scopes,
            "GitHub authentication is ready".to_owned(),
        );
    }

    if success && account.is_some() {
        return status(
            GithubAuthState::MissingScopes,
            hostname.to_owned(),
            account,
            scopes,
            missing_scopes,
            "GitHub authentication is missing required scopes".to_owned(),
        );
    }

    let normalized = stderr.to_ascii_lowercase();
    if normalized.contains("not logged") || normalized.contains("no accounts") {
        return status(
            GithubAuthState::Unauthenticated,
            hostname.to_owned(),
            None,
            scopes,
            missing_scopes,
            "GitHub CLI is not authenticated for this hostname".to_owned(),
        );
    }
    if normalized.contains("invalid")
        || normalized.contains("expired")
        || normalized.contains("revoked")
        || normalized.contains("failed to authenticate")
    {
        return status(
            GithubAuthState::InvalidCredentials,
            hostname.to_owned(),
            account,
            scopes,
            missing_scopes,
            "GitHub credentials are invalid or expired".to_owned(),
        );
    }

    status(
        GithubAuthState::UnknownFailure,
        hostname.to_owned(),
        account,
        scopes,
        missing_scopes,
        "GitHub authentication status could not be determined".to_owned(),
    )
}

fn active_account(host: &Value) -> Option<String> {
    host.as_array()?
        .iter()
        .find(|entry| entry.get("active")?.as_bool() == Some(true))
        .and_then(|entry| entry.get("login"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn active_scopes(host: &Value) -> Option<Vec<String>> {
    host.as_array()?
        .iter()
        .find(|entry| entry.get("active")?.as_bool() == Some(true))
        .and_then(|entry| entry.get("scopes"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
}

fn status(
    state: GithubAuthState,
    hostname: String,
    account: Option<String>,
    scopes: Vec<String>,
    missing_scopes: Vec<String>,
    message: String,
) -> GithubAuthStatus {
    GithubAuthStatus {
        state,
        hostname,
        account,
        scopes,
        missing_scopes,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ready_auth_without_exposing_tokens() {
        let result = parse_auth_status(
            "github.com",
            true,
            r#"{"hosts":{"github.com":[{"login":"octocat","active":true,"scopes":["workflow","repo","read:org"]}]}}"#,
            "",
        );
        assert_eq!(result.state, GithubAuthState::Ready);
        assert_eq!(result.account.as_deref(), Some("octocat"));
        assert!(result.missing_scopes.is_empty());
    }

    #[test]
    fn reports_missing_scopes_deterministically() {
        let result = parse_auth_status(
            "github.com",
            true,
            r#"{"hosts":{"github.com":[{"login":"octocat","active":true,"scopes":["repo"]}]}}"#,
            "",
        );
        assert_eq!(result.state, GithubAuthState::MissingScopes);
        assert_eq!(result.missing_scopes, vec!["read:org", "workflow"]);
    }

    #[test]
    fn classifies_revoked_credentials_without_copying_stderr() {
        let result = parse_auth_status(
            "github.com",
            false,
            "",
            "token ghp_super_secret was revoked",
        );
        assert_eq!(result.state, GithubAuthState::InvalidCredentials);
        assert!(!result.message.contains("ghp_"));
    }

    #[test]
    fn classifies_missing_login() {
        let result = parse_auth_status(
            "github.com",
            false,
            "",
            "You are not logged into any GitHub hosts",
        );
        assert_eq!(result.state, GithubAuthState::Unauthenticated);
    }
}
