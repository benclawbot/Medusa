from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected exactly one match, found {count}")
    return text.replace(old, new, 1)


config_path = Path("crates/medusa-config/src/lib.rs")
text = config_path.read_text()
text = replace_once(
    text,
    "pub struct VerificationConfig {\n    pub required: bool,\n}",
    "pub struct VerificationConfig {\n    pub required: bool,\n    /// Automatically run browser verification for effective UI changes.\n    pub browser_on_ui_change: bool,\n}",
    "verification config field",
)
text = replace_once(
    text,
    "impl Default for VerificationConfig {\n    fn default() -> Self {\n        Self { required: true }\n    }\n}",
    "impl Default for VerificationConfig {\n    fn default() -> Self {\n        Self {\n            required: true,\n            browser_on_ui_change: true,\n        }\n    }\n}",
    "verification defaults",
)
text = text.replace(
    "            \"version = 1\\n[verification]\\nbrowser_on_ui_change = true\\n\",\n",
    "",
)
config_path.write_text(text)

verification_path = Path("crates/medusa-agent/src/verification.rs")
text = verification_path.read_text()
text = replace_once(
    text,
    "use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};\nuse medusa_intelligence::{CodeIndex, ReviewImpact};",
    "use medusa_browser_client::{BrowserClient, BrowserRequest, BrowserResponse};\nuse medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};\nuse medusa_intelligence::{CodeIndex, ReviewImpact};",
    "browser imports",
)
text = replace_once(
    text,
    "pub(crate) fn targeted_verification_for_paths(\n    repo: &Path,\n    artifact_paths: &[String],\n) -> MedusaResult<VerificationResult> {\n    let changed_paths = artifact_paths.iter().map(PathBuf::from).collect::<Vec<_>>();\n    if !changed_paths.is_empty()\n        && let Some(result) = semantic_verification(repo, &changed_paths)?\n    {\n        return Ok(result);\n    }",
    "pub(crate) fn targeted_verification_for_paths(\n    repo: &Path,\n    artifact_paths: &[String],\n) -> MedusaResult<VerificationResult> {\n    let changed_paths = artifact_paths.iter().map(PathBuf::from).collect::<Vec<_>>();\n    let browser_decision = browser_verification_decision(&changed_paths);\n    if !changed_paths.is_empty()\n        && let Some(mut result) = semantic_verification(repo, &changed_paths)?\n    {\n        append_browser_verification(repo, browser_decision, &mut result)?;\n        return Ok(result);\n    }",
    "targeted verification setup",
)
text = replace_once(
    text,
    "    let output = run_supervised_command(repo, program, &args, VERIFICATION_TIMEOUT)?;\n    Ok(verification_result(program, &args, output))\n}",
    "    let output = run_supervised_command(repo, program, &args, VERIFICATION_TIMEOUT)?;\n    let mut result = verification_result(program, &args, output);\n    append_browser_verification(repo, browser_decision, &mut result)?;\n    Ok(result)\n}",
    "targeted verification result",
)
insert = r'''

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
        _ if changed_paths.iter().any(|path| is_effective_ui_change(path)) => {
            BrowserVerificationDecision::Run
        }
        _ => BrowserVerificationDecision::Skip,
    }
}

fn is_effective_ui_change(path: &Path) -> bool {
    let normalized = path.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
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
    result.evidence.push(format!("browser_override={override_value}"));
    if decision == BrowserVerificationDecision::Skip {
        result.evidence.push("browser_verification=skipped".to_owned());
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
            format!("UI changes require browser verification, but {command} could not start: {error}"),
        )
    })?;

    let navigation = client.request(BrowserRequest::Navigate { url: route.clone() })?;
    let (final_url, status) = match navigation {
        BrowserResponse::Navigate { final_url, status } => (final_url, status),
        BrowserResponse::Error { code, message } => {
            result.passed = false;
            result.evidence.push(format!("browser_error={code}:{message}"));
            return Ok(());
        }
        other => {
            result.passed = false;
            result.evidence.push(format!("browser_unexpected_navigation={other:?}"));
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
            result.evidence.push(format!("browser_snapshot_nonempty={nonempty}"));
            result.evidence.push(format!("browser_snapshot_refs={}", refs.len()));
            result.passed &= nonempty;
        }
        BrowserResponse::Error { code, message } => {
            result.passed = false;
            result.evidence.push(format!("browser_snapshot_error={code}:{message}"));
        }
        other => {
            result.passed = false;
            result.evidence.push(format!("browser_unexpected_snapshot={other:?}"));
        }
    }

    let console = client.request(BrowserRequest::Evaluate {
        expression: "JSON.stringify(globalThis.__MEDUSA_CONSOLE_ERRORS__ || [])".to_owned(),
    })?;
    match console {
        BrowserResponse::Evaluate { value } => {
            let serialized = value.as_str().unwrap_or_else(|| value.to_string().as_str()).to_owned();
            let clean = serialized == "[]" || serialized == "\"[]\"" || serialized == "null";
            result.evidence.push(format!("browser_console_errors={serialized}"));
            result.passed &= clean;
        }
        BrowserResponse::Error { code, message } => {
            result.passed = false;
            result.evidence.push(format!("browser_console_probe_error={code}:{message}"));
        }
        other => {
            result.passed = false;
            result.evidence.push(format!("browser_unexpected_console_probe={other:?}"));
        }
    }

    match client.request(BrowserRequest::Screenshot { full_page: true })? {
        BrowserResponse::Screenshot { format, bytes_base64 } => {
            let directory = repo.join(".medusa/verification/screenshots");
            fs::create_dir_all(&directory)?;
            let path = directory.join(format!("{}.{}", ulid::Ulid::new(), format));
            let bytes = decode_base64(&bytes_base64)?;
            fs::write(&path, bytes)?;
            result.evidence.push(format!("browser_screenshot={}", path.display()));
        }
        BrowserResponse::Error { code, message } => {
            result.passed = false;
            result.evidence.push(format!("browser_screenshot_error={code}:{message}"));
        }
        other => {
            result.passed = false;
            result.evidence.push(format!("browser_unexpected_screenshot={other:?}"));
        }
    }
    result.evidence.push(format!("browser_result={}", if result.passed { "passed" } else { "failed" }));
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
            return Err(MedusaError::new(ErrorCode::InvalidConfiguration, ErrorCategory::Validation, "browser screenshot returned invalid base64"));
        }
        length += 1;
        if length == 4 {
            output.push((chunk[0] << 2) | (chunk[1] >> 4));
            if chunk[2] != 64 { output.push((chunk[1] << 4) | (chunk[2] >> 2)); }
            if chunk[3] != 64 { output.push((chunk[2] << 6) | chunk[3]); }
            length = 0;
        }
    }
    if length != 0 {
        return Err(MedusaError::new(ErrorCode::InvalidConfiguration, ErrorCategory::Validation, "browser screenshot base64 was truncated"));
    }
    Ok(output)
}
'''
text = replace_once(text, "\nfn semantic_verification(\n", insert + "\nfn semantic_verification(\n", "browser verification helpers")

