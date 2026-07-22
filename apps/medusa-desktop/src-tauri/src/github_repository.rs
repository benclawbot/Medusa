use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubRepositoryAccess {
    pub state: GithubRepositoryAccessState,
    pub hostname: String,
    pub repository: String,
    pub visibility: Option<String>,
    pub default_branch: Option<String>,
    pub permissions: Vec<String>,
    pub message: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GithubRepositoryAccessState {
    Ready,
    GhUnavailable,
    AuthenticationRequired,
    NotFound,
    Forbidden,
    UnknownFailure,
}

#[derive(Debug, Deserialize)]
struct RepositoryResponse {
    full_name: String,
    visibility: Option<String>,
    private: Option<bool>,
    default_branch: Option<String>,
    permissions: Option<RepositoryPermissions>,
}

#[derive(Debug, Deserialize)]
struct RepositoryPermissions {
    admin: Option<bool>,
    maintain: Option<bool>,
    push: Option<bool>,
    triage: Option<bool>,
    pull: Option<bool>,
}

#[tauri::command]
pub fn runtime_github_repository_access(
    repository: String,
    hostname: Option<String>,
) -> GithubRepositoryAccess {
    let hostname = hostname
        .as_deref()
        .unwrap_or("github.com")
        .trim()
        .to_owned();
    let repository = repository.trim().to_owned();

    if !valid_hostname(&hostname) || !valid_repository(&repository) {
        return access(
            GithubRepositoryAccessState::UnknownFailure,
            hostname,
            repository,
            None,
            None,
            Vec::new(),
            "invalid GitHub repository or hostname".to_owned(),
        );
    }

    let endpoint = format!("repos/{repository}");
    let output = match Command::new("gh")
        .args(["api", "--hostname", hostname.as_str(), endpoint.as_str()])
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return access(
                GithubRepositoryAccessState::GhUnavailable,
                hostname,
                repository,
                None,
                None,
                Vec::new(),
                "GitHub CLI is not installed".to_owned(),
            );
        }
        Err(_) => {
            return access(
                GithubRepositoryAccessState::UnknownFailure,
                hostname,
                repository,
                None,
                None,
                Vec::new(),
                "cannot verify GitHub repository access".to_owned(),
            );
        }
    };

    parse_repository_access(
        &hostname,
        &repository,
        output.status.success(),
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
    )
}

fn parse_repository_access(
    hostname: &str,
    requested_repository: &str,
    success: bool,
    stdout: &str,
    stderr: &str,
) -> GithubRepositoryAccess {
    if success {
        if let Ok(response) = serde_json::from_str::<RepositoryResponse>(stdout) {
            let visibility = response.visibility.or_else(|| {
                response.private.map(|is_private| {
                    if is_private {
                        "private".to_owned()
                    } else {
                        "public".to_owned()
                    }
                })
            });
            return access(
                GithubRepositoryAccessState::Ready,
                hostname.to_owned(),
                response.full_name,
                visibility,
                response.default_branch,
                permission_names(response.permissions.as_ref()),
                "GitHub repository access is ready".to_owned(),
            );
        }
        return access(
            GithubRepositoryAccessState::UnknownFailure,
            hostname.to_owned(),
            requested_repository.to_owned(),
            None,
            None,
            Vec::new(),
            "GitHub repository response could not be read".to_owned(),
        );
    }

    let normalized = stderr.to_ascii_lowercase();
    let (state, message) = if normalized.contains("not logged")
        || normalized.contains("authentication")
        || normalized.contains("requires authentication")
    {
        (
            GithubRepositoryAccessState::AuthenticationRequired,
            "GitHub authentication is required for this repository",
        )
    } else if normalized.contains("not found") || normalized.contains("404") {
        (
            GithubRepositoryAccessState::NotFound,
            "GitHub repository was not found or is inaccessible",
        )
    } else if normalized.contains("forbidden") || normalized.contains("403") {
        (
            GithubRepositoryAccessState::Forbidden,
            "GitHub account does not have permission for this repository",
        )
    } else {
        (
            GithubRepositoryAccessState::UnknownFailure,
            "GitHub repository access could not be determined",
        )
    };

    access(
        state,
        hostname.to_owned(),
        requested_repository.to_owned(),
        None,
        None,
        Vec::new(),
        message.to_owned(),
    )
}

fn permission_names(value: Option<&RepositoryPermissions>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let candidates = [
        ("admin", value.admin),
        ("maintain", value.maintain),
        ("push", value.push),
        ("triage", value.triage),
        ("pull", value.pull),
    ];
    candidates
        .into_iter()
        .filter(|(_, enabled)| enabled.unwrap_or(false))
        .map(|(name, _)| name.to_owned())
        .collect()
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

fn access(
    state: GithubRepositoryAccessState,
    hostname: String,
    repository: String,
    visibility: Option<String>,
    default_branch: Option<String>,
    permissions: Vec<String>,
    message: String,
) -> GithubRepositoryAccess {
    GithubRepositoryAccess {
        state,
        hostname,
        repository,
        visibility,
        default_branch,
        permissions,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_private_repository_access_and_permissions() {
        let result = parse_repository_access(
            "github.com",
            "octo/private",
            true,
            r#"{"full_name":"octo/private","visibility":"private","default_branch":"main","permissions":{"admin":false,"maintain":false,"push":true,"triage":true,"pull":true}}"#,
            "",
        );
        assert_eq!(result.state, GithubRepositoryAccessState::Ready);
        assert_eq!(result.visibility.as_deref(), Some("private"));
        assert_eq!(result.default_branch.as_deref(), Some("main"));
        assert_eq!(result.permissions, vec!["push", "triage", "pull"]);
    }

    #[test]
    fn classifies_inaccessible_repository_without_copying_secret_output() {
        let result = parse_repository_access(
            "github.com",
            "octo/private",
            false,
            "",
            "HTTP 404: token ghp_super_secret cannot access repository",
        );
        assert_eq!(result.state, GithubRepositoryAccessState::NotFound);
        assert!(!result.message.contains("ghp_"));
    }

    #[test]
    fn rejects_malformed_repository_names() {
        assert!(!valid_repository("octo"));
        assert!(!valid_repository("octo/repo/extra"));
        assert!(!valid_repository("octo/../secret"));
        assert!(valid_repository("octo/repo-name"));
    }
}
