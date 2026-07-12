use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};

use crate::support::wait_with_timeout;

/// Browser evidence emitted by the managed Playwright sidecar.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BrowserEvidence {
    pub url: String,
    pub screenshot: PathBuf,
    pub accessibility_snapshot: String,
    pub console_errors: Vec<String>,
    pub failed_requests: Vec<String>,
    pub assertions: BTreeMap<String, bool>,
    pub viewport: String,
    pub browser_version: String,
    pub trace: Option<PathBuf>,
}

/// Runs the versioned Node/Playwright sidecar and validates its evidence.
pub fn verify_browser(
    node: &Path,
    sidecar: &Path,
    url: &str,
    output_directory: &Path,
    expected_text: &str,
    timeout: Duration,
) -> MedusaResult<BrowserEvidence> {
    fs::create_dir_all(output_directory)?;
    let child = Command::new(node)
        .arg(sidecar)
        .arg("--url")
        .arg(url)
        .arg("--output")
        .arg(output_directory)
        .arg("--expected-text")
        .arg(expected_text)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let output = wait_with_timeout(child, timeout)?;
    if !output.status.success() {
        return Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!(
                "browser sidecar failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }
    let evidence: BrowserEvidence = serde_json::from_slice(&output.stdout)?;
    if !evidence.screenshot.exists()
        || !evidence.assertions.values().all(|passed| *passed)
        || !evidence.console_errors.is_empty()
        || !evidence.failed_requests.is_empty()
    {
        return Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            "browser evidence did not satisfy verification policy",
        ));
    }
    Ok(evidence)
}
