mod compound;
mod executable_skills;
mod filesystem;
mod git;
mod intelligence;
mod shell;
pub(crate) mod skills;
#[path = "../tool_telemetry.rs"]
pub(crate) mod tool_telemetry;
mod web;
pub mod pipeline {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tool_pipeline.rs"));
}

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use medusa_capabilities::{CapabilityRegistry, SystemProbe};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_extensions::DesktopCommanderSettings;
use medusa_provider::ToolDefinition;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use crate::code_mode::{
    CodeModeExecutionV1, CodeModeLimits, CodeModeProgramV1, CodeModeSdkV1, generate_sdk,
};
use crate::team::AgentExecutionPolicy;
use crate::tool_result::CanonicalToolResultV1;
use pipeline::{
    FinalToolOutcome, GuardDecision, ResolvedToolIdentity, ToolExecutionPipeline,
    ToolPipelineRequest,
};

static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);

pub(crate) struct CertifiedToolExecution {
    pub receipt: Value,
    pub result: MedusaResult<String>,
    pub canonical: CanonicalToolResultV1,
}

fn certified_execution(outcome: FinalToolOutcome) -> MedusaResult<CertifiedToolExecution> {
    let mut receipt = outcome.receipt_value()?;
    let result = outcome.into_result();
    let canonical = CanonicalToolResultV1::from_receipt(&receipt, &result)?;
    if let Value::Object(fields) = &mut receipt {
        fields.insert(
            "canonical_result_schema_version".to_owned(),
            Value::from(canonical.schema_version),
        );
        fields.insert(
            "canonical_result_fingerprint".to_owned(),
            Value::String(canonical.source_fingerprint.clone()),
        );
    }
    Ok(CertifiedToolExecution {
        receipt,
        result,
        canonical,
    })
}

/// Single policy-aware registry for built-in tools shared by every agent frontend.
#[derive(Clone, Debug)]
pub struct ToolManager {
    desktop_commander: DesktopCommanderSettings,
}

impl ToolManager {
    #[must_use]
    pub fn new(desktop_commander: DesktopCommanderSettings) -> Self {
        Self { desktop_commander }
    }

    /// Compatibility projection for callers without a repository context. Discovery failure
    /// fails closed by returning no model tools.
    #[must_use]
    pub fn definitions(&self, read_only: bool) -> Vec<ToolDefinition> {
        CapabilityRegistry::discover_with_desktop(
            PathBuf::from("."),
            &SystemProbe,
            self.desktop_commander.clone(),
        )
        .map_or_else(|_| Vec::new(), |registry| registry.model_tools(read_only))
    }

    pub fn definitions_for(
        &self,
        repo: &Path,
        read_only: bool,
    ) -> MedusaResult<Vec<ToolDefinition>> {
        built_in_tools(repo, &self.desktop_commander, read_only)
    }

    /// Generates a Code Mode SDK from the same effective admitted definitions exposed to native
    /// model calls. The SDK is presentation only; nested execution must use the certified path.
    pub fn code_mode_sdk(&self, repo: &Path, read_only: bool) -> MedusaResult<CodeModeSdkV1> {
        self.code_mode_sdk_for(repo, read_only, &AgentExecutionPolicy::unrestricted(), None)
    }

    /// Generates a Code Mode SDK after applying the same execution-policy and optional active
    /// agent-scope filters used by the runtime. This prevents a presentation-only registry from
    /// advertising tools that the current caller cannot actually invoke.
    pub fn code_mode_sdk_for(
        &self,
        repo: &Path,
        read_only: bool,
        execution_policy: &AgentExecutionPolicy,
        session_id: Option<&str>,
    ) -> MedusaResult<CodeModeSdkV1> {
        let mut definitions = self.definitions_for(repo, read_only)?;
        definitions.retain(|definition| execution_policy.allows(&definition.name));
        if let Some(session_id) = session_id {
            let names = definitions
                .iter()
                .map(|definition| definition.name.clone())
                .collect::<Vec<_>>();
            let scoped = crate::agent_scope::effective_agent_scope_tools(repo, session_id, names)?;
            definitions.retain(|definition| scoped.binary_search(&definition.name).is_ok());
        }
        generate_sdk(&definitions)
    }

