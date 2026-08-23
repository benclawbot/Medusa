//! Runtime-owned implementation of the model-facing analysis workspace contract.
//!
//! The workspace remains subordinate to the runtime: repository reads are brokered into immutable
//! artifacts, computation goes through containment, and recursive delegation is admitted through
//! the production orchestrator/coordinator rather than constructing provider clients here.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, atomic::AtomicBool, mpsc},
};

use medusa_agent::analysis_host::AnalysisWorkspaceHost;
use medusa_config::Config;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde_json::{Value, json};

use crate::{
    RuntimeController, RuntimeEvent, SubmissionState,
    analysis_workspace::{AnalysisDelegationKind, AnalysisOperation, AnalysisValue},
    multi_agent_coordinator, production_orchestrator,
    invariants::RuntimeInvariantRegistry,
    prompt::PromptDraft,
    team_control::TeamControlPlane,
};

const MAX_OBJECTIVE_BYTES: usize = 8 * 1024;

#[derive(Clone)]
pub(crate) struct RuntimeAnalysisHost {
    repo: PathBuf,
    config: Config,
    session_api_key: Option<String>,
    team_control: TeamControlPlane,
    events: mpsc::Sender<RuntimeEvent>,
    cancellation: Arc<AtomicBool>,
}

impl RuntimeAnalysisHost {
    pub(crate) fn new(
        repo: PathBuf,
        config: Config,
        session_api_key: Option<String>,
        team_control: TeamControlPlane,
        events: mpsc::Sender<RuntimeEvent>,
        cancellation: Arc<AtomicBool>,
    ) -> Self {
        Self {
            repo,
            config,
            session_api_key,
            team_control,
            events,
            cancellation,
        }
    }

    /// Builds a non-running façade over the canonical workspace methods. No worker thread is
    /// spawned and the façade has no command receiver; it exists solely to avoid a second storage
    /// or provenance implementation.
    fn workspace(&self) -> RuntimeController {
        let (commands, _command_receiver) = mpsc::channel();
        let (_event_sender_unused, events) = mpsc::channel();
        RuntimeController {
            commands,
            events,
            cancel: Arc::clone(&self.cancellation),
            submission: Arc::new(Mutex::new(SubmissionState::default())),
            event_sender: self.events.clone(),
            team_control: self.team_control.clone(),
            repo: self.repo.clone(),
            invariants: Arc::new(Mutex::new(RuntimeInvariantRegistry::default())),
        }
    }

    fn import_reduce(&self, session_id: &str, input: &Value) -> MedusaResult<String> {
        let path = required_string(input, "path")?;
        let byte_range = optional_range(input)?;
        let operation = parse_operation(input)?;
        let workspace = self.workspace();
        let artifact = workspace
            .analysis_import_file(session_id, path, byte_range)
            .map_err(runtime_error)?;
        let result = workspace
            .analysis_reduce_contained(session_id, &artifact, operation)
            .map_err(runtime_error)?;
        serde_json::to_string(&json!({
            "artifact": artifact,
            "result": result.result,
            "metrics": result.metrics,
            "authority": "medusa-runtime/analysis-workspace",
        }))
        .map_err(json_error)
    }

    fn set_value(&self, session_id: &str, input: &Value) -> MedusaResult<String> {
        let name = required_string(input, "name")?;
        let value = input
            .get("value")
            .ok_or_else(|| invalid("analysis set_value requires `value`"))?;
        let value = analysis_value(value)?;
        let info = self
            .workspace()
            .analysis_set_value(session_id, name, value)
            .map_err(runtime_error)?;
        serde_json::to_string(&info).map_err(json_error)
    }

    fn delegate_read_only(&self, input: &Value) -> MedusaResult<String> {
        let objective = required_string(input, "objective")?.trim();
        if objective.is_empty() || objective.len() > MAX_OBJECTIVE_BYTES {
            return Err(invalid(format!(
                "analysis delegation objective must contain 1..={MAX_OBJECTIVE_BYTES} bytes"
            )));
        }
        let draft = PromptDraft {
            text: objective.to_owned(),
            attachments: Vec::new(),
            revision: 0,
        };
        let plan = production_orchestrator::plan_for_repository(&self.repo, &draft)
            .map_err(|error| invalid(error.to_string()))?;
        if production_orchestrator::requires_mutation(&plan)
            || plan.tasks.iter().any(|task| !task.write_paths.is_empty())
        {
            return Err(policy(
                "analysis workspace may delegate only read-only child work; repository mutation must return to the parent Medusa transaction path",
            ));
        }
        let evidence = multi_agent_coordinator::run_preflight(
            &self.repo,
            &self.config,
            self.session_api_key.clone(),
            &plan,
            &self.cancellation,
            &self.team_control,
            &self.events,
        )
        .map_err(invalid)?;
        serde_json::to_string(&json!({
            "plan_fingerprint": evidence.plan_fingerprint,
            "repository_fingerprint": evidence.repository_fingerprint,
            "workers": evidence.workers,
            "state_path": evidence.state_path,
            "authority": "medusa-runtime/production-orchestrator+multi-agent-coordinator",
        }))
        .map_err(json_error)
    }
}

