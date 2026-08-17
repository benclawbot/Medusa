use std::{fs, path::Path, sync::atomic::AtomicBool, time::Instant};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde_json::json;
use sha2::{Digest, Sha256};

#[path = "../repository_profile.rs"]
mod repository_profile;
#[path = "../tool_orchestration.rs"]
mod tool_orchestration;
#[path = "../tool_scheduler.rs"]
mod tool_scheduler;
#[path = "../tool_telemetry.rs"]
mod tool_telemetry;

use crate::{
    output_envelope::{OutputMode, adapt_command},
    policy::{sandboxed_command, sandboxed_command_cancellable, validate_shell_command},
};

pub(crate) fn run(
    repo: &Path,
    program: &str,
    args: &[String],
    output_mode: OutputMode,
) -> MedusaResult<String> {
    validate_shell_command(program, args)?;
    run_validated(repo, program, args, output_mode, None)
}

pub(crate) fn run_approved(
    repo: &Path,
    program: &str,
    args: &[String],
    output_mode: OutputMode,
) -> MedusaResult<String> {
    validate_shell_command(program, args)?;
    run_validated(repo, program, args, output_mode, None)
}

pub(crate) fn run_cancellable(
    repo: &Path,
    program: &str,
    args: &[String],
    output_mode: OutputMode,
    cancellation: &AtomicBool,
) -> MedusaResult<String> {
    validate_shell_command(program, args)?;
    run_validated(repo, program, args, output_mode, Some(cancellation))
}

pub(crate) fn run_approved_cancellable(
    repo: &Path,
    program: &str,
    args: &[String],
    output_mode: OutputMode,
    cancellation: &AtomicBool,
) -> MedusaResult<String> {
    validate_shell_command(program, args)?;
    run_validated(repo, program, args, output_mode, Some(cancellation))
}

fn output_mode_label(output_mode: OutputMode) -> &'static str {
    match output_mode {
        OutputMode::Compact => "compact",
        OutputMode::Normal => "normal",
        OutputMode::Verbatim => "verbatim",
    }
}

