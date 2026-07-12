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

#[cfg(all(test, unix))]
mod tests {
    use std::{collections::BTreeMap, fs, time::Duration};

    use super::*;

    fn sidecar_script(
        directory: &Path,
        evidence: &BrowserEvidence,
        exit_status: Option<i32>,
    ) -> PathBuf {
        let path = directory.join("sidecar.sh");
        let payload = serde_json::to_string(evidence).expect("serialize evidence");
        let script = match exit_status {
            Some(status) => format!("#!/bin/sh\necho browser-failed >&2\nexit {status}\n"),
            None => format!(
                "#!/bin/sh\nprintf '%s\\n' '{}'\n",
                payload.replace('\'', "'\\''")
            ),
        };
        fs::write(&path, script).expect("sidecar");
        path
    }

    fn evidence(screenshot: PathBuf) -> BrowserEvidence {
        BrowserEvidence {
            url: "https://example.invalid".into(),
            screenshot,
            accessibility_snapshot: "document".into(),
            console_errors: Vec::new(),
            failed_requests: Vec::new(),
            assertions: BTreeMap::from([("expected_text".into(), true)]),
            viewport: "1280x720".into(),
            browser_version: "fixture-1".into(),
            trace: None,
        }
    }

    #[test]
    fn valid_browser_evidence_is_accepted() {
        let directory = tempfile::tempdir().expect("tempdir");
        let screenshot = directory.path().join("shot.png");
        fs::write(&screenshot, b"png").expect("screenshot");
        let expected = evidence(screenshot);
        let sidecar = sidecar_script(directory.path(), &expected, None);

        let actual = verify_browser(
            Path::new("/bin/sh"),
            &sidecar,
            "https://example.invalid",
            &directory.path().join("output"),
            "Example",
            Duration::from_secs(2),
        )
        .expect("browser evidence");
        assert_eq!(actual, expected);
    }

    #[test]
    fn browser_policy_rejects_failed_assertions_and_sidecar_errors() {
        let directory = tempfile::tempdir().expect("tempdir");
        let screenshot = directory.path().join("shot.png");
        fs::write(&screenshot, b"png").expect("screenshot");
        let mut invalid = evidence(screenshot);
        invalid.assertions.insert("expected_text".into(), false);
        let sidecar = sidecar_script(directory.path(), &invalid, None);
        assert!(
            verify_browser(
                Path::new("/bin/sh"),
                &sidecar,
                "https://example.invalid",
                &directory.path().join("output"),
                "Example",
                Duration::from_secs(2),
            )
            .is_err()
        );

        let failing = sidecar_script(directory.path(), &invalid, Some(9));
        assert!(
            verify_browser(
                Path::new("/bin/sh"),
                &failing,
                "https://example.invalid",
                &directory.path().join("output-2"),
                "Example",
                Duration::from_secs(2),
            )
            .is_err()
        );
    }
}