    /// Executes a bounded declarative Code Mode call plan. The plan is intentionally an IR rather
    /// than an unsandboxed language runtime: every child call is independently admitted and runs
    /// through the existing certified pipeline with its own receipt and canonical result.
    pub fn execute_code_mode(
        &self,
        repo: &Path,
        program: &CodeModeProgramV1,
        read_only: bool,
        execution_policy: &AgentExecutionPolicy,
        cancellation: &AtomicBool,
    ) -> MedusaResult<CodeModeExecutionV1> {
        self.execute_code_mode_with_session(
            repo,
            program,
            read_only,
            execution_policy,
            cancellation,
            None,
        )
    }

    /// Executes Code Mode while binding every nested call to the active durable agent scope.
    /// Scope projection is refreshed before each child so revocation cannot race with a
    /// previously generated SDK and leave a stale tool callable.
    pub fn execute_code_mode_for_session(
        &self,
        repo: &Path,
        session_id: &str,
        program: &CodeModeProgramV1,
        read_only: bool,
        execution_policy: &AgentExecutionPolicy,
        cancellation: &AtomicBool,
    ) -> MedusaResult<CodeModeExecutionV1> {
        self.execute_code_mode_with_session(
            repo,
            program,
            read_only,
            execution_policy,
            cancellation,
            Some(session_id),
        )
    }

    fn execute_code_mode_with_session(
        &self,
        repo: &Path,
        program: &CodeModeProgramV1,
        read_only: bool,
        execution_policy: &AgentExecutionPolicy,
        cancellation: &AtomicBool,
        session_id: Option<&str>,
    ) -> MedusaResult<CodeModeExecutionV1> {
        let sdk = self.code_mode_sdk_for(repo, read_only, execution_policy, session_id)?;
        program.validate(&sdk, CodeModeLimits::default())?;
        let mut children = Vec::with_capacity(program.calls.len());
        for (ordinal, call) in program.calls.iter().enumerate() {
            if session_id.is_some() {
                let current_sdk =
                    self.code_mode_sdk_for(repo, read_only, execution_policy, session_id)?;
                program.validate(&current_sdk, CodeModeLimits::default())?;
            }
            let execution = execute_tool_cancellable_with_context_and_policy_certified(
                repo,
                &call.tool,
                &call.input,
                cancellation,
                None,
                execution_policy,
            )?;
            children.push(crate::code_mode::CodeModeChildResultV1::from_certified(
                ordinal as u32,
                &call.tool,
                execution.receipt,
                execution.canonical,
            ));
        }
        CodeModeExecutionV1::from_children(children)
    }

    pub fn execute(&self, repo: &Path, name: &str, input: &Value) -> MedusaResult<String> {
        execute_tool_cancellable(repo, name, input, &NEVER_CANCELLED)
    }

    pub fn execute_approved(&self, repo: &Path, name: &str, input: &Value) -> MedusaResult<String> {
        execute_approved_tool_cancellable(repo, name, input, &NEVER_CANCELLED)
    }
}

pub(crate) fn available_skills(repo: &Path) -> Vec<skills::SkillSummary> {
    skills::summaries(repo)
}

pub(crate) fn available_skills_for_query(repo: &Path, query: &str) -> Vec<skills::SkillSummary> {
    skills::summaries_for_query(repo, query)
}

pub(crate) fn built_in_tools(
    repo: &Path,
    desktop_commander: &DesktopCommanderSettings,
    read_only: bool,
) -> MedusaResult<Vec<ToolDefinition>> {
    CapabilityRegistry::discover_with_desktop(
        repo.to_path_buf(),
        &SystemProbe,
        desktop_commander.clone(),
    )
    .map(|registry| registry.model_tools(read_only))
}

