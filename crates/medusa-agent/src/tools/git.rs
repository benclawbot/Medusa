use std::{path::Path, process::Command};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

use crate::tools::format_command_output;

pub(crate) fn checkpoint(repo: &Path, message: &str) -> MedusaResult<String> {
    run_git(repo, &["add", "-A"])?;
    run_git(repo, &["commit", "-m", message])?;
    Ok(format!("checkpoint created: {message}"))
}

fn run_git(repo: &Path, args: &[&str]) -> MedusaResult<()> {
    let output = Command::new("git").args(args).current_dir(repo).output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format_command_output("git", args, &output.stdout, &output.stderr).join("\n"),
        ))
    }
}