impl AnalysisWorkspaceHost for RuntimeAnalysisHost {
    fn execute(
        &self,
        session_id: &str,
        input: &Value,
        cancellation: &AtomicBool,
    ) -> MedusaResult<String> {
        if cancellation.load(std::sync::atomic::Ordering::Acquire) {
            return Err(MedusaError::new(
                ErrorCode::ToolExecutionFailed,
                ErrorCategory::Execution,
                "analysis workspace call cancelled",
            ));
        }
        match required_string(input, "action")? {
            "import_reduce" => self.import_reduce(session_id, input),
            "set_value" => self.set_value(session_id, input),
            "list_values" => serde_json::to_string(
                &self
                    .workspace()
                    .analysis_list_values(session_id)
                    .map_err(runtime_error)?,
            )
            .map_err(json_error),
            "snapshot" => serde_json::to_string(
                &self
                    .workspace()
                    .analysis_snapshot(session_id)
                    .map_err(runtime_error)?,
            )
            .map_err(json_error),
            "restore" => serde_json::to_string(
                &self
                    .workspace()
                    .analysis_restore(session_id)
                    .map_err(runtime_error)?,
            )
            .map_err(json_error),
            "delegate_read_only" => self.delegate_read_only(input),
            "list_children" => serde_json::to_string(
                &self
                    .workspace()
                    .analysis_delegate(AnalysisDelegationKind::ListChildren)
                    .map_err(runtime_error)?,
            )
            .map_err(json_error),
            "follow_up_child" => {
                let worker_id = required_string(input, "worker_id")?.to_owned();
                let message = required_string(input, "message")?.to_owned();
                serde_json::to_string(
                    &self
                        .workspace()
                        .analysis_delegate(AnalysisDelegationKind::FollowUp { worker_id, message })
                        .map_err(runtime_error)?,
                )
                .map_err(json_error)
            }
            "await_child" => {
                let worker_id = required_string(input, "worker_id")?.to_owned();
                serde_json::to_string(
                    &self
                        .workspace()
                        .analysis_delegate(AnalysisDelegationKind::AwaitTerminal { worker_id })
                        .map_err(runtime_error)?,
                )
                .map_err(json_error)
            }
            action => Err(invalid(format!("unsupported analysis workspace action `{action}`"))),
        }
    }
}

fn parse_operation(input: &Value) -> MedusaResult<AnalysisOperation> {
    Ok(match required_string(input, "operation")? {
        "byte_count" => AnalysisOperation::ByteCount,
        "line_count" => AnalysisOperation::LineCount,
        "utf8_contains" => AnalysisOperation::Utf8Contains {
            needle: required_string(input, "needle")?.to_owned(),
        },
        "matching_lines" => AnalysisOperation::MatchingLines {
            needle: required_string(input, "needle")?.to_owned(),
            limit: optional_usize(input, "limit")?.unwrap_or(32).min(128),
        },
        "head_lines" => AnalysisOperation::HeadLines {
            limit: optional_usize(input, "limit")?.unwrap_or(32).min(128),
        },
        operation => return Err(invalid(format!("unsupported analysis operation `{operation}`"))),
    })
}

fn optional_range(input: &Value) -> MedusaResult<Option<(u64, u64)>> {
    match (
        input.get("byte_start").and_then(Value::as_u64),
        input.get("byte_end").and_then(Value::as_u64),
    ) {
        (None, None) => Ok(None),
        (Some(start), Some(end)) => Ok(Some((start, end))),
        _ => Err(invalid("byte_start and byte_end must be supplied together")),
    }
}

fn analysis_value(value: &Value) -> MedusaResult<AnalysisValue> {
    match value {
        Value::Null => Ok(AnalysisValue::Null),
        Value::Bool(value) => Ok(AnalysisValue::Bool(*value)),
        Value::Number(value) => value
            .as_i64()
            .map(AnalysisValue::Integer)
            .ok_or_else(|| invalid("analysis numeric values must be signed 64-bit integers")),
        Value::String(value) => Ok(AnalysisValue::Text(value.clone())),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("analysis arrays may contain strings only"))
            })
            .collect::<MedusaResult<Vec<_>>>()
            .map(AnalysisValue::StringList),
        Value::Object(_) => Err(invalid(
            "analysis set_value accepts only null, bool, i64, string, or string arrays",
        )),
    }
}

fn required_string<'a>(input: &'a Value, key: &str) -> MedusaResult<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid(format!("analysis workspace requires non-empty `{key}`")))
}

fn optional_usize(input: &Value, key: &str) -> MedusaResult<Option<usize>> {
    input
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| invalid(format!("`{key}` must be a non-negative integer")))
        })
        .transpose()
}

fn runtime_error(error: crate::RuntimeError) -> MedusaError {
    MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        error.to_string(),
    )
}

fn json_error(error: serde_json::Error) -> MedusaError {
    MedusaError::new(ErrorCode::ToolExecutionFailed, ErrorCategory::Internal, error.to_string())
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidInput, ErrorCategory::Validation, message)
}

fn policy(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::PolicyDenied, ErrorCategory::Policy, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn set_value_rejects_object_state() {
        assert!(analysis_value(&json!({"secret": "opaque"})).is_err());
    }

    #[test]
    fn range_requires_both_bounds() {
        assert!(optional_range(&json!({"byte_start": 1})).is_err());
    }

    #[test]
    fn mutation_delegation_is_rejected_before_worker_execution() {
        let temp = TempDir::new().expect("tempdir");
        std::fs::write(temp.path().join("README.md"), "hello").expect("fixture");
        let (events, _receiver) = mpsc::channel();
        let host = RuntimeAnalysisHost::new(
            temp.path().to_path_buf(),
            Config::default(),
            None,
            TeamControlPlane::default(),
            events,
            Arc::new(AtomicBool::new(false)),
        );
        let result = host.delegate_read_only(&json!({
            "objective": "Modify README.md to add another line and save the edit"
        }));
        assert!(result.is_err());
    }
}