pub(crate) fn execute_tool(repo: &Path, name: &str, input: &Value) -> MedusaResult<String> {
    match name {
        "fs_read" => filesystem::read(repo, input_string(input, "path")?),
        "fs_create_dir" => filesystem::create_dir(repo, input_string(input, "path")?),
        "fs_write" => filesystem::write(
            repo,
            input_string(input, "path")?,
            input_string(input, "content")?,
        ),
        "search_text" => filesystem::search(repo, input_string(input, "query")?),
        "semantic_capabilities" => intelligence::semantic_capabilities(),
        "code_index" => intelligence::code_index(repo, input),
        "inspect_target" => compound::inspect_target(repo, input),
        "apply_structured_patch" => compound::apply_structured_patch(repo, input),
        "verify_impacted" => compound::verify_impacted(repo, input),
        "typescript_semantic" => intelligence::typescript_semantic(repo, input),
        "patch_apply" => intelligence::patch_apply(repo, input),
        "symbol_rename" => intelligence::symbol_rename(repo, input),
        "shell_run" => {
            let program = input_string(input, "program")?;
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
            let output_mode = crate::output_envelope::OutputMode::parse(input_optional_string(
                input,
                "output_mode",
            )?)?;
            shell::run(repo, program, &args, output_mode)
        }
        "web_search" => web::search(
            input_string(input, "query")?,
            input_domains(input, "allowed_domains")?,
            input_domains(input, "blocked_domains")?,
        ),
        "web_fetch" => web::fetch(
            input_string(input, "url")?,
            input.get("prompt").and_then(Value::as_str),
        ),
        "skill_read" => skills::read(
            repo,
            input_string(input, "name")?,
            input.get("scope").and_then(Value::as_str),
        ),
        "skill_execute" => {
            let cancellation = AtomicBool::new(false);
            executable_skills::run(repo, input, &cancellation)
        }
        "git_checkpoint" => git::checkpoint(repo, input_string(input, "message")?),
        _ => Err(invalid_tool(format!("unknown tool: {name}"))),
    }
}

fn capability_guard(repo: &Path, name: &str) -> GuardDecision {
    let registry = match CapabilityRegistry::discover(repo) {
        Ok(registry) => registry,
        Err(error) => return GuardDecision::Deny(format!("capability discovery failed: {error}")),
    };
    let id = format!("tool.{name}");
    let Some(entry) = registry.entry(&id) else {
        return GuardDecision::Deny(format!("tool is not registered: {name}"));
    };
    if !entry.projected_to(medusa_capabilities::CapabilitySurface::Model) {
        return GuardDecision::Deny(format!(
            "tool is unavailable: {name}: {}",
            entry.readiness.detail
        ));
    }
    GuardDecision::Allow
}

fn certified_pipeline(
    repo: &Path,
    name: &str,
    execution_policy: &AgentExecutionPolicy,
) -> ToolExecutionPipeline {
    let capability_repo = repo.to_path_buf();
    let capability_name = name.to_owned();
    let policy = execution_policy.clone();
    let policy_name = name.to_owned();
    ToolExecutionPipeline::new()
        .with_guard("capability_readiness", move |_| {
            capability_guard(&capability_repo, &capability_name)
        })
        .with_guard("agent_execution_policy", move |request| {
            policy
                .denial_reason(&policy_name, &request.input)
                .map_or(GuardDecision::Allow, GuardDecision::Deny)
        })
}

fn engine_pipeline(name: &str, execution_policy: &AgentExecutionPolicy) -> ToolExecutionPipeline {
    let policy = execution_policy.clone();
    let policy_name = name.to_owned();
    ToolExecutionPipeline::new()
        .with_guard("engine_capability_authority", |_| GuardDecision::Allow)
        .with_guard("agent_execution_policy", move |request| {
            policy
                .denial_reason(&policy_name, &request.input)
                .map_or(GuardDecision::Allow, GuardDecision::Deny)
        })
}

