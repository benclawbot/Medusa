use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::output_envelope::{AdaptedOutput, OutputMode};

#[path = "tool_redaction.rs"]
mod redaction;

pub(crate) use redaction::{redact_args, redact_text};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandFamily {
    Git,
    Build,
    Test,
    PackageManager,
    Search,
    General,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VerificationState {
    NotApplicable,
    Pending,
    Passed,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ToolExecutionTrace {
    pub schema_version: u16,
    pub timestamp_unix_ms: i128,
    pub tool: String,
    pub command_family: CommandFamily,
    pub program: String,
    pub args: Vec<String>,
    pub output_mode: OutputMode,
    pub success: bool,
    pub latency_ms: u64,
    pub retry_count: u32,
    pub raw_bytes: usize,
    pub retained_bytes: usize,
    pub original_lines: usize,
    pub omitted_lines: usize,
    pub duplicate_lines_removed: usize,
    pub expansion_handle: Option<String>,
    pub verification_state: VerificationState,
}

impl ToolExecutionTrace {
    pub(crate) fn for_shell(
        program: &str,
        args: &[String],
        success: bool,
        latency: Duration,
        raw_bytes: usize,
        adapted: &AdaptedOutput,
    ) -> Self {
        Self {
            schema_version: 1,
            timestamp_unix_ms: OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000,
            tool: "shell_run".to_owned(),
            command_family: classify_command(program, args),
            program: redact_text(program),
            args: redact_args(args),
            output_mode: adapted.mode,
            success,
            latency_ms: latency.as_millis().try_into().unwrap_or(u64::MAX),
            retry_count: 0,
            raw_bytes,
            retained_bytes: adapted.to_string().len(),
            original_lines: adapted.original_lines,
            omitted_lines: adapted.omitted_lines,
            duplicate_lines_removed: adapted.duplicate_lines_removed,
            expansion_handle: adapted.expansion_handle.clone(),
            verification_state: verification_state(program, args, success),
        }
    }

    fn sanitized_for_persistence(&self) -> Self {
        let mut sanitized = self.clone();
        sanitized.program = redact_text(&sanitized.program);
        sanitized.args = redact_args(&sanitized.args);
        sanitized.expansion_handle = sanitized.expansion_handle.as_deref().map(redact_text);
        sanitized
    }
}

pub(crate) fn append_trace(repo: &Path, trace: &ToolExecutionTrace) -> MedusaResult<PathBuf> {
    let relative = Path::new(".medusa")
        .join("telemetry")
        .join("tool-executions.jsonl");
    let absolute = repo.join(&relative);
    if let Some(parent) = absolute.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&absolute)?;
    serde_json::to_writer(&mut file, &trace.sanitized_for_persistence())?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(relative)
}

fn classify_command(program: &str, args: &[String]) -> CommandFamily {
    let executable = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    match executable.as_str() {
        "git" => CommandFamily::Git,
        "cargo"
            if args
                .first()
                .is_some_and(|arg| matches!(arg.as_str(), "test" | "nextest")) =>
        {
            CommandFamily::Test
        }
        "cargo" => CommandFamily::Build,
        "pytest" => CommandFamily::Test,
        "go" if args.first().is_some_and(|arg| arg == "test") => CommandFamily::Test,
        "npm" | "pnpm" | "yarn" | "pip" | "pip3" => CommandFamily::PackageManager,
        "rg" | "grep" | "find" | "fd" => CommandFamily::Search,
        _ => CommandFamily::General,
    }
}

fn verification_state(program: &str, args: &[String], success: bool) -> VerificationState {
    if matches!(classify_command(program, args), CommandFamily::Test) {
        if success {
            VerificationState::Passed
        } else {
            VerificationState::Failed
        }
    } else {
        VerificationState::NotApplicable
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::output_envelope::{AdaptedOutput, OutputMode};

    use super::*;

    fn adapted() -> AdaptedOutput {
        AdaptedOutput {
            mode: OutputMode::Compact,
            rendered: "status=success".to_owned(),
            original_lines: 20,
            omitted_lines: 10,
            duplicate_lines_removed: 2,
            expansion_handle: Some("shell_run:fixture".to_owned()),
        }
    }

    #[test]
    fn command_family_and_verification_are_deterministic() {
        let args = vec!["test".to_owned(), "--workspace".to_owned()];
        let trace = ToolExecutionTrace::for_shell(
            "cargo",
            &args,
            true,
            Duration::from_millis(17),
            200,
            &adapted(),
        );
        assert_eq!(trace.command_family, CommandFamily::Test);
        assert_eq!(trace.verification_state, VerificationState::Passed);
        assert_eq!(trace.retry_count, 0);
        assert_eq!(trace.raw_bytes, 200);
        assert!(trace.retained_bytes < trace.raw_bytes);
    }

    #[test]
    fn trace_is_appended_as_parseable_json_lines() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let trace = ToolExecutionTrace::for_shell(
            "git",
            &["status".to_owned()],
            true,
            Duration::from_millis(1),
            100,
            &adapted(),
        );
        let relative = append_trace(directory.path(), &trace).expect("append trace");
        let body = fs::read_to_string(directory.path().join(relative)).expect("read trace");
        let restored: ToolExecutionTrace =
            serde_json::from_str(body.trim()).expect("parse trace line");
        assert_eq!(restored.command_family, CommandFamily::Git);
        assert_eq!(restored.output_mode, OutputMode::Compact);
        assert_eq!(restored.omitted_lines, 10);
        assert_eq!(
            restored.expansion_handle.as_deref(),
            Some("shell_run:fixture")
        );
    }

    #[test]
    fn persisted_trace_redacts_secret_arguments_before_serialization() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let trace = ToolExecutionTrace::for_shell(
            "curl",
            &[
                "--token".to_owned(),
                "telemetry-secret".to_owned(),
                "https://example.test/?X-Amz-Signature=signed-secret".to_owned(),
            ],
            true,
            Duration::from_millis(1),
            100,
            &adapted(),
        );
        let relative = append_trace(directory.path(), &trace).expect("append trace");
        let body = fs::read_to_string(directory.path().join(relative)).expect("read trace");
        assert!(!body.contains("telemetry-secret"));
        assert!(!body.contains("signed-secret"));
        assert!(body.contains("[REDACTED]"));
    }
}
