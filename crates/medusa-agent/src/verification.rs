use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use medusa_browser_client::{BrowserClient, BrowserRequest, BrowserResponse};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_intelligence::{CodeIndex, ReviewImpact};

const VERIFICATION_TIMEOUT: Duration = Duration::from_secs(300);
const VERIFICATION_POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_STREAM_BYTES: usize = 16 * 1024;
const MAX_STREAM_LINES: usize = 80;

/// Runs deterministic repository-specific verification.
pub fn targeted_verification(repo: &Path) -> MedusaResult<VerificationResult> {
    targeted_verification_for_paths(repo, &[])
}

pub(crate) fn targeted_verification_for_paths(
    repo: &Path,
    artifact_paths: &[String],
) -> MedusaResult<VerificationResult> {
    let changed_paths = artifact_paths.iter().map(PathBuf::from).collect::<Vec<_>>();
    let browser_decision = browser_verification_decision(&changed_paths);
    if !changed_paths.is_empty()
        && let Some(mut result) = semantic_verification(repo, &changed_paths)?
    {
        append_browser_verification(repo, browser_decision, &mut result)?;
        return Ok(result);
    }

    #[cfg(windows)]
    let command = if repo.join("verify.ps1").is_file() {
        Some(("powershell", vec!["-NoProfile", "-File", "verify.ps1"]))
    } else {
        inferred_command(repo)?
    };
    #[cfg(not(windows))]
    let command = inferred_command(repo)?;
    if command.is_none() {
        if !artifact_paths.is_empty() {
            return verify_standalone_artifacts(repo, artifact_paths);
        }
        if repo.join("index.html").is_file() {
            return verify_static_site(repo, Path::new("index.html"));
        }
    }
    let Some((program, args)) = command else {
        return Err(MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Environment,
            "no targeted verification command could be inferred",
        ));
    };
    let program = platform_program(program);
    let output = run_supervised_command(repo, program, &args, VERIFICATION_TIMEOUT)?;
    let mut result = verification_result(program, &args, output);
    append_browser_verification(repo, browser_decision, &mut result)?;
    Ok(result)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrowserVerificationDecision {
    Run,
    Skip,
}

fn browser_verification_decision(changed_paths: &[PathBuf]) -> BrowserVerificationDecision {
    match std::env::var("MEDUSA_BROWSER_VERIFY")
        .unwrap_or_else(|_| "auto".to_owned())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "force" | "always" | "1" | "true" => BrowserVerificationDecision::Run,
        "skip" | "never" | "0" | "false" => BrowserVerificationDecision::Skip,
        _ if changed_paths
            .iter()
            .any(|path| is_effective_ui_change(path)) =>
        {
            BrowserVerificationDecision::Run
        }
        _ => BrowserVerificationDecision::Skip,
    }
}

fn is_effective_ui_change(path: &Path) -> bool {
    let normalized = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    if normalized.starts_with("docs/")
        || normalized.contains("/docs/")
        || normalized.contains("__snapshots__")
        || normalized.ends_with(".snap")
        || normalized.ends_with(".md")
        || normalized.ends_with(".lock")
        || normalized.contains("/generated/")
        || normalized.starts_with("dist/")
        || normalized.starts_with("target/")
    {
        return false;
    }
    normalized.starts_with("apps/")
        || normalized.starts_with("web/")
        || normalized.starts_with("frontend/")
        || normalized.starts_with("src-tauri/")
        || normalized.ends_with(".html")
        || normalized.ends_with(".css")
        || normalized.ends_with(".scss")
        || normalized.ends_with(".tsx")
        || normalized.ends_with(".jsx")
        || normalized.ends_with(".vue")
        || normalized.ends_with(".svelte")
}

