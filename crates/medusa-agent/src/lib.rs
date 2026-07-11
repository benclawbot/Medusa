//! Persistent single-agent orchestration and built-in Phase 1 tools.

use std::{
    collections::VecDeque,
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use medusa_config::Config;
use medusa_core::{CorrelationId, ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use medusa_protocol::{Actor, EventEnvelope, EventPayload};
use medusa_provider::{
    Message, MessageBlock, ModelProvider, ModelRequest, ResponseBlock, Role, ToolDefinition,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use walkdir::WalkDir;

const SYSTEM_PROMPT: &str = "You are Medusa, an autonomous coding agent. Inspect the repository, make the smallest correct change, and verify it. Use tools rather than inventing repository contents. Never modify tests, verification scripts, snapshots, fixtures, or expected outputs unless the user explicitly asks for that exact change; fix the product code instead. Do not expose private chain-of-thought; provide concise decisions and evidence.";
const MAX_TOOL_OUTPUT_BYTES: usize = 1_000_000;

/// Durable state for one single-agent session.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentSession {
    pub id: SessionId,
    pub objective: String,
    pub repo: PathBuf,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub completed: bool,
    pub turn: u32,
    pub messages: Vec<Message>,
    pub events: Vec<EventEnvelope>,
    pub evidence: Vec<String>,
}

/// Result of one durable model/tool step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepOutcome {
    Continue,
    Completed,
}

/// Persistent single-agent engine.
pub struct AgentEngine<P> {
    provider: P,
    config: Config,
}

impl<P: ModelProvider> AgentEngine<P> {
    #[must_use]
    pub fn new(provider: P, config: Config) -> Self {
        Self { provider, config }
    }

    pub fn create_session(&self, repo: &Path, objective: String) -> MedusaResult<AgentSession> {
        bootstrap(repo)?;
        let now = OffsetDateTime::now_utc();
        let id = SessionId::new();
        let mut session = AgentSession {
            id: id.clone(),
            objective: objective.clone(),
            repo: repo.to_path_buf(),
            created_at: now,
            updated_at: now,
            completed: false,
            turn: 0,
            messages: vec![Message {
                role: Role::User,
                content: vec![MessageBlock::Text {
                    text: objective.clone(),
                }],
            }],
            events: Vec::new(),
            evidence: Vec::new(),
        };
        append_event(
            &mut session,
            Actor::User,
            EventPayload::SessionCreated { objective },
        )?;
        persist(&session)?;
        Ok(session)
    }

    pub fn load_session(&self, repo: &Path, session: &str) -> MedusaResult<AgentSession> {
        let id = SessionId::parse(session).map_err(|message| {
            MedusaError::new(
                ErrorCode::InvalidConfiguration,
                ErrorCategory::Validation,
                message,
            )
        })?;
        let path = session_path(repo, &id);
        let session: AgentSession = serde_json::from_slice(&fs::read(path)?)?;
        verify_chain(&session.events)?;
        Ok(session)
    }

    pub fn run_to_completion(&self, session: &mut AgentSession) -> MedusaResult<()> {
        while !session.completed && session.turn < self.config.agent.max_turns {
            self.step(session)?;
        }
        if session.completed {
            Ok(())
        } else {
            Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Execution,
                "agent exhausted max_turns before verification passed",
            ))
        }
    }

    pub fn step(&self, session: &mut AgentSession) -> MedusaResult<StepOutcome> {
        if session.completed {
            return Ok(StepOutcome::Completed);
        }
        session.turn = session.turn.saturating_add(1);
        append_event(
            session,
            Actor::Coordinator,
            EventPayload::ModelRequestStarted {
                provider: self.config.model.provider.clone(),
                model: self.config.model.name.clone(),
            },
        )?;
        let response = self.provider.complete(&ModelRequest {
            system: SYSTEM_PROMPT.into(),
            messages: session.messages.clone(),
            tools: built_in_tools(),
            max_tokens: self.config.model.max_output_tokens,
            temperature_milli: self.config.model.temperature_milli,
        })?;
        append_event(
            session,
            Actor::Coordinator,
            EventPayload::ModelResponseReceived {
                response_id: response.response_id.clone(),
                usage: serde_json::to_value(response.usage).map_err(json_error)?,
            },
        )?;

        let mut assistant_blocks = Vec::new();
        let mut calls = VecDeque::new();
        for block in response.blocks {
            match block {
                ResponseBlock::Text { text } => {
                    assistant_blocks.push(MessageBlock::Text { text });
                }
                ResponseBlock::ToolUse { id, name, input } => {
                    assistant_blocks.push(MessageBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    });
                    calls.push_back((id, name, input));
                }
            }
        }
        if !assistant_blocks.is_empty() {
            session.messages.push(Message {
                role: Role::Assistant,
                content: assistant_blocks,
            });
        }

        while let Some((id, name, input)) = calls.pop_front() {
            append_event(
                session,
                Actor::Coordinator,
                EventPayload::ToolCallRequested {
                    tool: name.clone(),
                    arguments: input.clone(),
                },
            )?;
            let result = execute_tool(&session.repo, &name, &input);
            let (content, is_error, exit_code) = match result {
                Ok(output) => (output, false, Some(0)),
                Err(error) => (error.to_string(), true, Some(1)),
            };
            append_event(
                session,
                Actor::Coordinator,
                EventPayload::ToolExecutionCompleted {
                    tool: name,
                    exit_code,
                },
            )?;
            session.messages.push(Message {
                role: Role::User,
                content: vec![MessageBlock::ToolResult {
                    tool_use_id: id,
                    content,
                    is_error,
                }],
            });
            persist(session)?;
        }

        if response.stop_reason.as_deref() == Some("end_turn")
            && !session.messages.last().is_some_and(|message| {
                matches!(
                    message.content.first(),
                    Some(MessageBlock::ToolResult { .. })
                )
            })
        {
            let verification = targeted_verification(&session.repo)?;
            append_event(
                session,
                Actor::Coordinator,
                EventPayload::VerificationCompleted {
                    passed: verification.passed,
                    evidence: verification.evidence.clone(),
                },
            )?;
            session.evidence.extend(verification.evidence.clone());
            if verification.passed {
                session.completed = true;
                append_event(
                    session,
                    Actor::Coordinator,
                    EventPayload::SessionCompleted {
                        report_ref: format!("session:{}.json", session.id),
                    },
                )?;
            } else {
                session.messages.push(Message {
                    role: Role::User,
                    content: vec![MessageBlock::Text {
                        text: format!(
                            "Verification failed. Fix the remaining issue. Evidence:\n{}",
                            verification.evidence.join("\n")
                        ),
                    }],
                });
            }
        }
        session.updated_at = OffsetDateTime::now_utc();
        persist(session)?;
        Ok(if session.completed {
            StepOutcome::Completed
        } else {
            StepOutcome::Continue
        })
    }
}