fn execute_tool_cancellable_with_context_unchecked(
    repo: &Path,
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
    mutation_context: Option<&crate::transaction::MutationContext>,
) -> MedusaResult<String> {
    if cancellation.load(Ordering::Acquire) {
        return Err(cancelled_tool(name));
    }
    if name == "fs_write" {
        if let Some(context) = mutation_context {
            let relative = input_string(input, "path")?;
            let content = input_string(input, "content")?;
            let outcome = filesystem::write_with_context(repo, relative, content, context)?;
            return Ok(format!(
                "wrote {} bytes to {relative}; mutation_ids={}",
                content.len(),
                outcome.mutation_ids.join(",")
            ));
        }
    }
    if name == "skill_execute" {
        return executable_skills::run(repo, input, cancellation);
    }
    if name != "shell_run" {
        return execute_tool(repo, name, input);
    }
    let program = input_string(input, "program")?;
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
    let output_mode =
        crate::output_envelope::OutputMode::parse(input_optional_string(input, "output_mode")?)?;
    shell::run_cancellable(repo, program, &args, output_mode, cancellation)
}

pub(crate) fn execute_tool_cancellable_with_context_and_policy_certified(
    repo: &Path,
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
    mutation_context: Option<&crate::transaction::MutationContext>,
    execution_policy: &AgentExecutionPolicy,
) -> MedusaResult<CertifiedToolExecution> {
    certified_execution(certified_pipeline(repo, name, execution_policy).execute(
        ToolPipelineRequest::built_in(name, input),
        cancellation,
        |canonical_input| {
            execute_tool_cancellable_with_context_unchecked(
                repo,
                name,
                canonical_input,
                cancellation,
                mutation_context,
            )
        },
    )?)
}

pub(crate) fn execute_tool_cancellable_with_context_and_policy(
    repo: &Path,
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
    mutation_context: Option<&crate::transaction::MutationContext>,
    execution_policy: &AgentExecutionPolicy,
) -> MedusaResult<String> {
    execute_tool_cancellable_with_context_and_policy_certified(
        repo,
        name,
        input,
        cancellation,
        mutation_context,
        execution_policy,
    )?
    .result
}

pub(crate) fn execute_tool_cancellable_with_context(
    repo: &Path,
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
    mutation_context: Option<&crate::transaction::MutationContext>,
) -> MedusaResult<String> {
    execute_tool_cancellable_with_context_and_policy(
        repo,
        name,
        input,
        cancellation,
        mutation_context,
        &AgentExecutionPolicy::unrestricted(),
    )
}

pub(crate) fn execute_tool_cancellable_with_policy_certified(
    repo: &Path,
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
    execution_policy: &AgentExecutionPolicy,
) -> MedusaResult<CertifiedToolExecution> {
    execute_tool_cancellable_with_context_and_policy_certified(
        repo,
        name,
        input,
        cancellation,
        None,
        execution_policy,
    )
}

pub(crate) fn certify_cached_tool_with_policy(
    repo: &Path,
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
    execution_policy: &AgentExecutionPolicy,
    cached_output: String,
) -> MedusaResult<CertifiedToolExecution> {
    certified_execution(certified_pipeline(repo, name, execution_policy).execute(
        ToolPipelineRequest::built_in(name, input),
        cancellation,
        move |_| Ok(cached_output),
    )?)
}

pub(crate) fn execute_engine_tool_with_policy(
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
    execution_policy: &AgentExecutionPolicy,
    handler: impl FnOnce(&Value) -> MedusaResult<String>,
) -> MedusaResult<CertifiedToolExecution> {
    let request = ToolPipelineRequest {
        identity: ResolvedToolIdentity::new(
            format!("engine-tool.{name}"),
            format!("medusa-agent-engine::{name}"),
            "engine-v1",
        ),
        input: input.clone(),
        approval_required: false,
        approval_granted: false,
    };
    certified_execution(engine_pipeline(name, execution_policy).execute(
        request,
        cancellation,
        handler,
    )?)
}