fn append_browser_verification(
    repo: &Path,
    decision: BrowserVerificationDecision,
    result: &mut VerificationResult,
) -> MedusaResult<()> {
    let override_value = std::env::var("MEDUSA_BROWSER_VERIFY").unwrap_or_else(|_| "auto".into());
    result
        .evidence
        .push(format!("browser_override={override_value}"));
    if decision == BrowserVerificationDecision::Skip {
        result
            .evidence
            .push("browser_verification=skipped".to_owned());
        return Ok(());
    }

    let route = std::env::var("MEDUSA_BROWSER_VERIFY_URL").map_err(|_| {
        MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Environment,
            "UI changes require browser verification, but MEDUSA_BROWSER_VERIFY_URL is not set; start the dev server and provide a runnable route",
        )
    })?;
    let command = std::env::var("MEDUSA_BROWSERD").unwrap_or_else(|_| "medusa-browserd".into());
    let mut client = BrowserClient::spawn(&command).map_err(|error| {
        MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Environment,
            format!(
                "UI changes require browser verification, but {command} could not start: {error}"
            ),
        )
    })?;

    let navigation = client.request(BrowserRequest::Navigate { url: route.clone() })?;
    let (final_url, status) = match navigation {
        BrowserResponse::Navigate { final_url, status } => (final_url, status),
        BrowserResponse::Error { code, message } => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_error={code}:{message}"));
            return Ok(());
        }
        other => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_unexpected_navigation={other:?}"));
            return Ok(());
        }
    };
    result.evidence.push(format!("browser_route={final_url}"));
    result.evidence.push(format!("browser_status={status}"));
    result.passed &= status < 400;

    let snapshot = client.request(BrowserRequest::Snapshot)?;
    match snapshot {
        BrowserResponse::Snapshot { text, refs } => {
            let nonempty = !text.trim().is_empty();
            result
                .evidence
                .push(format!("browser_snapshot_nonempty={nonempty}"));
            result
                .evidence
                .push(format!("browser_snapshot_refs={}", refs.len()));
            result.passed &= nonempty;
        }
        BrowserResponse::Error { code, message } => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_snapshot_error={code}:{message}"));
        }
        other => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_unexpected_snapshot={other:?}"));
        }
    }

    let console = client.request(BrowserRequest::Evaluate {
        expression: "JSON.stringify(globalThis.__MEDUSA_CONSOLE_ERRORS__ || [])".to_owned(),
    })?;
    match console {
        BrowserResponse::Evaluate { value } => {
            let serialized = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            let clean = serialized == "[]" || serialized == "\"[]\"" || serialized == "null";
            result
                .evidence
                .push(format!("browser_console_errors={serialized}"));
            result.passed &= clean;
        }
        BrowserResponse::Error { code, message } => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_console_probe_error={code}:{message}"));
        }
        other => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_unexpected_console_probe={other:?}"));
        }
    }

    match client.request(BrowserRequest::Screenshot { full_page: true })? {
        BrowserResponse::Screenshot {
            format,
            bytes_base64,
        } => {
            let directory = repo.join(".medusa/verification/screenshots");
            fs::create_dir_all(&directory)?;
            let path = directory.join(format!("{}.{}", ulid::Ulid::new(), format));
            let bytes = decode_base64(&bytes_base64)?;
            fs::write(&path, bytes)?;
            result
                .evidence
                .push(format!("browser_screenshot={}", path.display()));
        }
        BrowserResponse::Error { code, message } => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_screenshot_error={code}:{message}"));
        }
        other => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_unexpected_screenshot={other:?}"));
        }
    }
    result.evidence.push(format!(
        "browser_result={}",
        if result.passed { "passed" } else { "failed" }
    ));
    Ok(())
}

fn decode_base64(input: &str) -> MedusaResult<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = Vec::new();
    let mut chunk = [0u8; 4];
    let mut length = 0;
    for byte in input.bytes().filter(|byte| !byte.is_ascii_whitespace()) {
        if byte == b'=' {
            chunk[length] = 64;
        } else if let Some(index) = TABLE.iter().position(|candidate| *candidate == byte) {
            chunk[length] = index as u8;
        } else {
            return Err(MedusaError::new(
                ErrorCode::InvalidConfiguration,
                ErrorCategory::Validation,
                "browser screenshot returned invalid base64",
            ));
        }
        length += 1;
        if length == 4 {
            output.push((chunk[0] << 2) | (chunk[1] >> 4));
            if chunk[2] != 64 {
                output.push((chunk[1] << 4) | (chunk[2] >> 2));
            }
            if chunk[3] != 64 {
                output.push((chunk[2] << 6) | chunk[3]);
            }
            length = 0;
        }
    }
    if length != 0 {
        return Err(MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            "browser screenshot base64 was truncated",
        ));
    }
    Ok(output)
}