/// Creates the on-disk Medusa layout and repository map.
pub fn bootstrap(repo: &Path) -> MedusaResult<()> {
    fs::create_dir_all(repo.join(".medusa/sessions"))?;
    let map = repo.join("REPOSITORY_MAP.md");
    if !map.exists() {
        fs::write(
            map,
            "# Repository Map\n\n## Overview\n\n## Languages and Frameworks\n\n## Entry Points\n\n## Build and Run Commands\n\n## Test Commands\n\n## Critical Invariants\n",
        )?;
    }
    Ok(())
}

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

fn built_in_tools() -> Vec<ToolDefinition> {
    vec![
        tool(
            "fs_read",
            "Read a UTF-8 file inside the repository.",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }),
        ),
        tool(
            "fs_write",
            "Atomically write a UTF-8 file inside the repository.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        ),
        tool(
            "search_text",
            "Search UTF-8 repository files for an exact text fragment.",
            json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"],
                "additionalProperties": false
            }),
        ),
        tool(
            "shell_run",
            "Run a non-destructive command in the repository and capture output.",
            json!({
                "type": "object",
                "properties": {
                    "program": {"type": "string"},
                    "args": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["program", "args"],
                "additionalProperties": false
            }),
        ),
        tool(
            "git_checkpoint",
            "Stage all changes and create a Git checkpoint commit.",
            json!({
                "type": "object",
                "properties": {"message": {"type": "string"}},
                "required": ["message"],
                "additionalProperties": false
            }),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        input_schema,
    }
}

