use std::{path::Path, process::Command};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

use crate::tools::format_command_output;

/// Runs deterministic repository-specific verification.
pub fn targeted_verification(repo: &Path) -> MedusaResult<VerificationResult> {
    let command = if repo.join("verify.sh").is_file() {
        Some(("bash", vec!["verify.sh"]))
    } else if repo.join("Cargo.toml").is_file() {
        Some(("cargo", vec!["test", "--all-targets", "--all-features"]))
    } else if repo.join("package.json").is_file() {
        Some(("npm", vec!["test", "--", "--runInBand"]))
    } else if repo.join("pyproject.toml").is_file() {
        Some(("python", vec!["-m", "pytest"]))
    } else {
        None
    };
    let Some((program, args)) = command else {
        return Err(MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Environment,
            "no targeted verification command could be inferred",
        ));
    };
    let output = Command::new(program)
        .args(&args)
        .current_dir(repo)
        .output()?;
    let mut evidence = format_command_output(program, &args, &output.stdout, &output.stderr);
    evidence.push(format!("exit_status={}", output.status));
    Ok(VerificationResult {
        passed: output.status.success(),
        evidence,
    })
}

/// Verification result with exact command evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationResult {
    pub passed: bool,
    pub evidence: Vec<String>,
}
