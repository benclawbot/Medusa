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

pub(crate) use crate::tool_redaction::{redact_args, redact_local_paths, redact_text};

/// Maximum retained completion lines in `tool-executions.jsonl` before rotation.
pub(crate) const MAX_TRACE_LINES: usize = 5_000;
/// Current schema for completion and intent records.
pub(crate) const TELEMETRY_SCHEMA_VERSION: u16 = 1;

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
    #[serde(default)]
    pub operation_id: String,
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
        operation_id: &str,
    ) -> Self {
        Self {
            schema_version: TELEMETRY_SCHEMA_VERSION,
            timestamp_unix_ms: now_unix_ms(),
            operation_id: operation_id.to_owned(),
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

    /// Generic completion record for non-shell tools (browser, skill, compound, delegation).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_tool(
        tool: &str,
        program: &str,
        args: &[String],
        success: bool,
        latency: Duration,
        raw_bytes: usize,
        retained_bytes: usize,
        operation_id: &str,
    ) -> Self {
        Self {
            schema_version: TELEMETRY_SCHEMA_VERSION,
            timestamp_unix_ms: now_unix_ms(),
            operation_id: operation_id.to_owned(),
            tool: tool.to_owned(),
            command_family: classify_command(program, args),
            program: redact_text(program),
            args: redact_args(args),
            output_mode: OutputMode::Normal,
            success,
            latency_ms: latency.as_millis().try_into().unwrap_or(u64::MAX),
            retry_count: 0,
            raw_bytes,
            retained_bytes,
            original_lines: 0,
            omitted_lines: 0,
            duplicate_lines_removed: 0,
            expansion_handle: None,
            verification_state: VerificationState::NotApplicable,
        }
    }

    pub(crate) fn for_browser(
        action: &str,
        target: &str,
        success: bool,
        latency: Duration,
        raw_bytes: usize,
        retained_bytes: usize,
        operation_id: &str,
    ) -> Self {
        Self::for_tool(
            "browser",
            action,
            &[target.to_owned()],
            success,
            latency,
            raw_bytes,
            retained_bytes,
            operation_id,
        )
    }

    pub(crate) fn for_skill(
        skill_name: &str,
        args: &[String],
        success: bool,
        latency: Duration,
        raw_bytes: usize,
        retained_bytes: usize,
        operation_id: &str,
    ) -> Self {
        Self::for_tool(
            "skill",
            skill_name,
            args,
            success,
            latency,
            raw_bytes,
            retained_bytes,
            operation_id,
        )
    }

    pub(crate) fn for_compound(
        step: &str,
        args: &[String],
        success: bool,
        latency: Duration,
        raw_bytes: usize,
        retained_bytes: usize,
        operation_id: &str,
    ) -> Self {
        Self::for_tool(
            "compound",
            step,
            args,
            success,
            latency,
            raw_bytes,
            retained_bytes,
            operation_id,
        )
    }

    pub(crate) fn for_delegation(
        worker_id: &str,
        task: &str,
        success: bool,
        latency: Duration,
        raw_bytes: usize,
        retained_bytes: usize,
        operation_id: &str,
    ) -> Self {
        Self::for_tool(
            "delegation",
            worker_id,
            &[task.to_owned()],
            success,
            latency,
            raw_bytes,
            retained_bytes,
            operation_id,
        )
    }

    /// Synthetic completion synthesized when an intent never produced a trace (crash recovery).
    pub(crate) fn interrupted(intent: &ToolIntentRecord) -> Self {
        Self {
            schema_version: TELEMETRY_SCHEMA_VERSION,
            timestamp_unix_ms: now_unix_ms(),
            operation_id: intent.operation_id.clone(),
            tool: intent.tool.clone(),
            command_family: classify_command(&intent.program, &intent.args),
            program: redact_text(&intent.program),
            args: redact_args(&intent.args),
            output_mode: OutputMode::Normal,
            success: false,
            latency_ms: 0,
            retry_count: 0,
            raw_bytes: 0,
            retained_bytes: 0,
            original_lines: 0,
            omitted_lines: 0,
            duplicate_lines_removed: 0,
            expansion_handle: None,
            verification_state: VerificationState::NotApplicable,
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

/// Start-of-execution intent persisted BEFORE the tool runs, so a crash between
/// exec and completion cannot silently lose the action.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ToolIntentRecord {
    pub schema_version: u16,
    pub operation_id: String,
    pub tool: String,
    pub program: String,
    pub args: Vec<String>,
    pub started_unix_ms: i128,
}

pub(crate) fn new_operation_id(tool: &str) -> String {
    let nanos = OffsetDateTime::now_utc().unix_timestamp_nanos();
    format!("{tool}-{nanos}-{}", std::process::id())
}

fn now_unix_ms() -> i128 {
    OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000
}

fn telemetry_dir(repo: &Path) -> PathBuf {
    repo.join(".medusa").join("telemetry")
}

fn trace_path(repo: &Path) -> PathBuf {
    telemetry_dir(repo).join("tool-executions.jsonl")
}

fn intents_dir(repo: &Path) -> PathBuf {
    telemetry_dir(repo).join("intents")
}

fn intent_path(repo: &Path, operation_id: &str) -> PathBuf {
    intents_dir(repo).join(format!("{operation_id}.json"))
}

/// Persists the execution intent before the tool runs. Best-effort failures are
/// returned so callers can decide; telemetry must never break tool execution.
pub(crate) fn record_intent(
    repo: &Path,
    operation_id: &str,
    tool: &str,
    program: &str,
    args: &[String],
) -> MedusaResult<()> {
    let dir = intents_dir(repo);
    fs::create_dir_all(&dir)?;
    let intent = ToolIntentRecord {
        schema_version: TELEMETRY_SCHEMA_VERSION,
        operation_id: operation_id.to_owned(),
        tool: tool.to_owned(),
        program: redact_text(program),
        args: redact_args(args),
        started_unix_ms: now_unix_ms(),
    };
    fs::write(
        intent_path(repo, operation_id),
        serde_json::to_vec(&intent).map_err(std::io::Error::other)?,
    )?;
    Ok(())
}

/// Records a completion trace and retires its intent. Failures are swallowed so
/// telemetry can never break tool execution.
pub(crate) fn record_completion(repo: &Path, operation_id: &str, trace: &ToolExecutionTrace) {
    let _ = append_trace(repo, trace);
    let _ = complete_intent(repo, operation_id);
}

/// Removes the intent once its completion trace has been persisted.
pub(crate) fn complete_intent(repo: &Path, operation_id: &str) -> MedusaResult<()> {
    match fs::remove_file(intent_path(repo, operation_id)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn completed_operation_ids(repo: &Path) -> MedusaResult<std::collections::BTreeSet<String>> {
    let mut completed = std::collections::BTreeSet::new();
    let path = trace_path(repo);
    let body = match fs::read_to_string(&path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(completed),
        Err(error) => return Err(error.into()),
    };
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(trace) = serde_json::from_str::<ToolExecutionTrace>(line)
            && !trace.operation_id.is_empty()
        {
            completed.insert(trace.operation_id);
        }
    }
    Ok(completed)
}

/// Reconciles intents left behind by a crash: every intent without a matching
/// completion trace gains a synthetic interrupted completion. Returns the number
/// of orphaned intents recovered. Safe to call on every tool start.
pub(crate) fn reconcile_intents(repo: &Path) -> MedusaResult<usize> {
    let dir = intents_dir(repo);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let completed = completed_operation_ids(repo)?;
    let mut recovered = 0_usize;
    for entry in entries {
        let entry = entry?;
        let body = match fs::read_to_string(entry.path()) {
            Ok(body) => body,
            Err(_) => continue,
        };
        let intent: ToolIntentRecord = match serde_json::from_str(&body) {
            Ok(intent) => intent,
            Err(_) => {
                let _ = fs::remove_file(entry.path());
                continue;
            }
        };
        if completed.contains(&intent.operation_id) {
            let _ = fs::remove_file(entry.path());
            continue;
        }
        let trace = ToolExecutionTrace::interrupted(&intent);
        if append_trace(repo, &trace).is_ok() {
            recovered = recovered.saturating_add(1);
        }
        let _ = fs::remove_file(entry.path());
    }
    Ok(recovered)
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
    serde_json::to_writer(&mut file, &trace.sanitized_for_persistence())
        .map_err(std::io::Error::other)?;
    file.write_all(b"\n")?;
    file.flush()?;
    drop(file);
    let _ = prune_traces(repo);
    Ok(relative)
}

/// Retention: keeps only the newest completion lines so telemetry cannot grow
/// without bound. Returns the number of rotated lines.
pub(crate) fn prune_traces(repo: &Path) -> MedusaResult<usize> {
    prune_jsonl_lines(&trace_path(repo), MAX_TRACE_LINES)
}

pub(crate) fn prune_jsonl_lines(path: &Path, max_lines: usize) -> MedusaResult<usize> {
    let body = match fs::read_to_string(path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let lines: Vec<&str> = body.lines().collect();
    if lines.len() <= max_lines {
        return Ok(0);
    }
    let retained = lines[lines.len() - max_lines..].join("\n") + "\n";
    fs::write(path, retained)?;
    Ok(lines.len() - max_lines)
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
            "op-fixture-1",
        );
        assert_eq!(trace.command_family, CommandFamily::Test);
        assert_eq!(trace.verification_state, VerificationState::Passed);
        assert_eq!(trace.retry_count, 0);
        assert_eq!(trace.raw_bytes, 200);
        assert_eq!(trace.operation_id, "op-fixture-1");
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
            "op-fixture-2",
        );
        let relative = append_trace(directory.path(), &trace).expect("append trace");
        let body = fs::read_to_string(directory.path().join(relative)).expect("read trace");
        let restored: ToolExecutionTrace =
            serde_json::from_str(body.trim()).expect("parse trace line");
        assert_eq!(restored.command_family, CommandFamily::Git);
        assert_eq!(restored.output_mode, OutputMode::Compact);
        assert_eq!(restored.omitted_lines, 10);
        assert_eq!(restored.operation_id, "op-fixture-2");
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
            "op-fixture-3",
        );
        let relative = append_trace(directory.path(), &trace).expect("append trace");
        let body = fs::read_to_string(directory.path().join(relative)).expect("read trace");
        assert!(!body.contains("telemetry-secret"));
        assert!(!body.contains("signed-secret"));
        assert!(body.contains("[REDACTED]"));
    }

    #[test]
    fn intent_is_recorded_before_completion_and_reconciled_after_crash() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let operation_id = new_operation_id("shell_run");
        record_intent(
            directory.path(),
            &operation_id,
            "shell_run",
            "cargo",
            &["test".to_owned()],
        )
        .expect("record intent");
        assert!(intent_path(directory.path(), &operation_id).exists());
        // A crash leaves the intent behind with no completion trace.
        let recovered = reconcile_intents(directory.path()).expect("reconcile");
        assert_eq!(recovered, 1);
        assert!(!intent_path(directory.path(), &operation_id).exists());
        let body = fs::read_to_string(trace_path(directory.path())).expect("trace body");
        let restored: ToolExecutionTrace =
            serde_json::from_str(body.trim()).expect("parse interrupted trace");
        assert_eq!(restored.operation_id, operation_id);
        assert!(!restored.success);
        // Second reconcile finds nothing left to recover.
        assert_eq!(reconcile_intents(directory.path()).expect("reconcile"), 0);
    }

    #[test]
    fn completed_intent_is_not_reported_as_interrupted() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let operation_id = new_operation_id("shell_run");
        record_intent(
            directory.path(),
            &operation_id,
            "shell_run",
            "git",
            &["status".to_owned()],
        )
        .expect("record intent");
        let trace = ToolExecutionTrace::for_shell(
            "git",
            &["status".to_owned()],
            true,
            Duration::from_millis(1),
            100,
            &adapted(),
            &operation_id,
        );
        append_trace(directory.path(), &trace).expect("append trace");
        complete_intent(directory.path(), &operation_id).expect("complete intent");
        assert_eq!(reconcile_intents(directory.path()).expect("reconcile"), 0);
        let body = fs::read_to_string(trace_path(directory.path())).expect("trace body");
        assert_eq!(body.lines().count(), 1);
    }

    #[test]
    fn non_shell_constructors_carry_args_latency_and_bytes() {
        let latency = Duration::from_millis(42);
        let browser = ToolExecutionTrace::for_browser(
            "navigate",
            "https://example.test/",
            true,
            latency,
            1024,
            256,
            "op-browser-1",
        );
        assert_eq!(browser.tool, "browser");
        assert_eq!(browser.latency_ms, 42);
        assert_eq!(browser.raw_bytes, 1024);
        assert_eq!(browser.retained_bytes, 256);
        let skill = ToolExecutionTrace::for_skill(
            "release",
            &["--dry-run".to_owned()],
            true,
            latency,
            512,
            128,
            "op-skill-1",
        );
        assert_eq!(skill.tool, "skill");
        assert_eq!(skill.args, vec!["--dry-run".to_owned()]);
        let compound = ToolExecutionTrace::for_compound(
            "structured_patch",
            &["src/lib.rs".to_owned()],
            false,
            latency,
            64,
            32,
            "op-compound-1",
        );
        assert_eq!(compound.tool, "compound");
        assert!(!compound.success);
        let delegation = ToolExecutionTrace::for_delegation(
            "worker-a",
            "task-0",
            true,
            latency,
            2048,
            512,
            "op-delegation-1",
        );
        assert_eq!(delegation.tool, "delegation");
        assert_eq!(delegation.raw_bytes, 2048);
    }

    #[test]
    fn trace_retention_keeps_newest_lines() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let path = trace_path(directory.path());
        fs::create_dir_all(path.parent().expect("parent")).expect("telemetry dir");
        let mut body = String::new();
        for index in 0..10 {
            let trace = ToolExecutionTrace::for_tool(
                "shell_run",
                "echo",
                &[format!("{index}")],
                true,
                Duration::from_millis(1),
                10,
                5,
                &format!("op-{index}"),
            );
            body.push_str(&serde_json::to_string(&trace).expect("serialize"));
            body.push('\n');
        }
        fs::write(&path, body).expect("seed traces");
        let removed = prune_jsonl_lines(&path, 4).expect("prune");
        assert_eq!(removed, 6);
        let pruned = fs::read_to_string(&path).expect("read pruned");
        assert_eq!(pruned.lines().count(), 4);
        assert!(pruned.contains("op-9"));
        assert!(!pruned.contains("op-0"));
    }
}