fn execute_tool(repo: &Path, name: &str, input: &Value) -> MedusaResult<String> {
    match name {
        "fs_read" => {
            let path = input_string(input, "path")?;
            Ok(fs::read_to_string(safe_path(repo, path)?)?)
        }
        "fs_write" => {
            let path = safe_path(repo, input_string(input, "path")?)?;
            let content = input_string(input, "content")?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let original_permissions = fs::metadata(&path)
                .ok()
                .map(|metadata| metadata.permissions());
            let temporary = path.with_extension("medusa-tmp");
            fs::write(&temporary, content)?;
            if let Some(permissions) = original_permissions {
                fs::set_permissions(&temporary, permissions)?;
            }
            fs::rename(&temporary, &path)?;
            Ok(format!(
                "wrote {} bytes to {}",
                content.len(),
                path.display()
            ))
        }
        "search_text" => search_text(repo, input_string(input, "query")?),
        "shell_run" => {
            let program = input_string(input, "program")?;
            if matches!(program, "rm" | "sudo" | "shutdown" | "reboot" | "mkfs") {
                return Err(MedusaError::new(
                    ErrorCode::PolicyDenied,
                    ErrorCategory::Policy,
                    format!("hard-denied command: {program}"),
                ));
            }
            let args = input
                .get("args")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid_tool("args must be an array"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| invalid_tool("every arg must be a string"))
                })
                .collect::<MedusaResult<Vec<_>>>()?;
            let output = Command::new(program)
                .args(&args)
                .current_dir(repo)
                .output()?;
            let evidence = format_command_output(program, &args, &output.stdout, &output.stderr);
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
        "git_checkpoint" => {
            let message = input_string(input, "message")?;
            run_git(repo, &["add", "-A"])?;
            run_git(repo, &["commit", "-m", message])?;
            Ok(format!("checkpoint created: {message}"))
        }
        _ => Err(invalid_tool(format!("unknown tool: {name}"))),
    }
}

fn search_text(repo: &Path, query: &str) -> MedusaResult<String> {
    let mut results = Vec::new();
    for entry in WalkDir::new(repo).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file()
            || entry.path().components().any(|part| {
                matches!(part, Component::Normal(name) if name == ".git" || name == ".medusa")
            })
        {
            continue;
        }
        if let Ok(text) = fs::read_to_string(entry.path()) {
            for (index, line) in text.lines().enumerate() {
                if line.contains(query) {
                    results.push(format!(
                        "{}:{}:{}",
                        entry.path().display(),
                        index + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    Ok(truncate(results.join("\n")))
}

fn safe_path(repo: &Path, relative: &str) -> MedusaResult<PathBuf> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!("path escapes repository: {relative}"),
        ));
    }
    Ok(repo.join(path))
}

fn input_string<'a>(input: &'a Value, key: &str) -> MedusaResult<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_tool(format!("{key} must be a string")))
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

fn format_command_output(
    program: &str,
    args: &[impl AsRef<str>],
    stdout: &[u8],
    stderr: &[u8],
) -> Vec<String> {
    vec![
        format!(
            "command={} {}",
            program,
            args.iter()
                .map(|arg| arg.as_ref())
                .collect::<Vec<_>>()
                .join(" ")
        ),
        format!(
            "stdout={}",
            truncate(String::from_utf8_lossy(stdout).into_owned())
        ),
        format!(
            "stderr={}",
            truncate(String::from_utf8_lossy(stderr).into_owned())
        ),
    ]
}

fn truncate(mut value: String) -> String {
    if value.len() > MAX_TOOL_OUTPUT_BYTES {
        value.truncate(MAX_TOOL_OUTPUT_BYTES);
        value.push_str("\n[truncated]");
    }
    value
}

fn append_event(
    session: &mut AgentSession,
    actor: Actor,
    payload: EventPayload,
) -> MedusaResult<()> {
    let previous_hash = session.events.last().map(|event| event.checksum.clone());
    let event = EventEnvelope::new(
        session.events.len() as u64 + 1,
        session.id.clone(),
        actor,
        CorrelationId::new(),
        payload,
        previous_hash,
        OffsetDateTime::now_utc(),
    )?;
    session.events.push(event);
    Ok(())
}

