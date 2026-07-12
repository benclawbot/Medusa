use std::{
    fs,
    path::{Component, Path},
    process::Command,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

use crate::tools::format_command_output;

/// Runs deterministic repository-specific verification.
pub fn targeted_verification(repo: &Path) -> MedusaResult<VerificationResult> {
    #[cfg(windows)]
    let command = if repo.join("verify.ps1").is_file() {
        Some(("powershell", vec!["-NoProfile", "-File", "verify.ps1"]))
    } else {
        inferred_command(repo)?
    };
    #[cfg(not(windows))]
    let command = inferred_command(repo)?;
    if command.is_none() && repo.join("index.html").is_file() {
        return verify_static_site(repo);
    }
    let Some((program, args)) = command else {
        return Err(MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Environment,
            "no targeted verification command could be inferred",
        ));
    };
    let program = platform_program(program);
    let output = Command::new(program)
        .args(&args)
        .current_dir(repo)
        .output()
        .map_err(|error| command_error(program, error))?;
    let mut evidence = format_command_output(program, &args, &output.stdout, &output.stderr);
    evidence.push(format!("exit_status={}", output.status));
    Ok(VerificationResult {
        passed: output.status.success(),
        evidence,
    })
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

fn verify_static_site(repo: &Path) -> MedusaResult<VerificationResult> {
    let html = fs::read_to_string(repo.join("index.html"))?;
    let mut passed = html.to_ascii_lowercase().contains("<html");
    let mut evidence = vec![
        "static_site=index.html".to_owned(),
        format!("html_document={passed}"),
    ];
    for asset in local_asset_references(&html) {
        let path = Path::new(&asset);
        let safe = !path.is_absolute()
            && !path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            });
        if safe && repo.join(path).is_file() {
            evidence.push(format!("asset_present={asset}"));
        } else {
            passed = false;
            evidence.push(format!("missing_asset={asset}"));
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
        format!("failed to start verification program `{program}`: {error}")
    };
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        message,
    )
}

/// Verification result with exact command evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationResult {
    pub passed: bool,
    pub evidence: Vec<String>,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

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

    #[cfg(windows)]
    #[test]
    fn windows_node_verification_uses_command_shim() {
        assert_eq!(platform_program("npm"), "npm.cmd");
    }
}