pub(crate) fn execute_tool_cancellable(
    repo: &Path,
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
) -> MedusaResult<String> {
    execute_tool_cancellable_with_context(repo, name, input, cancellation, None)
}

fn execute_approved_tool_cancellable_unchecked(
    repo: &Path,
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
) -> MedusaResult<String> {
    if cancellation.load(Ordering::Acquire) {
        return Err(cancelled_tool(name));
    }
    if name != "shell_run" {
        return execute_approved_tool(repo, name, input);
    }
    let program = input_string(input, "program")?;
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
    let output_mode =
        crate::output_envelope::OutputMode::parse(input_optional_string(input, "output_mode")?)?;
    shell::run_approved_cancellable(repo, program, &args, output_mode, cancellation)
}

pub(crate) fn execute_approved_tool_cancellable_with_policy_certified(
    repo: &Path,
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
    execution_policy: &AgentExecutionPolicy,
) -> MedusaResult<CertifiedToolExecution> {
    let mut request = ToolPipelineRequest::built_in(name, input);
    request.approval_required = true;
    request.approval_granted = true;
    certified_execution(certified_pipeline(repo, name, execution_policy).execute(
        request,
        cancellation,
        |canonical_input| {
            execute_approved_tool_cancellable_unchecked(repo, name, canonical_input, cancellation)
        },
    )?)
}

pub(crate) fn execute_approved_tool_cancellable_with_policy(
    repo: &Path,
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
    execution_policy: &AgentExecutionPolicy,
) -> MedusaResult<String> {
    execute_approved_tool_cancellable_with_policy_certified(
        repo,
        name,
        input,
        cancellation,
        execution_policy,
    )?
    .result
}

pub(crate) fn execute_approved_tool_cancellable(
    repo: &Path,
    name: &str,
    input: &Value,
    cancellation: &AtomicBool,
) -> MedusaResult<String> {
    execute_approved_tool_cancellable_with_policy(
        repo,
        name,
        input,
        cancellation,
        &AgentExecutionPolicy::unrestricted(),
    )
}

pub(crate) fn execute_approved_tool(
    repo: &Path,
    name: &str,
    input: &Value,
) -> MedusaResult<String> {
    match name {
        "fs_create_dir" => filesystem::create_dir_approved(input_string(input, "path")?),
        "fs_write" => filesystem::write_approved(
            input_string(input, "path")?,
            input_string(input, "content")?,
        ),
        "shell_run" => {
            let program = input_string(input, "program")?;
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
            let output_mode = crate::output_envelope::OutputMode::parse(input_optional_string(
                input,
                "output_mode",
            )?)?;
            shell::run_approved(repo, program, &args, output_mode)
        }
        _ => Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!("{name} cannot be authorized interactively"),
        )),
    }
}

fn input_domains(input: &Value, key: &str) -> MedusaResult<Vec<String>> {
    let Some(domains) = input.get(key) else {
        return Ok(Vec::new());
    };
    domains
        .as_array()
        .ok_or_else(|| invalid_tool(format!("{key} must be an array")))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(|domain| domain.trim().to_ascii_lowercase())
                .filter(|domain| !domain.is_empty())
                .ok_or_else(|| {
                    invalid_tool(format!("every {key} entry must be a non-empty string"))
                })
        })
        .collect()
}

pub(crate) fn input_string<'a>(input: &'a Value, key: &str) -> MedusaResult<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_tool(format!("{key} must be a string")))
}

fn input_optional_string<'a>(input: &'a Value, key: &str) -> MedusaResult<Option<&'a str>> {
    match input.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| invalid_tool(format!("{key} must be a string"))),
    }
}

pub(crate) fn input_usize(input: &Value, key: &str) -> MedusaResult<usize> {
    input
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| invalid_tool(format!("{key} must be a non-negative integer")))
}

