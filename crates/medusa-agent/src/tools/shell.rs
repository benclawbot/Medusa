use std::{fs, path::Path, time::Instant};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use sha2::{Digest, Sha256};

#[path = "../tool_telemetry.rs"]
mod tool_telemetry;

use crate::{
    output_envelope::{OutputMode, adapt_command},
    policy::{sandboxed_command, validate_shell_command},
};

pub(crate) fn run(repo: &Path, program: &str, args: &[String]) -> MedusaResult<String> {
    validate_shell_command(program, args)?;
    run_validated(repo, program, args)
}

pub(crate) fn run_approved(repo: &Path, program: &str, args: &[String]) -> MedusaResult<String> {
    validate_shell_command(program, args)?;
    run_validated(repo, program, args)
}

fn run_validated(repo: &Path, program: &str, args: &[String]) -> MedusaResult<String> {
    let started = Instant::now();
    let output = sandboxed_command(repo, program, args)?;
    let command = format!("command={} {}", program, args.join(" "));
    let raw = format!(
        "{command}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let adapted = adapt_command(
        program,
        args,
        &output.stdout,
        &output.stderr,
        output.status.success(),
        OutputMode::Compact,
    );
    let trace = tool_telemetry::ToolExecutionTrace::for_shell(
        program,
        args,
        output.status.success(),
        started.elapsed(),
        raw.len(),
        &adapted,
    );
    let trace_path = tool_telemetry::append_trace(repo, &trace)?;

    let mut evidence = adapted.to_string();
    evidence.push_str(&format!(
        "\n[tool-telemetry path={}; schema_version={}]",
        trace_path.display(),
        trace.schema_version
    ));
    if adapted.expansion_handle.is_some() {
        let path = persist_expansion(repo, &raw)?;
        evidence.push_str(&format!(
            "\n[output-expansion path={}; retrieve_with=fs_read]",
            path.display()
        ));
    }
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

fn persist_expansion(repo: &Path, raw: &str) -> MedusaResult<std::path::PathBuf> {
    let digest = Sha256::digest(format!("shell_run\0{raw}").as_bytes());
    let relative = Path::new(".medusa")
        .join("output-expansions")
        .join(format!("{}.txt", hex::encode(&digest[..8])));
    let absolute = repo.join(&relative);
    if let Some(parent) = absolute.parent() {
        fs::create_dir_all(parent)?;
    }
    if !absolute.exists() {
        fs::write(&absolute, raw)?;
    }
    Ok(relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expansion_path_is_deterministic_and_readable() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let first = persist_expansion(
            directory.path(),
            "command=cargo test\nstdout:\nok\nstderr:\n",
        )
        .expect("persist expansion");
        let second = persist_expansion(
            directory.path(),
            "command=cargo test\nstdout:\nok\nstderr:\n",
        )
        .expect("persist expansion again");
        assert_eq!(first, second);
        assert!(first.starts_with(".medusa/output-expansions"));
        assert_eq!(
            fs::read_to_string(directory.path().join(first)).expect("read expansion"),
            "command=cargo test\nstdout:\nok\nstderr:\n"
        );
    }
}
