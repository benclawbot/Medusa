use std::{path::{Path, PathBuf}, process::Command};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

/// Runs the canonical formatter for changed file types.
pub fn format_changed(repo: &Path, changed_paths: &[PathBuf]) -> MedusaResult<Vec<String>> {
    let mut evidence = Vec::new();
    if changed_paths
        .iter()
        .any(|path| path.extension().is_some_and(|ext| ext == "rs"))
    {
        let output = Command::new("cargo")
            .args(["fmt", "--all"])
            .current_dir(repo)
            .output()?;
        evidence.push(format!("cargo fmt --all: {}", output.status));
        if !output.status.success() {
            return Err(MedusaError::new(
                ErrorCode::ToolExecutionFailed,
                ErrorCategory::Execution,
                String::from_utf8_lossy(&output.stderr),
            ));
        }
    }
    Ok(evidence)
}
