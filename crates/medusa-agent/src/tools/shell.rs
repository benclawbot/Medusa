use std::path::Path;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

use crate::{
    output_envelope::{OutputMode, adapt_command},
    policy::{sandboxed_command, validate_shell_command},
};

pub(crate) fn run(
    repo: &Path,
    program: &str,
    args: &[String],
    mode: OutputMode,
) -> MedusaResult<String> {
    validate_shell_command(program, args)?;
    run_validated(repo, program, args, mode)
}

pub(crate) fn run_approved(
    repo: &Path,
    program: &str,
    args: &[String],
    mode: OutputMode,
) -> MedusaResult<String> {
    validate_shell_command(program, args)?;
    run_validated(repo, program, args, mode)
}

fn run_validated(
    repo: &Path,
    program: &str,
    args: &[String],
    mode: OutputMode,
) -> MedusaResult<String> {
    let output = sandboxed_command(repo, program, args)?;
    let evidence = adapt_command(
        program,
        args,
        &output.stdout,
        &output.stderr,
        output.status.success(),
        mode,
    )
    .to_string();
    if output.status.success() {
        Ok(evidence)
    } else {
        Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            evidence,
        ))
    }
}
