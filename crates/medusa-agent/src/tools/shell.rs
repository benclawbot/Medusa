use std::path::Path;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

use crate::{
    policy::{sandboxed_command, validate_shell_command},
    tools::format_command_output,
};

pub(crate) fn run(repo: &Path, program: &str, args: &[String]) -> MedusaResult<String> {
    validate_shell_command(program, args)?;
    let output = sandboxed_command(repo, program, args)?;
    let evidence = format_command_output(program, args, &output.stdout, &output.stderr);
    if output.status.success() {
        Ok(evidence.join("\n"))
    } else {
        Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            evidence.join("\n"),
        ))
    }
}