fn semantic_verification(
    repo: &Path,
    changed_paths: &[PathBuf],
) -> MedusaResult<Option<VerificationResult>> {
    let index = match CodeIndex::build(repo) {
        Ok(index) => index,
        Err(_) => return Ok(None),
    };
    let impact = ReviewImpact::analyze(&index, changed_paths);
    if impact.validation.commands.is_empty() {
        return Ok(None);
    }

    let mut passed = true;
    let mut evidence = vec![impact.reviewer_context()];
    evidence.extend(
        impact
            .validation
            .reasons
            .iter()
            .map(|reason| format!("validation_reason={reason}")),
    );

    for command in &impact.validation.commands {
        let argv = parse_command_line(command).ok_or_else(|| {
            MedusaError::new(
                ErrorCode::InvalidConfiguration,
                ErrorCategory::Validation,
                format!("invalid semantic verification command: {command}"),
            )
        })?;
        let (program, args) = argv.split_first().ok_or_else(|| {
            MedusaError::new(
                ErrorCode::InvalidConfiguration,
                ErrorCategory::Validation,
                "semantic verification command was empty",
            )
        })?;
        let program = platform_program(program);
        let output = run_supervised_command(repo, program, args, VERIFICATION_TIMEOUT)?;
        let result = verification_result(program, args, output);
        evidence.push(format!("semantic_command={command}"));
        evidence.extend(result.evidence);
        passed &= result.passed;
    }

    Ok(Some(VerificationResult { passed, evidence }))
}

fn run_supervised_command<S: AsRef<std::ffi::OsStr>>(
    repo: &Path,
    program: &str,
    args: &[S],
    timeout: Duration,
) -> MedusaResult<SupervisedOutput> {
    let id = ulid::Ulid::new();
    let stdout_path = std::env::temp_dir().join(format!("medusa-verify-{id}.stdout"));
    let stderr_path = std::env::temp_dir().join(format!("medusa-verify-{id}.stderr"));
    let stdout_file = File::create(&stdout_path)?;
    let stderr_file = File::create(&stderr_path)?;

    let started = Instant::now();
    let mut child = Command::new(program)
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .map_err(|error| command_error(program, error))?;

    let (status, timed_out) = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| command_error(program, error))?
        {
            break (status, false);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let status = child
                .wait()
                .map_err(|error| command_error(program, error))?;
            break (status, true);
        }
        thread::sleep(VERIFICATION_POLL_INTERVAL);
    };

    let stdout = read_and_remove(&stdout_path)?;
    let stderr = read_and_remove(&stderr_path)?;
    Ok(SupervisedOutput {
        status,
        stdout,
        stderr,
        timed_out,
        duration: started.elapsed(),
    })
}

fn read_and_remove(path: &Path) -> MedusaResult<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    let _ = fs::remove_file(path);
    Ok(bytes)
}

fn verification_result<S: AsRef<std::ffi::OsStr>>(
    program: &str,
    args: &[S],
    output: SupervisedOutput,
) -> VerificationResult {
    let mut evidence = vec![format!("program={program}")];
    evidence.extend(
        args.iter()
            .map(|arg| format!("arg={}", arg.as_ref().to_string_lossy())),
    );
    evidence.push(format!("duration_ms={}", output.duration.as_millis()));
    evidence.push(format!("timed_out={}", output.timed_out));
    append_bounded_stream_evidence(&mut evidence, "stdout", &output.stdout);
    append_bounded_stream_evidence(&mut evidence, "stderr", &output.stderr);
    evidence.push(format!("exit_status={}", output.status));
    VerificationResult {
        passed: !output.timed_out && output.status.success(),
        evidence,
    }
}

fn append_bounded_stream_evidence(evidence: &mut Vec<String>, stream: &str, bytes: &[u8]) {
    let byte_truncated = bytes.len() > MAX_STREAM_BYTES;
    let tail = if byte_truncated {
        &bytes[bytes.len() - MAX_STREAM_BYTES..]
    } else {
        bytes
    };
    let text = String::from_utf8_lossy(tail);
    let lines = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    let line_truncated = lines.len() > MAX_STREAM_LINES;
    let start = lines.len().saturating_sub(MAX_STREAM_LINES);
    for line in &lines[start..] {
        evidence.push(format!("{stream}={line}"));
    }
    evidence.push(format!("{stream}_bytes={}", bytes.len()));
    evidence.push(format!(
        "{stream}_truncated={}",
        byte_truncated || line_truncated
    ));
}

fn parse_command_line(command: &str) -> Option<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;

    for character in command.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            } else {
                current.push(character);
            }
            continue;
        }
        if character.is_whitespace() && quote.is_none() {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if escaped || quote.is_some() {
        return None;
    }
    if !current.is_empty() {
        args.push(current);
    }
    Some(args)
}

