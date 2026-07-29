use std::{fs, path::Path, time::Instant};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde_json::json;
use sha2::{Digest, Sha256};

#[path = "../tool_orchestration.rs"]
mod tool_orchestration;
#[path = "../tool_scheduler.rs"]
mod tool_scheduler;
#[path = "../tool_telemetry.rs"]
mod tool_telemetry;

use crate::{
    output_envelope::{OutputMode, adapt_command},
    policy::{sandboxed_command, validate_shell_command},
};

pub(crate) fn run(
    repo: &Path,
    program: &str,
    args: &[String],
    output_mode: OutputMode,
) -> MedusaResult<String> {
    validate_shell_command(program, args)?;
    run_validated(repo, program, args, output_mode)
}

pub(crate) fn run_approved(
    repo: &Path,
    program: &str,
    args: &[String],
    output_mode: OutputMode,
) -> MedusaResult<String> {
    validate_shell_command(program, args)?;
    run_validated(repo, program, args, output_mode)
}

fn run_validated(
    repo: &Path,
    program: &str,
    args: &[String],
    output_mode: OutputMode,
) -> MedusaResult<String> {
    let mode = output_mode.as_str();
    let command_summary = format!("{} {}", program, args.join(" "));
    let input_summary = format!("{command_summary}\0output_mode={mode}");
    let recommendation = tool_orchestration::recommend("shell_run", &command_summary);
    let mut execution_budget = tool_scheduler::ExecutionBudget::for_turn(1);
    let schedule = execution_budget.schedule_batch(1, 0)?;
    let scheduler_evidence = tool_scheduler::ExecutionBudget::format_schedule(&schedule);
    let input = json!({"program": program, "args": args, "output_mode": mode});
    let call_digest = execution_budget.before_call("shell_run", &input)?;
    let (cached, mut cache_evidence) =
        tool_orchestration::cache_lookup(repo, "shell_run", &input_summary)?;
    if let Some(cached) = cached {
        execution_budget.record_output(&cached)?;
        let verification = execution_budget.verification_for("shell_run", &call_digest, true);
        let orchestration_evidence =
            tool_orchestration::format_evidence(&recommendation, &cache_evidence);
        return Ok(format!(
            "{cached}\n{orchestration_evidence}\n{scheduler_evidence}\n{}",
            tool_scheduler::ExecutionBudget::format_verification(&verification)
        ));
    }

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
        output_mode,
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
    execution_budget.record_output(&evidence)?;
    if output.status.success() {
        cache_evidence =
            tool_orchestration::cache_store(repo, "shell_run", &input_summary, &evidence)?;
    }
    let verification =
        execution_budget.verification_for("shell_run", &call_digest, output.status.success());
    let orchestration_evidence =
        tool_orchestration::format_evidence(&recommendation, &cache_evidence);
    evidence.push('\n');
    evidence.push_str(&orchestration_evidence);
    evidence.push('\n');
    evidence.push_str(&scheduler_evidence);
    evidence.push('\n');
    evidence.push_str(&tool_scheduler::ExecutionBudget::format_verification(
        &verification,
    ));
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

    #[test]
    fn shipped_shell_path_emits_ranking_and_escalation_evidence() {
        let recommendation = tool_orchestration::recommend("shell_run", "cargo test --workspace");
        let cache = tool_orchestration::CacheEvidence {
            status: "bypass".into(),
            key: String::new(),
            reason: "tool invocation is not safely cacheable".into(),
        };
        let evidence = tool_orchestration::format_evidence(&recommendation, &cache);
        assert!(evidence.contains("selected=shell_run"));
        assert!(evidence.contains("TargetedTestBeforeBroadSuite"));
        assert!(evidence.contains("prefer targeted test before broad suite"));
    }

    #[test]
    fn shipped_shell_path_emits_scheduler_and_verification_evidence() {
        let mut budget = tool_scheduler::ExecutionBudget::for_turn(2);
        let schedule = budget.schedule_batch(1, 0).expect("schedule shell");
        let digest = budget
            .before_call(
                "shell_run",
                &json!({"program":"cargo","args":["test"],"output_mode":"compact"}),
            )
            .expect("record call");
        let verification = budget.verification_for("shell_run", &digest, true);
        assert!(
            tool_scheduler::ExecutionBudget::format_schedule(&schedule).contains("parallel=false")
        );
        assert_eq!(verification.status, "not_applicable");
    }

    #[test]
    fn output_mode_is_part_of_cache_and_loop_identity() {
        let compact = format!("cargo test\0output_mode={}", OutputMode::Compact.as_str());
        let verbatim = format!("cargo test\0output_mode={}", OutputMode::Verbatim.as_str());
        assert_ne!(compact, verbatim);
        assert_ne!(
            json!({"program":"cargo","args":["test"],"output_mode":"compact"}),
            json!({"program":"cargo","args":["test"],"output_mode":"verbatim"})
        );
    }
}