fn verify_chain(events: &[EventEnvelope]) -> MedusaResult<()> {
    let mut previous: Option<&str> = None;
    for event in events {
        event.validate()?;
        if event.previous_hash.as_deref() != previous {
            return Err(MedusaError::new(
                ErrorCode::ChecksumMismatch,
                ErrorCategory::Persistence,
                "event chain previous hash mismatch",
            ));
        }
        previous = Some(&event.checksum);
    }
    Ok(())
}

fn persist(session: &AgentSession) -> MedusaResult<()> {
    let path = session_path(&session.repo, &session.id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(session)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn session_path(repo: &Path, id: &SessionId) -> PathBuf {
    repo.join(".medusa/sessions").join(format!("{id}.json"))
}

fn invalid_tool(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn json_error(error: serde_json::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use medusa_provider::{ModelResponse, Usage};

    use super::*;

    struct ScriptedProvider {
        responses: Mutex<VecDeque<ModelResponse>>,
    }

    impl ScriptedProvider {
        fn new(responses: Vec<ModelResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
            }
        }
    }

    impl ModelProvider for ScriptedProvider {
        fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
            self.responses
                .lock()
                .expect("provider lock")
                .pop_front()
                .ok_or_else(|| {
                    MedusaError::new(
                        ErrorCode::DependencyUnavailable,
                        ErrorCategory::Internal,
                        "scripted response exhausted",
                    )
                })
        }
    }

    fn response(blocks: Vec<ResponseBlock>, stop_reason: &str) -> ModelResponse {
        ModelResponse {
            response_id: Some("fixture".into()),
            stop_reason: Some(stop_reason.into()),
            blocks,
            usage: Usage::default(),
        }
    }

    #[test]
    fn fixture_bug_fix_survives_restart_with_exact_evidence() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("value.txt"), "41\n").expect("buggy fixture");
        fs::write(
            directory.path().join("verify.sh"),
            "#!/bin/sh\nset -eu\ntest \"$(cat value.txt)\" = \"42\"\necho verified-value-42\n",
        )
        .expect("verification script");

        let first = AgentEngine::new(
            ScriptedProvider::new(vec![response(
                vec![ResponseBlock::ToolUse {
                    id: "read-1".into(),
                    name: "fs_read".into(),
                    input: json!({"path": "value.txt"}),
                }],
                "tool_use",
            )]),
            Config::default(),
        );
        let mut session = first
            .create_session(directory.path(), "fix the off-by-one value".into())
            .expect("session");
        assert_eq!(
            first.step(&mut session).expect("inspect step"),
            StepOutcome::Continue
        );

        let second = AgentEngine::new(
            ScriptedProvider::new(vec![
                response(
                    vec![ResponseBlock::ToolUse {
                        id: "write-1".into(),
                        name: "fs_write".into(),
                        input: json!({"path": "value.txt", "content": "42\n"}),
                    }],
                    "tool_use",
                ),
                response(
                    vec![ResponseBlock::Text {
                        text: "The value is corrected; run targeted verification.".into(),
                    }],
                    "end_turn",
                ),
            ]),
            Config::default(),
        );
        let mut resumed = second
            .load_session(directory.path(), session.id.as_str())
            .expect("restart load");
        second
            .run_to_completion(&mut resumed)
            .expect("complete fix");

        assert_eq!(
            fs::read_to_string(directory.path().join("value.txt")).expect("value"),
            "42\n"
        );
        assert!(resumed.completed);
        assert!(
            resumed
                .evidence
                .iter()
                .any(|line| line.contains("verified-value-42"))
        );
        assert!(
            resumed
                .evidence
                .iter()
                .any(|line| line.contains("exit_status=exit status: 0"))
        );
    }

    #[test]
    fn parent_path_escape_is_denied() {
        let directory = tempfile::tempdir().expect("tempdir");
        assert!(safe_path(directory.path(), "../secret").is_err());
    }
}