fn run_validated(
    repo: &Path,
    program: &str,
    args: &[String],
    output_mode: OutputMode,
    cancellation: Option<&AtomicBool>,
) -> MedusaResult<String> {
    let mode = output_mode_label(output_mode);
    let persisted_args = tool_telemetry::redact_args(args);
    let command_summary = format!("{} {}", program, persisted_args.join(" "));
    let cache_input_summary = format!("{} {}\0output_mode={mode}", program, args.join(" "));
    let mut recommendation = tool_orchestration::recommend("shell_run", &command_summary);
    let profile_decision = repository_profile::decision(repo, "shell_run");
    recommendation.score = recommendation
        .score
        .saturating_add(profile_decision.score_adjustment);
    let mut execution_budget = tool_scheduler::ExecutionBudget::for_turn(1);
    let schedule = execution_budget.schedule_batch(1, 0)?;
    let scheduler_evidence = tool_scheduler::ExecutionBudget::format_schedule(&schedule);
    let input = json!({"program": program, "args": args, "output_mode": mode});
    let call_digest = execution_budget.before_call("shell_run", &input)?;
    let (cached, mut cache_evidence) =
        tool_orchestration::cache_lookup(repo, "shell_run", &cache_input_summary)?;
    if let Some(cached) = cached {
        let cached = tool_telemetry::redact_text(&cached);
        execution_budget.record_output(&cached)?;
        let verification = execution_budget.verification_for("shell_run", &call_digest, true);
        let orchestration_evidence =
            tool_orchestration::format_evidence(&recommendation, &cache_evidence);
        return Ok(format!(
            "{cached}\n{orchestration_evidence}\n{}\n{scheduler_evidence}\n{}",
            repository_profile::format_decision(&profile_decision),
            tool_scheduler::ExecutionBudget::format_verification(&verification)
        ));
    }

    let started = Instant::now();
    let output = match cancellation {
        Some(cancellation) => sandboxed_command_cancellable(repo, program, args, cancellation)?,
        None => sandboxed_command(repo, program, args)?,
    };
    let command = format!("command={} {}", program, args.join(" "));
    let raw = format!(
        "{command}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let persisted_stdout = tool_telemetry::redact_text(&String::from_utf8_lossy(&output.stdout));
    let persisted_stderr = tool_telemetry::redact_text(&String::from_utf8_lossy(&output.stderr));
    let adapted = adapt_command(
        program,
        &persisted_args,
        persisted_stdout.as_bytes(),
        persisted_stderr.as_bytes(),
        output.status.success(),
        output_mode,
    );
    let elapsed = started.elapsed();
    let trace = tool_telemetry::ToolExecutionTrace::for_shell(
        program,
        args,
        output.status.success(),
        elapsed,
        raw.len(),
        &adapted,
    );
    let trace_path = tool_telemetry::append_trace(repo, &trace)?;
    let profile_record_error = repository_profile::record(
        repo,
        "shell_run",
        output.status.success(),
        elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
        adapted.to_string().len(),
        match output_mode {
            OutputMode::Compact => repository_profile::LearnedOutputMode::Compact,
            OutputMode::Normal => repository_profile::LearnedOutputMode::Normal,
            OutputMode::Verbatim => repository_profile::LearnedOutputMode::Verbatim,
        },
        false,
    )
    .err();

    let mut evidence = tool_telemetry::redact_text(&adapted.to_string());
    execution_budget.record_output(&evidence)?;
    if output.status.success() {
        cache_evidence = tool_orchestration::cache_store(
            repo,
            "shell_run",
            &cache_input_summary,
            &evidence,
        )?;
    }
    let verification =
        execution_budget.verification_for("shell_run", &call_digest, output.status.success());
    let orchestration_evidence =
        tool_orchestration::format_evidence(&recommendation, &cache_evidence);
    evidence.push('\n');
    evidence.push_str(&orchestration_evidence);
    evidence.push('\n');
    evidence.push_str(&repository_profile::format_decision(&profile_decision));
    if let Some(error) = profile_record_error {
        evidence.push_str(&format!(
            "\n[repository-profile record_status=ignored; reason={error}]"
        ));
    }
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
    let redacted = tool_telemetry::redact_text(raw);
    let digest = Sha256::digest(format!("shell_run\0{redacted}").as_bytes());
    let relative = Path::new(".medusa")
        .join("output-expansions")
        .join(format!("{}.txt", hex::encode(&digest[..8])));
    let absolute = repo.join(&relative);
    if let Some(parent) = absolute.parent() {
        fs::create_dir_all(parent)?;
    }
    if !absolute.exists() {
        fs::write(&absolute, redacted)?;
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
    fn expansion_redacts_command_stdout_and_stderr_secrets() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let relative = persist_expansion(
            directory.path(),
            "command=curl --token cli-secret https://user:db-secret@example.test/?sig=url-secret\nstdout:\nAuthorization: Bearer stdout-secret\nstderr:\npassword=stderr-secret\n",
        )
        .expect("persist redacted expansion");
        let body = fs::read_to_string(directory.path().join(relative)).expect("read expansion");
        for secret in [
            "cli-secret",
            "db-secret",
            "url-secret",
            "stdout-secret",
            "stderr-secret",
        ] {
            assert!(!body.contains(secret));
        }
        assert!(body.contains("[REDACTED]"));
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
        let compact = format!(
            "cargo test\0output_mode={}",
            output_mode_label(OutputMode::Compact)
        );
        let verbatim = format!(
            "cargo test\0output_mode={}",
            output_mode_label(OutputMode::Verbatim)
        );
        assert_ne!(compact, verbatim);
        assert_ne!(
            json!({"program":"cargo","args":["test"],"output_mode":"compact"}),
            json!({"program":"cargo","args":["test"],"output_mode":"verbatim"})
        );
    }
}