# Fix a temporary string lifetime in the generated console evidence branch.
text = text.replace(
    'let serialized = value.as_str().unwrap_or_else(|| value.to_string().as_str()).to_owned();',
    'let serialized = value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string());',
)

# Add focused policy tests near the existing test module.
marker = "#[cfg(test)]\nmod tests {"
tests = r'''#[cfg(test)]
mod browser_policy_tests {
    use super::*;

    #[test]
    fn classifies_representative_ui_changes() {
        assert!(is_effective_ui_change(Path::new("apps/desktop/src/App.tsx")));
        assert!(is_effective_ui_change(Path::new("web/styles.css")));
        assert!(is_effective_ui_change(Path::new("index.html")));
    }

    #[test]
    fn skips_documentation_generated_and_snapshot_changes() {
        assert!(!is_effective_ui_change(Path::new("docs/browser.md")));
        assert!(!is_effective_ui_change(Path::new("apps/web/__snapshots__/App.snap")));
        assert!(!is_effective_ui_change(Path::new("dist/index.js")));
        assert!(!is_effective_ui_change(Path::new("src/generated/client.ts")));
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

'''
text = replace_once(text, marker, tests + marker, "browser policy tests")
verification_path.write_text(text)

docs_path = Path("docs/CONFIGURATION.md")
docs = docs_path.read_text()
docs = docs.replace(
    "[verification]\nrequired = true\n",
    "[verification]\nrequired = true\nbrowser_on_ui_change = true\n",
)
docs = docs.replace("- `verification.browser_on_ui_change`\n", "")
docs += "\n## Browser verification policy\n\nWhen `verification.browser_on_ui_change` is enabled, effective UI changes automatically require browser verification. Documentation-only, generated, snapshot-only, lockfile, and build-output changes are skipped. Set `MEDUSA_BROWSER_VERIFY=force` or `MEDUSA_BROWSER_VERIFY=skip` for an explicit audited override. A runnable route must be supplied through `MEDUSA_BROWSER_VERIFY_URL`; `MEDUSA_BROWSERD` may override the browser daemon executable. Evidence records the override, tested route, HTTP status, snapshot assertions, screenshot path, console errors, and final browser result.\n"
docs_path.write_text(docs)

print("issue 355 source migration applied")
