use std::{
    io::Read,
    process::{Command, Stdio},
};

use serde::Serialize;

const MAX_LOG_BYTES: usize = 256 * 1024;
const READ_LIMIT_BYTES: u64 = (MAX_LOG_BYTES + 1) as u64;

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
    let (bytes, source_truncated) = run_gh_api(&hostname, &endpoint)?;
    Ok(sanitize_log(
        &repository,
        job_id,
        &bytes,
        source_truncated,
    ))
}

fn run_gh_api(hostname: &str, endpoint: &str) -> Result<(Vec<u8>, bool), String> {
    let mut child = Command::new("gh")
        .args(["api", "--hostname", hostname, endpoint])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "GitHub CLI is not installed".to_owned()
            } else {
                "cannot inspect GitHub Actions job log".to_owned()
            }
        })?;

    let mut bytes = Vec::with_capacity(MAX_LOG_BYTES + 1);
    child
        .stdout
        .take()
        .ok_or_else(|| "cannot inspect GitHub Actions job log".to_owned())?
        .take(READ_LIMIT_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot inspect GitHub Actions job log".to_owned())?;

    let truncated = bytes.len() > MAX_LOG_BYTES;
    if truncated {
        bytes.truncate(MAX_LOG_BYTES);
        let _ = child.kill();
        let _ = child.wait();
        return Ok((bytes, true));
    }

    let status = child
        .wait()
        .map_err(|_| "cannot inspect GitHub Actions job log".to_owned())?;
    if status.success() {
        Ok((bytes, false))
    } else {
        Err("cannot inspect GitHub Actions job log".to_owned())
    }
}

fn sanitize_log(
    repository: &str,
    job_id: u64,
    bytes: &[u8],
    source_truncated: bool,
) -> GithubActionsLogResult {
    let decoded = String::from_utf8_lossy(bytes).replace('\0', "");
    let mut redacted_lines = 0;
    let mut output = String::new();
    let mut truncated = source_truncated;

    for line in decoded.lines() {
        let cleaned = redact_line(line, &mut redacted_lines);
        if !push_bounded_line(&mut output, &cleaned) {
            truncated = true;
            break;
        }
    }

    GithubActionsLogResult {
        repository: repository.to_owned(),
        job_id,
        content: output,
        truncated,
        redacted_lines,
    }
}

fn push_bounded_line(output: &mut String, line: &str) -> bool {
    let remaining = MAX_LOG_BYTES.saturating_sub(output.len());
    if remaining == 0 {
        return false;
    }

    let content_limit = remaining.saturating_sub(1);
    if line.len() <= content_limit {
        output.push_str(line);
        output.push('\n');
        return true;
    }

    let end = char_boundary_at_or_before(line, content_limit);
    output.push_str(&line[..end]);
    if output.len() < MAX_LOG_BYTES {
        output.push('\n');
    }
    false
}

fn char_boundary_at_or_before(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn redact_line(line: &str, redacted_lines: &mut usize) -> String {
    let lower = line.to_ascii_lowercase();
    let sensitive = [
        "authorization:",
        "bearer ",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
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
    fn redacts_secret_bearing_lines_and_token_prefixes() {
        let result = sanitize_log(
            "octo/repo",
            42,
            b"safe line\nAuthorization: Bearer secret\nGITHUB_TOKEN=hidden\nghs_app\ngho_oauth\ngithub_pat_fine_grained\ndone\n",
            false,
        );
        assert!(result.content.contains("safe line"));
        assert!(result.content.contains("done"));
        assert!(!result.content.contains("secret"));
        assert!(!result.content.contains("hidden"));
        assert!(!result.content.contains("ghs_app"));
        assert!(!result.content.contains("gho_oauth"));
        assert!(!result.content.contains("github_pat_fine_grained"));
        assert_eq!(result.redacted_lines, 5);
    }

    #[test]
    fn truncates_oversized_single_line_logs() {
        let input = "x".repeat(MAX_LOG_BYTES + 100);
        let result = sanitize_log("octo/repo", 7, input.as_bytes(), true);
        assert!(result.truncated);
        assert!(!result.content.is_empty());
        assert!(result.content.len() <= MAX_LOG_BYTES);
    }

    #[test]
    fn truncation_preserves_utf8_boundaries() {
        let input = "é".repeat(MAX_LOG_BYTES);
        let result = sanitize_log("octo/repo", 8, input.as_bytes(), true);
        assert!(result.truncated);
        assert!(result.content.is_char_boundary(result.content.len()));
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