fn inferred_command(repo: &Path) -> MedusaResult<Option<(&'static str, Vec<&'static str>)>> {
    let command = if repo.join("verify.sh").is_file() {
        Some(("bash", vec!["verify.sh"]))
    } else if repo.join("Cargo.toml").is_file() {
        Some(("cargo", vec!["test", "--all-targets", "--all-features"]))
    } else if repo.join("package.json").is_file() && package_has_test_script(repo)? {
        Some(("npm", vec!["test", "--", "--runInBand"]))
    } else if repo.join("pyproject.toml").is_file() {
        Some(("python", vec!["-m", "pytest"]))
    } else {
        None
    };
    Ok(command)
}

fn package_has_test_script(repo: &Path) -> MedusaResult<bool> {
    let package: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(repo.join("package.json"))?)?;
    Ok(package
        .get("scripts")
        .and_then(|scripts| scripts.get("test"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|script| !script.trim().is_empty()))
}

fn verify_static_site(repo: &Path, entry: &Path) -> MedusaResult<VerificationResult> {
    let html = fs::read_to_string(repo.join(entry))?;
    let mut passed = html.to_ascii_lowercase().contains("<html");
    let mut evidence = vec![
        format!("static_site={}", entry.display()),
        format!("html_document={passed}"),
    ];
    let base = entry.parent().unwrap_or_else(|| Path::new(""));
    for asset in local_asset_references(&html) {
        let path = Path::new(&asset);
        let safe = !path.is_absolute()
            && !path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            });
        if safe && repo.join(base).join(path).is_file() {
            evidence.push(format!("asset_present={asset}"));
        } else {
            passed = false;
            evidence.push(format!("missing_asset={asset}"));
        }
    }
    Ok(VerificationResult { passed, evidence })
}

fn verify_standalone_artifacts(
    repo: &Path,
    artifact_paths: &[String],
) -> MedusaResult<VerificationResult> {
    let mut passed = true;
    let mut evidence = Vec::new();
    let unique = artifact_paths
        .iter()
        .map(PathBuf::from)
        .collect::<BTreeSet<_>>();
    for relative in unique {
        let safe = !relative.is_absolute()
            && !relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            });
        if !safe {
            passed = false;
            evidence.push(format!("unsafe_artifact={}", relative.display()));
            continue;
        }
        let absolute = repo.join(&relative);
        if absolute.is_dir() {
            evidence.push(format!("directory_present={}", relative.display()));
        } else if absolute.is_file()
            && relative
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("html"))
        {
            let result = verify_static_site(repo, &relative)?;
            passed &= result.passed;
            evidence.extend(result.evidence);
        } else if absolute.is_file() {
            let nonempty = absolute.metadata()?.len() > 0;
            passed &= nonempty;
            evidence.push(format!("artifact_present={}", relative.display()));
            evidence.push(format!("artifact_nonempty={nonempty}"));
        } else {
            passed = false;
            evidence.push(format!("missing_artifact={}", relative.display()));
        }
    }
    Ok(VerificationResult { passed, evidence })
}

fn local_asset_references(html: &str) -> Vec<String> {
    let mut assets = Vec::new();
    for attribute in ["href", "src"] {
        for quote in ['"', '\''] {
            let marker = format!("{attribute}={quote}");
            let mut remaining = html;
            while let Some((_, after_marker)) = remaining.split_once(&marker) {
                let Some((value, after_value)) = after_marker.split_once(quote) else {
                    break;
                };
                remaining = after_value;
                let value = value.split(['?', '#']).next().unwrap_or_default();
                if !value.is_empty()
                    && !value.starts_with('#')
                    && !value.starts_with("//")
                    && !value.contains("://")
                    && !value.starts_with("data:")
                    && !value.starts_with("mailto:")
                    && !value.starts_with("javascript:")
                {
                    assets.push(value.to_owned());
                }
            }
        }
    }
    assets.sort();
    assets.dedup();
    assets
}

#[cfg(windows)]
fn platform_program(program: &str) -> &str {
    match program {
        "npm" => "npm.cmd",
        "python" => "python.exe",
        "cargo" => "cargo.exe",
        "bash" => "bash.exe",
        "powershell" => "powershell.exe",
        _ => program,
    }
}

#[cfg(not(windows))]
fn platform_program(program: &str) -> &str {
    program
}

fn command_error(program: &str, error: std::io::Error) -> MedusaError {
    let message = if error.kind() == std::io::ErrorKind::NotFound {
        format!("verification program `{program}` was not found on PATH")
    } else {
        format!("failed to run verification program `{program}`: {error}")
    };
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        message,
    )
}

struct SupervisedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
    duration: Duration,
}

/// Verification result with bounded command evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationResult {
    pub passed: bool,
    pub evidence: Vec<String>,
}

