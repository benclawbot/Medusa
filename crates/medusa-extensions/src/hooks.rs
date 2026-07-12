use std::{
    collections::BTreeMap,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    redaction::redact,
    support::{internal, invalid, wait_with_timeout},
};

/// Supported deterministic hook events.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    SessionStart,
    BeforeModel,
    AfterModel,
    BeforeTool,
    AfterTool,
    ToolError,
    BeforePatch,
    AfterPatch,
    BeforeCommit,
    AfterCommit,
    BeforeVerification,
    AfterVerification,
    BeforeCompletion,
    SessionEnd,
    BeforeMemoryWrite,
    AfterMemoryWrite,
    BeforeImprovementPromote,
}

/// Failure behavior for a hook.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookFailurePolicy {
    Ignore,
    Warn,
    Block,
}

/// Command hook contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommandHook {
    pub id: String,
    pub event: HookEvent,
    pub program: String,
    pub args: Vec<String>,
    pub timeout_ms: u64,
    pub declared_side_effects: Vec<String>,
    pub path_scope: Vec<PathBuf>,
    pub environment_allowlist: Vec<String>,
    pub failure_policy: HookFailurePolicy,
}

/// Structured hook decision.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HookDecision {
    pub allow: bool,
    pub reason: Option<String>,
    #[serde(default)]
    pub data: Value,
}

/// Executes a JSON command hook with environment and path declarations enforced.
pub fn run_command_hook(
    hook: &CommandHook,
    repository: &Path,
    input: &Value,
    source_environment: &BTreeMap<String, String>,
) -> MedusaResult<HookDecision> {
    validate_hook(hook, repository)?;
    let mut command = Command::new(&hook.program);
    command
        .args(&hook.args)
        .current_dir(repository)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in &hook.environment_allowlist {
        if let Some(value) = source_environment.get(key) {
            command.env(key, value);
        }
    }
    let mut child = command.spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| internal("hook stdin unavailable"))?;
    serde_json::to_writer(&mut stdin, input)?;
    stdin.write_all(b"\n")?;
    drop(stdin);
    let output = wait_with_timeout(child, Duration::from_millis(hook.timeout_ms))?;
    if !output.status.success() {
        let reason = format!(
            "hook {} failed: {}",
            hook.id,
            redact(&String::from_utf8_lossy(&output.stderr))
        );
        return match hook.failure_policy {
            HookFailurePolicy::Ignore | HookFailurePolicy::Warn => Ok(HookDecision {
                allow: true,
                reason: Some(reason),
                data: Value::Null,
            }),
            HookFailurePolicy::Block => Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                reason,
            )),
        };
    }
    let decision: HookDecision = serde_json::from_slice(&output.stdout)?;
    if !decision.allow && hook.failure_policy == HookFailurePolicy::Block {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            decision
                .reason
                .clone()
                .unwrap_or_else(|| "hook denied action".into()),
        ));
    }
    Ok(decision)
}

fn validate_hook(hook: &CommandHook, repository: &Path) -> MedusaResult<()> {
    if hook.id.trim().is_empty() || hook.program.trim().is_empty() || hook.timeout_ms == 0 {
        return Err(invalid(
            "hook id, program, and positive timeout are required",
        ));
    }
    for scope in &hook.path_scope {
        if scope.is_absolute()
            || scope.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                format!("hook path scope escapes repository: {}", scope.display()),
            ));
        }
        let _ = repository.join(scope);
    }
    Ok(())
}