pub fn format_command_output(
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
        format!("stdout={}", String::from_utf8_lossy(stdout)),
        format!("stderr={}", String::from_utf8_lossy(stderr)),
    ]
}

fn cancelled_tool(name: &str) -> MedusaError {
    let mut error = MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        format!("tool execution cancelled: {name}"),
    );
    error
        .context
        .insert("cancelled".into(), serde_json::Value::Bool(true));
    error
}

pub(crate) fn invalid_tool(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_mode::{CODE_MODE_SCHEMA_VERSION, CodeModeCallV1, CodeModeProgramV1};
    use crate::tool_result::CanonicalToolOutcome;

    #[test]
    fn optional_string_rejects_present_non_string_values() {
        assert_eq!(
            input_optional_string(&json!({}), "output_mode").expect("absent mode"),
            None
        );

        let normal = json!({"output_mode": "normal"});
        assert_eq!(
            input_optional_string(&normal, "output_mode").expect("string mode"),
            Some("normal")
        );

        for invalid in [Value::Null, json!(42), json!({"mode": "compact"})] {
            let input = json!({"output_mode": invalid});
            let error = input_optional_string(&input, "output_mode")
                .expect_err("present non-string mode must fail closed");
            assert!(error.to_string().contains("output_mode must be a string"));
        }
    }

    #[test]
    fn policy_aware_dispatch_denies_role_forbidden_tool_inside_pipeline() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let policy = AgentExecutionPolicy::for_team_role(crate::team::TeamRole::Researcher);
        let error = execute_tool_cancellable_with_policy_certified(
            directory.path(),
            "fs_write",
            &json!({"path":"denied.txt","content":"no"}),
            &AtomicBool::new(false),
            &policy,
        )
        .expect("certified execution")
        .result
        .expect_err("researcher write must be denied by certified pipeline");
        assert_eq!(error.code, ErrorCode::PolicyDenied);
        assert!(!directory.path().join("denied.txt").exists());
    }

    #[test]
    fn approved_retry_rechecks_role_policy_inside_pipeline() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let policy = AgentExecutionPolicy::for_team_role(crate::team::TeamRole::Researcher);
        let error = execute_approved_tool_cancellable_with_policy(
            directory.path(),
            "fs_write",
            &json!({"path": directory.path().join("denied.txt"), "content":"no"}),
            &AtomicBool::new(false),
            &policy,
        )
        .expect_err("approval must not override role policy");
        assert_eq!(error.code, ErrorCode::PolicyDenied);
        assert!(!directory.path().join("denied.txt").exists());
    }

    #[test]
    fn certified_execution_exposes_immutable_receipt_before_projection() {
        let directory = tempfile::tempdir().expect("temporary repository");
        std::fs::write(directory.path().join("fixture.txt"), "receipt").expect("fixture");
        let execution = execute_tool_cancellable_with_policy_certified(
            directory.path(),
            "fs_read",
            &json!({"path":"fixture.txt"}),
            &AtomicBool::new(false),
            &AgentExecutionPolicy::unrestricted(),
        )
        .expect("certified execution");
        assert_eq!(execution.receipt["outcome"], json!("success"));
        assert_eq!(
            execution.receipt["canonical_result_schema_version"],
            json!(crate::tool_result::CANONICAL_TOOL_RESULT_SCHEMA_VERSION)
        );
        assert_eq!(
            execution.receipt["canonical_result_fingerprint"],
            execution.canonical.source_fingerprint
        );
        assert!(execution.result.expect("result").contains("receipt"));
    }

    #[test]
    fn engine_owned_tool_still_runs_agent_policy_guard() {
        let policy = AgentExecutionPolicy::for_team_role(crate::team::TeamRole::Researcher);
        let execution = execute_engine_tool_with_policy(
            "fs_write",
            &json!({"path":"denied.txt","content":"no"}),
            &AtomicBool::new(false),
            &policy,
            |_| Ok("must not run".to_owned()),
        )
        .expect("certified execution");
        assert_eq!(execution.receipt["outcome"], json!("denied"));
        assert_eq!(
            execution.result.expect_err("policy denial").code,
            ErrorCode::PolicyDenied
        );
    }

    #[test]
    fn code_mode_executes_admitted_children_with_canonical_results() {
        let directory = tempfile::tempdir().expect("temporary repository");
        std::fs::write(directory.path().join("fixture.txt"), "code mode").expect("fixture");
        let manager = ToolManager::new(Default::default());
        let program = CodeModeProgramV1 {
            schema_version: CODE_MODE_SCHEMA_VERSION,
            calls: vec![CodeModeCallV1 {
                tool: "fs_read".to_owned(),
                input: json!({"path": "fixture.txt"}),
            }],
        };
        let execution = manager
            .execute_code_mode(
                directory.path(),
                &program,
                true,
                &AgentExecutionPolicy::unrestricted(),
                &AtomicBool::new(false),
            )
            .expect("code mode execution");
        assert_eq!(execution.children.len(), 1);
        assert_eq!(
            execution.children[0].canonical.outcome,
            CanonicalToolOutcome::Success
        );
        assert_eq!(execution.outcome, CanonicalToolOutcome::Success);
        assert_eq!(
            execution.children[0]
                .parent_execution_fingerprint
                .as_deref(),
            Some(execution.execution_fingerprint.as_str())
        );
        assert!(
            execution.model_projection["children"][0]["source_fingerprint"]
                .as_str()
                .is_some_and(|fingerprint| !fingerprint.is_empty())
        );
        assert_eq!(
            execution.model_projection["children"][0]["parent_execution_fingerprint"],
            execution.execution_fingerprint
        );
    }

    #[test]
    fn session_code_mode_refreshes_scope_before_nested_calls() {
        let directory = tempfile::tempdir().expect("temporary repository");
        std::fs::write(directory.path().join("fixture.txt"), "scoped code mode").expect("fixture");
        let session = medusa_core::SessionId::new();
        let provider_profile = json!({"provider": "test", "model": "test-model"});
        let execution_policy = json!({"policy": "read_only"});
        let contract = crate::agent_scope::prepare_agent_scope(
            directory.path(),
            &session,
            crate::agent_scope::AgentScopePreparation {
                mode: medusa_config::Mode::ReadOnly,
                provider_profile: provider_profile.clone(),
                execution_policy: execution_policy.clone(),
                effective_tools: vec!["fs_read".to_owned()],
                team_id: None,
                member_id: None,
                analysis_workspace: false,
            },
        )
        .expect("prepare scope");
        let scope = crate::agent_scope::publish_agent_scope(
            directory.path(),
            &contract,
            provider_profile,
            execution_policy,
            vec!["fs_read".to_owned()],
        )
        .expect("publish scope");
        let manager = ToolManager::new(Default::default());
        let program = CodeModeProgramV1 {
            schema_version: CODE_MODE_SCHEMA_VERSION,
            calls: vec![CodeModeCallV1 {
                tool: "fs_read".to_owned(),
                input: json!({"path": "fixture.txt"}),
            }],
        };
        manager
            .execute_code_mode_for_session(
                directory.path(),
                session.as_str(),
                &program,
                true,
                &AgentExecutionPolicy::unrestricted(),
                &AtomicBool::new(false),
            )
            .expect("scoped code mode execution");

        crate::agent_scope::revoke_agent_scope_tool(
            directory.path(),
            session.as_str(),
            &scope,
            "fs_read",
        )
        .expect("revoke scope tool");
        let error = manager
            .execute_code_mode_for_session(
                directory.path(),
                session.as_str(),
                &program,
                true,
                &AgentExecutionPolicy::unrestricted(),
                &AtomicBool::new(false),
            )
            .expect_err("revoked tool must not remain callable");
        assert!(error.to_string().contains("effective admitted SDK"));
    }
}
