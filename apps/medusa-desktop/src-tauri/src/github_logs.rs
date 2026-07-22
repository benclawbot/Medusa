use std::process::{Command, Output};

use serde::Serialize;

const MAX_LOG_BYTES: usize = 256 * 1024;

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GithubActionsLogResult {
    pub repository: String,
    pub job_id: u64,
    pub content: String,
    pub truncated: bool,
    pub redacted_lines: usize,
}

#[tauri::command]
pub fn runtime_github_actions_job_log(
    repository: String,
    hostname: Option<String>,
    job_id: u64,
) -> Result<GithubActionsLogResult, String> {
    let repository = repository.trim().to_owned();
    let hostname = hostname
        .as_deref()
        .unwrap_or("github.com")
        .trim()
        .to_owned();
    validate_repository(&repository)?;
    validate_hostname(&hostname)?;
    if job_id == 0 {
        return Err("GitHub Actions job id must be positive".to_owned());
    }

    let endpoint = format!("repos/{repository}/actions/jobs/{job_id}/logs");
    let output = run_gh_api(&hostname, &endpoint)?;
    sanitize_log(&repository, job_id, &output.stdout)
}

fn run_gh_api(hostname: &str, endpoint: &str) -> Result<Output, String> {
    let output = Command::new("gh")
        .args(["api", "--hostname", hostname, endpoint])
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "GitHub CLI is not installed".to_owned()
            } else {
                "cannot inspect GitHub Actions job log".to_owned()
            }
        })?;
    if output.status.success() {
        Ok(output)
    } else {
        Err("cannot inspect GitHub Actions job log".to_owned())
    }
}

fn sanitize_log(
    repository: &str,
    job_id: u64,
    bytes: &[u8],
) -> Result<GithubActionsLogResult, String> {
    let decoded = String::from_utf8_lossy(bytes).replace('\0', "");
    let mut redacted_lines = 0;
    let mut output = String::new();
    for line in decoded.lines() {
        let cleaned = redact_line(line, &mut redacted_lines);
        if output.len() + cleaned.len() + 1 > MAX_LOG_BYTES {
            break;
        }
        output.push_str(&cleaned);
        output.push('\n');
    }
    let truncated = output.len() < decoded.len();
    if output.is_empty() && !decoded.is_empty() {
        return Err("GitHub Actions job log could not be sanitized".to_owned());
    }
    Ok(GithubActionsLogResult {
        repository: repository.to_owned(),
        job_id,
        content: output,
        truncated,
        redacted_lines,
    })
}

fn redact_line(line: &str, redacted_lines: &mut usize) -> String {
    let lower = line.to_ascii_lowercase();
    let sensitive = [
        "authorization:",
        "bearer ",
        "ghp_",
        "github_token",
        "access_token",
        "client_secret",
        "private_key",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    if sensitive {
        *redacted_lines += 1;
        "[REDACTED SECRET-BEARING LOG LINE]".to_owned()
    } else {
        line.to_owned()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secret_bearing_lines() {
        let result = sanitize_log(
            "octo/repo",
            42,
            b"safe line\nAuthorization: Bearer secret\nGITHUB_TOKEN=hidden\ndone\n",
        )
        .expect("log should sanitize");
        assert!(result.content.contains("safe line"));
        assert!(result.content.contains("done"));
        assert!(!result.content.contains("secret"));
        assert!(!result.content.contains("hidden"));
        assert_eq!(result.redacted_lines, 2);
    }

    #[test]
    fn truncates_oversized_logs() {
        let input = "x".repeat(MAX_LOG_BYTES + 100);
        let result = sanitize_log("octo/repo", 7, input.as_bytes()).expect("log should sanitize");
        assert!(result.truncated);
        assert!(result.content.len() <= MAX_LOG_BYTES);
    }

    #[test]
    fn validates_targets() {
        assert!(validate_repository("octo/repo").is_ok());
        assert!(validate_repository("octo/../secret").is_err());
        assert!(validate_hostname("github.com").is_ok());
        assert!(validate_hostname("--help").is_err());
    }
}