#[cfg(test)]
mod browser_policy_tests {
    use super::*;

    #[test]
    fn classifies_representative_ui_changes() {
        assert!(is_effective_ui_change(Path::new(
            "apps/desktop/src/App.tsx"
        )));
        assert!(is_effective_ui_change(Path::new("web/styles.css")));
        assert!(is_effective_ui_change(Path::new("index.html")));
    }

    #[test]
    fn skips_documentation_generated_and_snapshot_changes() {
        assert!(!is_effective_ui_change(Path::new("docs/browser.md")));
        assert!(!is_effective_ui_change(Path::new(
            "apps/web/__snapshots__/App.snap"
        )));
        assert!(!is_effective_ui_change(Path::new("dist/index.js")));
        assert!(!is_effective_ui_change(Path::new(
            "src/generated/client.ts"
        )));
    }

    #[test]
    fn manual_override_is_auditable() {
        unsafe { std::env::set_var("MEDUSA_BROWSER_VERIFY", "force") };
        assert_eq!(
            browser_verification_decision(&[PathBuf::from("README.md")]),
            BrowserVerificationDecision::Run
        );
        unsafe { std::env::set_var("MEDUSA_BROWSER_VERIFY", "skip") };
        assert_eq!(
            browser_verification_decision(&[PathBuf::from("apps/web/App.tsx")]),
            BrowserVerificationDecision::Skip
        );
        unsafe { std::env::remove_var("MEDUSA_BROWSER_VERIFY") };
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn parses_generated_validation_commands_without_a_shell() {
        assert_eq!(
            parse_command_line("cargo test -p widget --test api"),
            Some(vec![
                "cargo".to_owned(),
                "test".to_owned(),
                "-p".to_owned(),
                "widget".to_owned(),
                "--test".to_owned(),
                "api".to_owned(),
            ])
        );
        assert_eq!(
            parse_command_line("python -m pytest \"tests/my test.py\""),
            Some(vec![
                "python".to_owned(),
                "-m".to_owned(),
                "pytest".to_owned(),
                "tests/my test.py".to_owned(),
            ])
        );
    }

    #[test]
    fn bounded_stream_preserves_failure_tail() {
        let input = (0..120)
            .map(|line| format!("line-{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut evidence = Vec::new();

        append_bounded_stream_evidence(&mut evidence, "stderr", input.as_bytes());

        assert!(evidence.iter().any(|line| line == "stderr=line-119"));
        assert!(!evidence.iter().any(|line| line == "stderr=line-0"));
        assert!(evidence.iter().any(|line| line == "stderr_truncated=true"));
    }

    #[test]
    fn static_site_without_test_script_verifies_locally() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("package.json"), "{}").expect("package");
        fs::write(
            directory.path().join("index.html"),
            "<!doctype html><html><head><link rel=\"stylesheet\" href=\"styles.css\"></head><body><script src=\"script.js\"></script></body></html>",
        )
        .expect("html");
        fs::write(
            directory.path().join("styles.css"),
            "body { color: black; }",
        )
        .expect("css");
        fs::write(directory.path().join("script.js"), "console.log('ready');").expect("js");

        let result = targeted_verification(directory.path()).expect("verification");

        assert!(result.passed);
        assert!(
            result
                .evidence
                .iter()
                .any(|line| line == "static_site=index.html")
        );
    }

    #[test]
    fn static_site_reports_missing_local_assets() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(
            directory.path().join("index.html"),
            "<!doctype html><html><head><link rel=\"stylesheet\" href=\"missing.css\"></head><body></body></html>",
        )
        .expect("html");

        let result = targeted_verification(directory.path()).expect("verification");

        assert!(!result.passed);
        assert!(
            result
                .evidence
                .iter()
                .any(|line| line == "missing_asset=missing.css")
        );
    }

    #[test]
    fn standalone_html_artifact_verifies_without_a_repository_test_command() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(
            directory.path().join("latest-ai-news.html"),
            "<!doctype html><html><head><title>AI news</title></head><body>Current reporting</body></html>",
        )
        .expect("html artifact");

        let result =
            targeted_verification_for_paths(directory.path(), &["latest-ai-news.html".to_owned()])
                .expect("standalone artifact verification");

        assert!(result.passed);
        assert!(
            result
                .evidence
                .iter()
                .any(|line| line == "static_site=latest-ai-news.html")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_node_verification_uses_command_shim() {
        assert_eq!(platform_program("npm"), "npm.cmd");
    }
}
