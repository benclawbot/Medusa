mod context_budget {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/context_budget.rs"
    ));
}
mod coding_policy {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/coding_policy.rs"));
}
mod repository_index {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/repository_index.rs"
    ));
}
mod world_model_observation {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/world_model_observation.rs"
    ));
}
pub(crate) mod effective_request;
mod runtime_failure;

use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
    sync::{Arc, Mutex, atomic::AtomicBool},
    thread,
};

use medusa_config::{Config, Mode};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use medusa_extensions::{DesktopCommanderClient, DesktopCommanderSettings};
use medusa_protocol::{Actor, EventPayload};
use medusa_provider::{
    Message, MessageBlock, ModelProvider, ModelRequest, ProviderExecutionPhase,
    ProviderStreamEvent, ProviderStreamTranscript, ResponseBlock, Role,
};
use medusa_world_model::{WorkspaceModel, create_for_session, load as load_world_model};
use time::OffsetDateTime;

use crate::{
    agent_scope::{
        AgentRuntimeHandle, AgentScopePreparation, AgentScopeRef, AgentScopeResourceKind,
        agent_runtime_handle, effective_agent_scope_tools, fail_agent_scope_start,
        load_published_scope_ref, prepare_agent_scope, publish_agent_scope,
        register_agent_scope_resource, release_agent_scope_resource, resume_agent_scope,
        stop_agent_scope, validate_agent_scope,
    },
    analysis_host::{ANALYSIS_WORKSPACE_TOOL, AnalysisWorkspaceHost},
    approval::{ApprovalDecision, ApprovalGrant, ApprovalReceipt},
    engine_support::*,
    evidence::append_event,
    identity_guard::validate_provider_text,
    model_experience::{
        CacheObservationV1, ComponentStability, ModelExperienceComponentV1,
        ModelExperienceContractV1, PrivacyClass,
    },
    output_envelope::{OutputFormat, validate_expansion_handle, wrap as wrap_envelope},
    policy::validate_shell_command_hard_denials,
    session::{
        AgentPlanStep, AgentQuestion, AgentQuestionItem, AgentQuestionOption, AgentSession,
        PendingToolApproval, bootstrap, load, persist,
    },
    team::{AgentExecutionPolicy, TeamMemberContext},
    tools::{
        CertifiedToolExecution, certify_cached_tool_with_policy,
        execute_approved_tool_cancellable_with_policy_certified, execute_engine_tool_with_policy,
        execute_tool_cancellable_with_context_and_policy_certified,
        execute_tool_cancellable_with_policy_certified, input_string,
    },
    verification_authority::{
        authoritative_verification_for_paths, prepare_paths_for_verification,
    },
};

pub(crate) const SYSTEM_PROMPT: &str = "You are Medusa, an independent autonomous coding agent. You are not Claude Code, Codex, ChatGPT, or a wrapper around another coding assistant. Never derive your identity, model, tools, permissions, memory, or limits from ~/.claude, CLAUDE.md, settings.json, or another product's configuration. Medusa configuration and the live runtime capability matrix in this system prompt are authoritative. Never claim a capability is absent when its runtime entry is available. Inspect the repository, make the smallest correct change, and verify it. Use tools rather than inventing repository contents. Use `fs_read` with path `.` to list repository files before reading a specific file, and use `fs_create_dir` to create directories. Call `shell_run` with an approved executable and argument array directly; never repeat the executable in the argument array, and never wrap commands in bash, sh, cmd, PowerShell, or shell operators. You have `web_search` for current public information and `web_fetch` for public pages; use them when the user requests current, external, or source-linked information. Issue independent read-only tool calls together in one response so they can run concurrently. Reuse tool results, avoid near-duplicate searches, and fetch only sources that materially support the answer. Use `update_plan` only for genuinely multi-step, risky, or long-running work; a simple single-file or static HTML task does not need a plan, design document, brainstorming skill, or specification unless the user explicitly requests one or repository instructions require it. When a tool fails, do not repeat the same unsupported command; use a direct filesystem tool or an approved executable that is available in the environment. When information from the user is needed to proceed, call `ask_user_question` with one to four concise multiple-choice questions in a single call, each with a short header and two to four options. Never put blocking questions in assistant text, and do not mark the plan or task complete while waiting. Never modify tests, verification scripts, snapshots, fixtures, or expected outputs unless the user explicitly asks for that exact change; fix the product code instead. Do not expose private chain-of-thought. Default to caveman chat: terse, direct, concrete, usually one to three short sentences. Avoid preambles, repetition, and broad explanations unless the user asks for detail. Report only the decision, action, result, and essential evidence.";
const GENERAL_CHAT_SYSTEM_PROMPT: &str = "You are Medusa, a helpful general-purpose assistant. Answer the user's request directly and concisely, whether it is conversation, explanation, research, or confirmation. Do not inspect repositories, make plans, edit files, run shell commands, or use desktop tools unless the user explicitly asks for repository work. Use web_search or web_fetch only when current or source-linked information is needed. Do not invent follow-up work.";
pub(crate) const PLAN_SYSTEM_PROMPT: &str = "You are Medusa, an independent coding agent, in read-only planning mode. You are not Claude Code or a wrapper around another assistant. Never derive identity, model, configuration, tools, permissions, memory, or limits from ~/.claude, CLAUDE.md, settings.json, or another product. Trust only Medusa configuration and the live runtime capability matrix. Inspect the repository and produce a concise, ordered implementation plan grounded in the files you examined. Use `update_plan` to maintain the visible plan as your understanding changes. When clarification is necessary, call `ask_user_question` with one to four concise multiple-choice questions in a single call, each with a short header and two to four options, then wait for its answer before producing a final plan. You can use `web_search` and `web_fetch` for current public information. Do not modify files, create commits, or claim that implementation work has been completed. Only read-only repository and web tools are available. Do not expose private chain-of-thought. Use terse, direct language and an ordered plan without commentary or repetition.";

/// Result of one durable model/tool step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepOutcome {
    Continue,
    TurnComplete,
    WaitingForUser,
    Completed,
}

const MAX_PARALLEL_TOOL_CALLS: usize = 8;
const MIN_READ_ONLY_PHASE_OUTPUT_TOKENS: u32 = 2_048;

pub(crate) fn parallel_tool_limit(configured_workers: u16) -> usize {
    usize::from(configured_workers).clamp(1, MAX_PARALLEL_TOOL_CALLS)
}

fn phase_output_token_budget(phase: ProviderExecutionPhase, configured: u32) -> u32 {
    let divisor = match phase {
        ProviderExecutionPhase::Default | ProviderExecutionPhase::Implementation => 1,
        ProviderExecutionPhase::Repair => 2,
        ProviderExecutionPhase::Planning | ProviderExecutionPhase::HighRiskReview => 4,
        ProviderExecutionPhase::Summarization | ProviderExecutionPhase::Formatting => 8,
    };
    let budget = configured.div_ceil(divisor).max(1);
    if matches!(
        phase,
        ProviderExecutionPhase::Planning | ProviderExecutionPhase::HighRiskReview
    ) {
        budget.max(configured.min(MIN_READ_ONLY_PHASE_OUTPUT_TOKENS))
    } else {
        budget
    }
}

fn messages_with_turn_instruction(
    session: &AgentSession,
    turn_instruction: Option<&str>,
) -> Vec<Message> {
    let mut messages = session.messages.clone();
    if let Some(instruction) = turn_instruction
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        messages.push(Message {
            role: Role::User,
            content: vec![MessageBlock::Text {
                text: instruction.to_owned(),
            }],
        });
    }
    messages
}

pub(crate) fn map_parallel_ordered<T, U, F>(items: Vec<T>, operation: F) -> MedusaResult<Vec<U>>
where
    T: Send,
    U: Send,
    F: Fn(T) -> U + Sync,
{
    thread::scope(|scope| {
        let handles = items
            .into_iter()
            .map(|item| scope.spawn(|| operation(item)))
            .collect::<Vec<_>>();
        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            results.push(handle.join().map_err(|_| {
                MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Execution,
                    "parallel tool worker panicked",
                )
            })?);
        }
        Ok(results)
    })
}

/// A live update emitted while the engine executes one step.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentUpdate {
    Event(EventPayload),
    AssistantText(String),
    Plan(Vec<AgentPlanStep>),
    Question(AgentQuestion),
    ToolOutput {
        tool: String,
        output: String,
        is_error: bool,
    },
}

/// Persistent single-agent engine.
pub struct AgentEngine<P> {
    provider: P,
    config: Config,
    desktop_commander_settings: DesktopCommanderSettings,
    desktop_commander: Mutex<Option<DesktopCommanderClient>>,
    cancellation: Arc<AtomicBool>,
    execution_policy: AgentExecutionPolicy,
    runtime_config_fingerprint: Option<String>,
    runtime_config_binding: Option<(u16, String, serde_json::Value)>,
    team_context: Option<TeamMemberContext>,
    analysis_host: Option<Arc<dyn AnalysisWorkspaceHost>>,
    general_chat: bool,
}

fn refreshed_repository_revision(repo: &Path) -> Option<String> {
    crate::agent_scope::repository_revision(repo)
}

fn stale_revision_error(name: &str, current_revision: &str) -> MedusaError {
    let mut error = MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Execution,
        format!("tool {name} was cancelled because its repository revision became stale"),
    );
    error.context.insert(
        "stale_repository_revision".into(),
        serde_json::Value::Bool(true),
    );
    error.context.insert(
        "current_repository_revision".into(),
        serde_json::Value::String(current_revision.to_owned()),
    );
    error
}

fn dependency_failure_error(name: &str) -> MedusaError {
    let mut error = MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Execution,
        format!("tool {name} was cancelled because an authoritative dependency failed"),
    );
    error.context.insert(
        "authoritative_dependency_failed".into(),
        serde_json::Value::Bool(true),
    );
    error
}

fn journal_certified_tool_execution(
    session: &mut AgentSession,
    tool_use_id: &str,
    name: &str,
    input: &serde_json::Value,
    receipt: serde_json::Value,
    result: &MedusaResult<String>,
    execution_policy: &AgentExecutionPolicy,
) -> MedusaResult<()> {
    let scope = crate::agent_scope::load_published_scope_ref(&session.repo, session.id.as_str())?;
    let canonical = crate::tool_result::CanonicalToolResultV1::from_receipt(&receipt, result);
    append_event(
        session,
        Actor::Coordinator,
        EventPayload::WorkerEvidenceRecorded {
            evidence: serde_json::json!({
                "kind": "certified_tool_execution",
                "tool_use_id": tool_use_id,
                "tool": audited_tool_name(name, input),
                "receipt": receipt,
                "canonical_result": canonical.durable_evidence_projection(),
                "execution_authority": execution_policy.audit_projection(),
                "agent_scope_id": scope.scope_id,
                "agent_scope_fingerprint": scope.scope_fingerprint,
                "agent_scope_generation": scope.generation,
            }),
        },
    )?;
    session.updated_at = OffsetDateTime::now_utc();
    persist(session)
}

struct SessionToolAuthority<'a> {
    execution_policy: &'a AgentExecutionPolicy,
    session_id: &'a str,
    task_step_id: Option<&'a str>,
    activity_id: &'a str,
}

fn execute_session_tool(
    repo: &Path,
    name: &str,
    input: &serde_json::Value,
    cancellation: &AtomicBool,
    authority: SessionToolAuthority<'_>,
) -> MedusaResult<CertifiedToolExecution> {
    if name != "fs_write" {
        return execute_tool_cancellable_with_policy_certified(
            repo,
            name,
            input,
            cancellation,
            authority.execution_policy,
        );
    }

    let requested_path = input
        .get("path")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            MedusaError::new(
                ErrorCode::InvalidConfiguration,
                ErrorCategory::Validation,
                "fs_write path must be a string",
            )
        })?;

    // Absolute/external writes must reach the existing path-policy and approval boundary before
    // any provenance work. They are not repository mutations and cannot be selectively reverted.
    if Path::new(requested_path).is_absolute() {
        return execute_tool_cancellable_with_policy_certified(
            repo,
            name,
            input,
            cancellation,
            authority.execution_policy,
        );
    }

    // Non-Git workspaces remain writable, but repository-diff provenance is unavailable there.
    // Keep that limitation explicit instead of failing the write or manufacturing authority.
    let provenance_available = medusa_core::hidden_command("git")
        .args(["diff", "--binary", "--no-ext-diff", "--", "."])
        .current_dir(repo)
        .output()
        .is_ok_and(|output| output.status.success());
    if !provenance_available {
        let mut execution = execute_tool_cancellable_with_policy_certified(
            repo,
            name,
            input,
            cancellation,
            authority.execution_policy,
        )?;
        execution.result = execution.result.map(|output| {
            format!(
                "{output}; selective_revert=unavailable (workspace has no authoritative Git provenance)"
            )
        });
        return Ok(execution);
    }

    let sequence = crate::transaction::next_mutation_sequence(repo, authority.session_id)?;
    let occurred_at_unix_ms = i64::try_from(
        OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000,
    )
    .map_err(|_| {
        MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            "mutation timestamp overflow",
        )
    })?;
    let context = crate::transaction::MutationContext {
        session_id: authority.session_id.to_owned(),
        task_step_id: authority.task_step_id.map(str::to_owned),
        activity_id: authority.activity_id.to_owned(),
        actor: "medusa-agent".to_owned(),
        sequence,
        occurred_at_unix_ms,
    };
    execute_tool_cancellable_with_context_and_policy_certified(
        repo,
        name,
        input,
        cancellation,
        Some(&context),
        authority.execution_policy,
    )
}

fn active_plan_step_id(session: &AgentSession) -> Option<&str> {
    session
        .plan
        .iter()
        .find(|step| matches!(step.status, crate::session::AgentPlanStepStatus::InProgress))
        .map(|step| step.title.as_str())
}

fn audited_tool_name(name: &str, input: &serde_json::Value) -> String {
    if name == "desktop_commander" {
        if let Some(tool) = input.get("tool").and_then(serde_json::Value::as_str) {
            return format!("desktop_commander:{tool}");
        }
    }
    name.to_owned()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ToolExecutionTiming {
    queue_duration_ns: u64,
    execution_duration_ns: u64,
    cached: bool,
}

enum PostToolAction {
    PlanUpdated(Vec<AgentPlanStep>),
    AskQuestion(Box<AgentQuestion>),
}

struct EarlyToolExecution {
    name: String,
    input: serde_json::Value,
    execution: CertifiedToolExecution,
    requested_at: std::time::Instant,
    timing: ToolExecutionTiming,
}

fn stream_dispatch_safe_tool(name: &str, input: &serde_json::Value) -> bool {
    let profile = crate::tool_dag::profile(name, input);
    profile.side_effect == crate::tool_dag::SideEffectClass::None
        && profile.idempotent
        && profile.parallel_safe
        && profile.cancellation_supported
}

fn early_tool_identity_error(id: &str) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Execution,
        format!("streamed tool call {id} changed before provider completion"),
    )
}

fn duration_ns(duration: std::time::Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

fn model_experience_component(
    id: &str,
    order: u32,
    location: &str,
    stability: ComponentStability,
    value: &impl serde::Serialize,
    privacy_class: PrivacyClass,
    max_bytes: Option<u64>,
) -> ModelExperienceComponentV1 {
    let serialized = serde_json::to_vec(value).unwrap_or_default();
    let bytes = u64::try_from(serialized.len()).unwrap_or(u64::MAX);
    ModelExperienceComponentV1 {
        id: id.to_owned(),
        version: "1".to_owned(),
        insertion_order: order,
        location: location.to_owned(),
        stability,
        estimated_tokens: Some(bytes.saturating_add(3) / 4),
        actual_tokens: None,
        estimated_bytes: Some(bytes),
        actual_bytes: Some(bytes),
        cache_eligible: Some(matches!(
            stability,
            ComponentStability::Static | ComponentStability::SessionStable
        )),
        cache_breaking_dimensions: match stability {
            ComponentStability::Static | ComponentStability::SessionStable => {
                vec!["authority_state".to_owned(), "tool_schema".to_owned()]
            }
            ComponentStability::TurnStable | ComponentStability::RequestDynamic => {
                vec!["turn".to_owned(), "request_content".to_owned()]
            }
        },
        privacy_class,
        max_bytes,
        fingerprint: effective_request::fragment_fingerprint(&String::from_utf8_lossy(&serialized)),
    }
}

fn model_experience_contract(
    phase: ProviderExecutionPhase,
    request: &ModelRequest,
) -> ModelExperienceContractV1 {
    let components = vec![
        model_experience_component(
            "system",
            0,
            "system",
            ComponentStability::SessionStable,
            &request.system,
            PrivacyClass::SecretExcluded,
            Some(256 * 1024),
        ),
        model_experience_component(
            "messages",
            1,
            "conversation",
            ComponentStability::RequestDynamic,
            &request.messages,
            PrivacyClass::UserContent,
            Some(2 * 1024 * 1024),
        ),
        model_experience_component(
            "tools",
            2,
            "tool_schema",
            ComponentStability::SessionStable,
            &request.tools,
            PrivacyClass::Public,
            Some(512 * 1024),
        ),
        model_experience_component(
            "response_budget",
            3,
            "request_parameters",
            ComponentStability::TurnStable,
            &(&request.max_tokens, &request.temperature_milli),
            PrivacyClass::Public,
            None,
        ),
    ];
    let tool_schema_fingerprint = effective_request::fragment_fingerprint(
        &serde_json::to_string(&request.tools).unwrap_or_default(),
    );
    ModelExperienceContractV1::new(format!("{phase:?}"), components, tool_schema_fingerprint)
}

fn attach_model_experience_measurement(
    mut usage: serde_json::Value,
    contract: &ModelExperienceContractV1,
) -> serde_json::Value {
    let cache = if usage.get("provenance").and_then(serde_json::Value::as_str)
        == Some("provider_reported")
    {
        let read_tokens = usage
            .get("cache_read_input_tokens")
            .and_then(serde_json::Value::as_u64);
        let write_tokens = usage
            .get("cache_creation_input_tokens")
            .and_then(serde_json::Value::as_u64);
        CacheObservationV1::Observed {
            read_tokens,
            write_tokens,
            hit: read_tokens.map(|tokens| tokens > 0),
        }
    } else {
        CacheObservationV1::Unknown
    };
    let measurement = contract.measurement(cache);
    if let Some(fields) = usage.as_object_mut() {
        fields.insert(
            "model_experience_measurement".to_owned(),
            serde_json::to_value(measurement).unwrap_or_else(|_| {
                serde_json::json!({
                    "schema_version": crate::model_experience::MODEL_EXPERIENCE_SCHEMA_VERSION
                })
            }),
        );
    }
    usage
}

#[cfg(test)]
mod model_experience_usage_tests {
    use super::*;

    fn contract() -> ModelExperienceContractV1 {
        model_experience_contract(
            ProviderExecutionPhase::Default,
            &ModelRequest {
                system: "system".to_owned(),
                messages: Vec::new(),
                tools: Vec::new(),
                max_tokens: 128,
                temperature_milli: 0,
            },
        )
    }

    #[test]
    fn provider_usage_records_observed_cache_measurement() {
        let usage = attach_model_experience_measurement(
            serde_json::json!({
                "provenance": "provider_reported",
                "cache_read_input_tokens": 120,
                "cache_creation_input_tokens": 8,
            }),
            &contract(),
        );
        assert_eq!(
            usage["model_experience_measurement"]["cache"]["status"],
            "observed"
        );
        assert_eq!(
            usage["model_experience_measurement"]["cache"]["read_tokens"],
            120
        );
        assert_eq!(usage["model_experience_measurement"]["cache"]["hit"], true);
    }

    #[test]
    fn estimated_usage_keeps_cache_unknown() {
        let usage = attach_model_experience_measurement(
            serde_json::json!({
                "provenance": "estimated",
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 0,
            }),
            &contract(),
        );
        assert_eq!(
            usage["model_experience_measurement"]["cache"]["status"],
            "unknown"
        );
    }
}

impl<P: ModelProvider> AgentEngine<P> {
    #[must_use]
    pub fn new(provider: P, config: Config) -> Self {
        Self {
            provider,
            config,
            desktop_commander_settings: DesktopCommanderSettings::from_env(),
            desktop_commander: Mutex::new(None),
            cancellation: Arc::new(AtomicBool::new(false)),
            execution_policy: AgentExecutionPolicy::unrestricted(),
            runtime_config_fingerprint: None,
            runtime_config_binding: None,
            team_context: None,
            analysis_host: None,
            general_chat: false,
        }
    }

    #[must_use]
    pub fn new_with_cancellation(
        provider: P,
        config: Config,
        cancellation: Arc<AtomicBool>,
    ) -> Self {
        Self {
            provider,
            config,
            desktop_commander_settings: DesktopCommanderSettings::from_env(),
            desktop_commander: Mutex::new(None),
            cancellation,
            execution_policy: AgentExecutionPolicy::unrestricted(),
            runtime_config_fingerprint: None,
            runtime_config_binding: None,
            team_context: None,
            analysis_host: None,
            general_chat: false,
        }
    }

    #[must_use]
    pub fn with_execution_policy(mut self, policy: AgentExecutionPolicy) -> Self {
        self.execution_policy = policy;
        self
    }

    /// Binds the versioned runtime-loop configuration to every effective request manifest.
    ///
    /// The runtime owns compilation and validation of this fingerprint; the agent only records
    /// it as assembly provenance so replay/audit can distinguish configuration generations.
    #[must_use]
    pub fn with_runtime_config_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.runtime_config_fingerprint = Some(fingerprint.into());
        self
    }

    /// Binds a redacted, versioned effective runtime configuration to new sessions.
    ///
    /// The binding is persisted in the canonical session journal and is intentionally separate
    /// from secrets or process-local provider credentials.
    #[must_use]
    pub fn with_runtime_config_binding(
        mut self,
        schema_version: u16,
        fingerprint: impl Into<String>,
        snapshot: serde_json::Value,
    ) -> Self {
        let fingerprint = fingerprint.into();
        self.runtime_config_fingerprint = Some(fingerprint.clone());
        self.runtime_config_binding = Some((schema_version, fingerprint, snapshot));
        self
    }

    #[must_use]
    pub fn with_team_context(mut self, context: TeamMemberContext) -> Self {
        self.team_context = Some(context);
        self
    }

    #[must_use]
    pub fn with_analysis_workspace_host(mut self, host: Arc<dyn AnalysisWorkspaceHost>) -> Self {
        self.analysis_host = Some(host);
        self
    }

    /// Uses the small, tool-limited request path for ordinary conversation.
    #[must_use]
    pub fn with_general_chat(mut self, enabled: bool) -> Self {
        self.general_chat = enabled;
        self
    }

    fn scope_provider_profile(&self) -> MedusaResult<serde_json::Value> {
        serde_json::to_value(&self.config.model).map_err(json_error)
    }

    fn bind_runtime_config_provenance(&self, provenance: &mut BTreeMap<String, String>) {
        if let Some(fingerprint) = self
            .runtime_config_fingerprint
            .as_deref()
            .filter(|fingerprint| !fingerprint.trim().is_empty())
        {
            provenance.insert(
                "runtime_config_fingerprint".to_owned(),
                fingerprint.to_owned(),
            );
        }
    }

    fn scope_effective_tools(&self, repo: &Path) -> MedusaResult<Vec<String>> {
        let mut tools = available_tools(
            self.config.agent.mode,
            repo,
            &self.desktop_commander_settings,
        )?;
        if let Some(team) = &self.team_context {
            tools.extend(team.definitions());
        }
        if self.analysis_host.is_some() {
            tools.push(crate::analysis_host::tool_definition());
        }
        tools.retain(|tool| self.execution_policy.allows(&tool.name));
        let mut names = tools.into_iter().map(|tool| tool.name).collect::<Vec<_>>();
        names.sort();
        names.dedup();
        Ok(names)
    }

    fn scope_team_identity(&self) -> MedusaResult<(Option<String>, Option<String>)> {
        let Some(team) = &self.team_context else {
            return Ok((None, None));
        };
        Ok((
            Some(team.team_id().map_err(|error| {
                MedusaError::new(ErrorCode::InternalInvariant, ErrorCategory::Internal, error)
            })?),
            Some(team.member_id().to_owned()),
        ))
    }

    fn scoped_runtime_tools(&self, session: &AgentSession) -> MedusaResult<Vec<String>> {
        effective_agent_scope_tools(
            &session.repo,
            session.id.as_str(),
            self.scope_effective_tools(&session.repo)?,
        )
    }

    pub fn runtime_handle(&self, session: &AgentSession) -> MedusaResult<AgentRuntimeHandle> {
        agent_runtime_handle(
            &session.repo,
            session.id.as_str(),
            Arc::clone(&self.cancellation),
        )
    }

    fn validate_scope(&self, session: &AgentSession) -> MedusaResult<AgentScopeRef> {
        validate_agent_scope(
            &session.repo,
            session.id.as_str(),
            self.scope_provider_profile()?,
            self.execution_policy.audit_projection(),
            self.scope_effective_tools(&session.repo)?,
        )
    }

    pub fn stop_session_scope(
        &self,
        session: &AgentSession,
        cause: impl Into<String>,
    ) -> MedusaResult<crate::agent_scope::AgentScopeStopReceipt> {
        if let Ok(mut client) = self.desktop_commander.lock() {
            client.take();
        }
        stop_agent_scope(&session.repo, session.id.as_str(), cause)
    }

    fn execute_desktop_commander(
        &self,
        repo: &Path,
        session_id: &str,
        input: &serde_json::Value,
    ) -> MedusaResult<String> {
        let tool = input_string(input, "tool")?;
        let arguments = input.get("arguments").ok_or_else(|| {
            MedusaError::new(
                ErrorCode::InvalidConfiguration,
                ErrorCategory::Validation,
                "desktop_commander.arguments must be an object",
            )
        })?;
        let mut client = self.desktop_commander.lock().map_err(|_| {
            MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                "Desktop Commander client lock was poisoned",
            )
        })?;
        if client.is_none() {
            let scope = load_published_scope_ref(repo, session_id)?;
            register_agent_scope_resource(
                repo,
                session_id,
                &scope,
                "desktop-commander",
                AgentScopeResourceKind::DesktopCommander,
            )?;
            match DesktopCommanderClient::connect(repo, self.desktop_commander_settings.clone()) {
                Ok(connected) => *client = Some(connected),
                Err(error) => {
                    let _ =
                        release_agent_scope_resource(repo, session_id, &scope, "desktop-commander");
                    return Err(error);
                }
            }
        }
        let initialized = client.as_mut().ok_or_else(|| {
            MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                "Desktop Commander client was not initialized after a successful connection",
            )
        })?;
        let result = initialized.call_tool(
            repo,
            tool,
            arguments,
            self.config.agent.mode == Mode::ReadOnly,
        );
        if result.is_err() {
            client.take();
        }
        serde_json::to_string_pretty(&result?).map_err(Into::into)
    }

    pub fn create_session(&self, repo: &Path, objective: String) -> MedusaResult<AgentSession> {
        self.create_session_with_id(repo, SessionId::new(), objective)
    }

    pub fn create_session_with_id(
        &self,
        repo: &Path,
        id: SessionId,
        objective: String,
    ) -> MedusaResult<AgentSession> {
        self.create_session_with_content_and_id(
            repo,
            id,
            objective.clone(),
            vec![MessageBlock::Text { text: objective }],
        )
    }

    pub fn create_session_with_content(
        &self,
        repo: &Path,
        objective: String,
        content: Vec<MessageBlock>,
    ) -> MedusaResult<AgentSession> {
        self.create_session_with_content_and_id(repo, SessionId::new(), objective, content)
    }

    fn create_session_with_content_and_id(
        &self,
        repo: &Path,
        id: SessionId,
        objective: String,
        content: Vec<MessageBlock>,
    ) -> MedusaResult<AgentSession> {
        let content = content_with_session_goal(content, &objective);
        validate_user_content(&content, &self.provider.capabilities())?;
        if let Some((schema_version, fingerprint, snapshot)) = &self.runtime_config_binding {
            if *schema_version == 0 || fingerprint.trim().is_empty() {
                return Err(MedusaError::new(
                    ErrorCode::InvalidConfiguration,
                    ErrorCategory::Validation,
                    "runtime configuration binding requires a schema version and fingerprint",
                ));
            }
            if snapshot.is_null() {
                return Err(MedusaError::new(
                    ErrorCode::InvalidConfiguration,
                    ErrorCategory::Validation,
                    "runtime configuration binding requires a redacted snapshot",
                ));
            }
            let snapshot_schema = snapshot
                .get("schema_version")
                .and_then(serde_json::Value::as_u64);
            let snapshot_fingerprint = snapshot
                .get("fingerprint")
                .and_then(serde_json::Value::as_str);
            if snapshot_schema != Some(u64::from(*schema_version))
                || snapshot_fingerprint != Some(fingerprint.as_str())
            {
                return Err(MedusaError::new(
                    ErrorCode::InvalidConfiguration,
                    ErrorCategory::Validation,
                    "runtime configuration binding snapshot does not match its identity",
                ));
            }
        }
        bootstrap(repo)?;
        medusa_intelligence::recover_patch_transactions(repo)?;
        let effective_tools = self.scope_effective_tools(repo)?;
        let provider_profile = self.scope_provider_profile()?;
        let execution_policy = self.execution_policy.audit_projection();
        let (team_id, member_id) = self.scope_team_identity()?;
        let scope = prepare_agent_scope(
            repo,
            &id,
            AgentScopePreparation {
                mode: self.config.agent.mode,
                provider_profile: provider_profile.clone(),
                execution_policy: execution_policy.clone(),
                effective_tools: effective_tools.clone(),
                team_id,
                member_id,
                analysis_workspace: self.analysis_host.is_some(),
            },
        )?;
        if let Err(error) = publish_agent_scope(
            repo,
            &scope,
            provider_profile,
            execution_policy,
            effective_tools,
        ) {
            let _ = fail_agent_scope_start(repo, id.as_str(), error.to_string());
            return Err(error);
        }
        let now = OffsetDateTime::now_utc();
        let world_model = create_for_session(repo, id.as_str(), objective.clone()).ok();
        let mut session = AgentSession {
            id: id.clone(),
            objective: objective.clone(),
            repo: repo.to_path_buf(),
            created_at: now,
            updated_at: now,
            completed: false,
            turn: 0,
            plan: Vec::new(),
            pending_question: None,
            messages: vec![Message {
                role: Role::User,
                content,
            }],
            events: Vec::new(),
            evidence: Vec::new(),
            tool_artifacts: Vec::new(),
            world_model,
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
        };
        if let Err(error) = append_event(
            &mut session,
            Actor::User,
            EventPayload::SessionCreated { objective },
        ) {
            let _ = fail_agent_scope_start(repo, id.as_str(), error.to_string());
            return Err(error);
        }
        if let Some((schema_version, fingerprint, snapshot)) = &self.runtime_config_binding {
            if let Err(error) = append_event(
                &mut session,
                Actor::Coordinator,
                EventPayload::RuntimeConfigurationBound {
                    schema_version: *schema_version,
                    fingerprint: fingerprint.clone(),
                    snapshot: snapshot.clone(),
                },
            ) {
                let _ = fail_agent_scope_start(repo, id.as_str(), error.to_string());
                return Err(error);
            }
        }
        if let Err(error) = persist(&session) {
            let _ = fail_agent_scope_start(repo, id.as_str(), error.to_string());
            return Err(error);
        }
        Ok(session)
    }

    pub fn load_session(&self, repo: &Path, session: &str) -> MedusaResult<AgentSession> {
        medusa_intelligence::recover_patch_transactions(repo)?;
        let loaded = load(repo, session)?;
        resume_agent_scope(
            repo,
            loaded.id.as_str(),
            self.scope_provider_profile()?,
            self.execution_policy.audit_projection(),
            self.scope_effective_tools(repo)?,
        )?;
        Ok(loaded)
    }

    /// Loads the durable evidence model associated with a session, when enabled.
    pub fn load_session_world_model(
        &self,
        session: &AgentSession,
    ) -> MedusaResult<Option<WorkspaceModel>> {
        let Some(reference) = &session.world_model else {
            return Ok(None);
        };
        load_world_model(&session.repo, reference)
            .map(Some)
            .map_err(|error| {
                MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Internal,
                    format!("failed to load session world model: {error}"),
                )
            })
    }

    /// Adds a follow-up prompt to an existing session so later turns retain context.
    pub fn append_user_message(
        &self,
        session: &mut AgentSession,
        mut content: Vec<MessageBlock>,
    ) -> MedusaResult<()> {
        self.validate_scope(session)?;
        content.insert(
            0,
            MessageBlock::Text {
                text: format!("Current session goal: {}", session.objective),
            },
        );
        validate_user_content(&content, &self.provider.capabilities())?;
        let text = compact_message_text(&content);
        session.completed = false;
        session.turn = 0;
        session.messages.push(Message {
            role: Role::User,
            content,
        });
        append_event(
            session,
            Actor::User,
            EventPayload::UserPromptReceived { text },
        )?;
        session.updated_at = OffsetDateTime::now_utc();
        persist(session)
    }

    /// Applies one previously accepted queued follow-up exactly once.
    pub fn append_queued_user_message(
        &self,
        session: &mut AgentSession,
        command_id: String,
        mut content: Vec<MessageBlock>,
    ) -> MedusaResult<()> {
        self.validate_scope(session)?;
        content.insert(
            0,
            MessageBlock::Text {
                text: format!("Current session goal: {}", session.objective),
            },
        );
        validate_user_content(&content, &self.provider.capabilities())?;
        let text = compact_message_text(&content);
        session.completed = false;
        session.turn = 0;
        session.messages.push(Message {
            role: Role::User,
            content,
        });
        append_event(
            session,
            Actor::User,
            EventPayload::UserFollowupDequeued { command_id, text },
        )?;
        session.updated_at = OffsetDateTime::now_utc();
        persist(session)
    }

    /// Resolves a blocking question with a single user response and resumes the same session.
    pub fn answer_pending_question(
        &self,
        session: &mut AgentSession,
        content: Vec<MessageBlock>,
    ) -> MedusaResult<()> {
        self.validate_scope(session)?;
        let question = session.pending_question.take().ok_or_else(|| {
            MedusaError::new(
                ErrorCode::InvalidConfiguration,
                ErrorCategory::Validation,
                "there is no pending question to answer",
            )
        })?;
        validate_user_content(&content, &self.provider.capabilities())?;
        let answer = compact_message_text(&content);
        if answer.trim().is_empty() {
            session.pending_question = Some(question);
            return Err(MedusaError::new(
                ErrorCode::InvalidConfiguration,
                ErrorCategory::Validation,
                "a question response cannot be empty",
            ));
        }
        session.completed = false;
        session.turn = 0;
        let content = if let Some(approval) = question.approval {
            let approved = answer.trim().eq_ignore_ascii_case("approve")
                || answer.trim().to_ascii_lowercase().starts_with("approve ");
            let now = OffsetDateTime::now_utc();
            let decision = if approved {
                approval
                    .grant
                    .authorizes(&approval.tool, &approval.input, &session.plan, now)
            } else {
                ApprovalDecision::Denied
            };
            let receipt = ApprovalReceipt {
                decision: decision.clone(),
                scope: approval.grant.scope.clone(),
                recorded_at: now,
                reason: if approved {
                    "user approved exact action".to_owned()
                } else {
                    format!("user denied action: {answer}")
                },
            };
            session.approval_receipts.push(receipt.clone());
            if decision == ApprovalDecision::Approved {
                session.approval_grants.push(approval.grant.clone());
            }
            append_event(
                session,
                Actor::User,
                EventPayload::ApprovalDecisionRecorded {
                    decision: serde_json::json!({
                        "receipt": receipt,
                        "tool_use_id": &approval.tool_use_id,
                        "tool": &approval.tool,
                    }),
                },
            )?;
            session.updated_at = now;
            persist(session)?;

            let (content, is_error) = if decision == ApprovalDecision::Approved {
                let scoped_tools = self.scoped_runtime_tools(session)?;
                if scoped_tools.binary_search(&approval.tool).is_err() {
                    return Err(MedusaError::new(
                        ErrorCode::PolicyDenied,
                        ErrorCategory::Policy,
                        format!(
                            "approved tool {} was revoked from the active agent scope before execution",
                            approval.tool
                        ),
                    ));
                }
                let event_tool = audited_tool_name(&approval.tool, &approval.input);
                append_event(
                    session,
                    Actor::Coordinator,
                    EventPayload::ToolExecutionStarted {
                        tool: event_tool.clone(),
                    },
                )?;
                session.updated_at = OffsetDateTime::now_utc();
                persist(session)?;
                let execution = execute_approved_tool_cancellable_with_policy_certified(
                    &session.repo,
                    &approval.tool,
                    &approval.input,
                    self.cancellation.as_ref(),
                    &self.execution_policy,
                )?;
                journal_certified_tool_execution(
                    session,
                    &approval.tool_use_id,
                    &approval.tool,
                    &approval.input,
                    execution.receipt,
                    &execution.result,
                    &self.execution_policy,
                )?;
                let result = execution.result;
                append_event(
                    session,
                    Actor::Coordinator,
                    EventPayload::ToolExecutionCompleted {
                        tool: event_tool,
                        exit_code: Some(if result.is_ok() { 0 } else { 1 }),
                    },
                )?;
                session.updated_at = OffsetDateTime::now_utc();
                persist(session)?;
                match result {
                    Ok(output) => (format!("User approved this exact action.\n{output}"), false),
                    Err(error) => (format!("Approved action failed: {error}"), true),
                }
            } else {
                (
                    format!("Action was not authorized ({decision:?}). Feedback: {answer}"),
                    true,
                )
            };
            vec![MessageBlock::ToolResult {
                tool_use_id: approval.tool_use_id,
                content,
                is_error,
            }]
        } else {
            match question.tool_use_id {
                Some(tool_use_id) => vec![MessageBlock::ToolResult {
                    tool_use_id,
                    content: format!("User response: {answer}"),
                    is_error: false,
                }],
                None => vec![MessageBlock::Text {
                    text: format!("User response to the clarification question: {answer}"),
                }],
            }
        };
        session.messages.push(Message {
            role: Role::User,
            content,
        });
        append_event(
            session,
            Actor::User,
            EventPayload::UserPromptReceived { text: answer },
        )?;
        append_event(session, Actor::Coordinator, EventPayload::SessionResumed)?;
        session.updated_at = OffsetDateTime::now_utc();
        persist(session)
    }

    /// Updates the durable session objective without creating a new conversation.
    pub fn update_objective(
        &self,
        session: &mut AgentSession,
        objective: String,
    ) -> MedusaResult<()> {
        update_session_objective(session, objective)
    }

    /// Replaces prior message history with a bounded durable summary for the next model request.
    pub fn compact_session(
        &self,
        session: &mut AgentSession,
        focus: Option<&str>,
    ) -> MedusaResult<()> {
        compact_session(session, focus)
    }

    fn compact_session_v2(
        &self,
        session: &mut AgentSession,
        focus: Option<&str>,
    ) -> MedusaResult<()> {
        let summary_request = crate::compaction_v2::semantic_summary_request(session, focus);
        let summary_model_experience =
            model_experience_contract(ProviderExecutionPhase::Summarization, &summary_request);
        let summary_provenance = BTreeMap::from([
            (
                "compaction_v2_system".to_owned(),
                effective_request::fragment_fingerprint(&summary_request.system),
            ),
            (
                "compaction_v2_focus".to_owned(),
                effective_request::fragment_fingerprint(focus.unwrap_or_default()),
            ),
        ]);
        let mut summary_provenance = summary_provenance;
        self.bind_runtime_config_provenance(&mut summary_provenance);
        summary_provenance.insert(
            "model_experience_contract".to_owned(),
            summary_model_experience.fingerprint(),
        );
        summary_provenance.insert(
            "model_experience_total_bytes".to_owned(),
            summary_model_experience
                .measurement(CacheObservationV1::Unknown)
                .total_bytes
                .map_or_else(|| "unknown".to_owned(), |bytes| bytes.to_string()),
        );
        summary_provenance.insert("model_experience_cache".to_owned(), "unknown".to_owned());
        let execution_policy = self.execution_policy.audit_projection();
        let capabilities = self.provider.capabilities();
        let manifest = effective_request::persist_before_provider_call(
            session,
            &summary_request,
            effective_request::RequestManifestInput {
                phase: ProviderExecutionPhase::Summarization,
                provider: &self.config.model.provider,
                model: &self.config.model.name,
                capabilities: &capabilities,
                execution_policy: &execution_policy,
                assembly_provenance: summary_provenance,
                previous: None,
            },
        )?;
        append_event(
            session,
            Actor::Coordinator,
            effective_request::started_event(
                &manifest,
                &self.config.model.provider,
                &self.config.model.name,
            ),
        )?;
        session.updated_at = OffsetDateTime::now_utc();
        persist(session)?;
        let request_started = std::time::Instant::now();
        let audit_repo = session.repo.clone();
        let audit_session_id = session.id.to_string();
        let mut before_provider_attempt = |attempt: &medusa_provider::ProviderAttemptDescriptor| {
            effective_request::persist_provider_attempt(
                &audit_repo,
                &audit_session_id,
                &manifest,
                attempt,
            )
            .map(|_| ())
        };
        let semantic = match self.provider.complete_cancellable_for_phase_with_attempts(
            &summary_request,
            ProviderExecutionPhase::Summarization,
            &self.cancellation,
            &mut before_provider_attempt,
        ) {
            Ok(response) => {
                let turn_usage = crate::session::record_turn_usage(
                    session.turn,
                    &summary_request,
                    &response,
                    request_started.elapsed(),
                );
                append_event(
                    session,
                    Actor::Coordinator,
                    effective_request::response_event(
                        &manifest,
                        response.response_id.clone(),
                        attach_model_experience_measurement(
                            serde_json::to_value(turn_usage).map_err(json_error)?,
                            &summary_model_experience,
                        ),
                    ),
                )?;
                session.updated_at = OffsetDateTime::now_utc();
                persist(session)?;
                crate::compaction_v2::validate_semantic_response(
                    &response,
                    &self.config.model.name,
                    &self.config.model.provider,
                )
            }
            Err(error) => {
                append_event(
                    session,
                    Actor::Coordinator,
                    effective_request::failure_event(&manifest, &error),
                )?;
                session.updated_at = OffsetDateTime::now_utc();
                persist(session)?;
                None
            }
        };
        crate::engine_support::compact_session_with_semantic(session, focus, semantic)
    }

    pub fn run_to_completion(&self, session: &mut AgentSession) -> MedusaResult<()> {
        let default_phase = provider_execution_phase(self.config.agent.mode);
        let mut phase = default_phase;
        while !session.completed && session.turn < self.config.agent.max_turns {
            match self.step_for_provider_phase(session, phase) {
                Ok(StepOutcome::WaitingForUser) => {
                    let error = MedusaError::new(
                        ErrorCode::DependencyUnavailable,
                        ErrorCategory::Execution,
                        "agent is waiting for a user response",
                    );
                    let _ = runtime_failure::handle(session, &error)?;
                    return Err(error);
                }
                Ok(StepOutcome::TurnComplete) => return Ok(()),
                Ok(StepOutcome::Continue | StepOutcome::Completed) => {
                    phase = default_phase;
                }
                Err(error) => match runtime_failure::handle(session, &error)? {
                    runtime_failure::RuntimeFailureAction::Retry => continue,
                    runtime_failure::RuntimeFailureAction::Replan => {
                        phase = ProviderExecutionPhase::Repair;
                        continue;
                    }
                    runtime_failure::RuntimeFailureAction::Stop => return Err(error),
                },
            }
        }
        if session.completed {
            Ok(())
        } else {
            let error = MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Execution,
                "agent exhausted max_turns before verification passed",
            );
            runtime_failure::record_terminal(
                session,
                &error,
                "agent exhausted its bounded runtime without passing verification",
            )?;
            Err(error)
        }
    }

    pub fn step(&self, session: &mut AgentSession) -> MedusaResult<StepOutcome> {
        self.step_with_observer(session, |_| {})
    }

    fn step_for_provider_phase(
        &self,
        session: &mut AgentSession,
        phase: ProviderExecutionPhase,
    ) -> MedusaResult<StepOutcome> {
        self.step_with_observer_and_context_and_turn_instruction_for_phase(
            session,
            None,
            None,
            phase,
            |_| {},
        )
    }

    pub fn step_with_observer<F>(
        &self,
        session: &mut AgentSession,
        observer: F,
    ) -> MedusaResult<StepOutcome>
    where
        F: FnMut(&AgentUpdate),
    {
        self.step_with_observer_and_context(session, None, observer)
    }

    pub fn step_with_observer_and_context<F>(
        &self,
        session: &mut AgentSession,
        additional_system_context: Option<&str>,
        observer: F,
    ) -> MedusaResult<StepOutcome>
    where
        F: FnMut(&AgentUpdate),
    {
        self.step_with_observer_and_context_and_turn_instruction(
            session,
            additional_system_context,
            None,
            observer,
        )
    }

    /// Executes one model step with ephemeral system context and an optional latest-turn
    /// instruction. It is not appended to conversational session history, but the exact effective
    /// request is retained through the protected request-manifest authority for audit/replay.
    pub fn step_with_observer_and_context_and_turn_instruction<F>(
        &self,
        session: &mut AgentSession,
        additional_system_context: Option<&str>,
        turn_instruction: Option<&str>,
        observer: F,
    ) -> MedusaResult<StepOutcome>
    where
        F: FnMut(&AgentUpdate),
    {
        self.step_with_observer_and_context_and_turn_instruction_for_phase(
            session,
            additional_system_context,
            turn_instruction,
            provider_execution_phase(self.config.agent.mode),
            observer,
        )
    }

    pub fn step_with_observer_and_context_and_turn_instruction_for_phase<F>(
        &self,
        session: &mut AgentSession,
        additional_system_context: Option<&str>,
        turn_instruction: Option<&str>,
        phase: ProviderExecutionPhase,
        mut observer: F,
    ) -> MedusaResult<StepOutcome>
    where
        F: FnMut(&AgentUpdate),
    {
        self.validate_scope(session)?;
        if session.completed {
            return Ok(StepOutcome::Completed);
        }
        if session.pending_question.is_some() {
            return Ok(StepOutcome::WaitingForUser);
        }
        validate_messages(&session.messages, &self.provider.capabilities())?;
        session.turn = session.turn.saturating_add(1);
        if !self.general_chat
            && let Some(refresh) = repository_index::refresh(&session.repo)?
        {
            observer(&AgentUpdate::ToolOutput {
                tool: "code_index".to_owned(),
                output: repository_index::summary(&refresh),
                is_error: false,
            });
        }
        let mut assembly_provenance = BTreeMap::new();
        self.bind_runtime_config_provenance(&mut assembly_provenance);
        if let Some(context) = additional_system_context.filter(|text| !text.trim().is_empty()) {
            assembly_provenance.insert(
                "additional_system_context".to_owned(),
                effective_request::fragment_fingerprint(context),
            );
        }
        if let Some(instruction) = turn_instruction.filter(|text| !text.trim().is_empty()) {
            assembly_provenance.insert(
                "turn_instruction".to_owned(),
                effective_request::fragment_fingerprint(instruction),
            );
        }
        let mut system = if self.general_chat {
            GENERAL_CHAT_SYSTEM_PROMPT.to_owned()
        } else {
            coding_policy::apply(
                system_prompt_with_context(
                    self.config.agent.mode,
                    &session.repo,
                    additional_system_context,
                ),
                self.config.agent.mode,
            )
        };
        assembly_provenance.insert(
            "base_system_projection".to_owned(),
            effective_request::fragment_fingerprint(&system),
        );
        if let Some(team) = &self.team_context {
            let team_context = team.prompt_context()?;
            assembly_provenance.insert(
                "team_context".to_owned(),
                effective_request::fragment_fingerprint(&team_context),
            );
            system.push_str("\n\n");
            system.push_str(&team_context);
        }
        if let Some(branch_context) = crate::branch_summary::advisory_context(session) {
            assembly_provenance.insert(
                "branch_summary".to_owned(),
                effective_request::fragment_fingerprint(&branch_context),
            );
            system.push_str("\n\n");
            system.push_str(&branch_context);
        }
        let mut tools = available_tools(
            self.config.agent.mode,
            &session.repo,
            &self.desktop_commander_settings,
        )?;
        if let Some(team) = &self.team_context {
            tools.extend(team.definitions());
        }
        if self.analysis_host.is_some() {
            tools.push(crate::analysis_host::tool_definition());
        }
        if self.general_chat {
            tools.retain(|tool| matches!(tool.name.as_str(), "web_search" | "web_fetch"));
        }
        tools.retain(|tool| self.execution_policy.allows(&tool.name));
        let current_tool_names = tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        let scoped_tool_names =
            effective_agent_scope_tools(&session.repo, session.id.as_str(), current_tool_names)?;
        tools.retain(|tool| scoped_tool_names.binary_search(&tool.name).is_ok());
        let mut request_messages = messages_with_turn_instruction(session, turn_instruction);
        validate_messages(&request_messages, &self.provider.capabilities())?;
        let requested_output_tokens =
            phase_output_token_budget(phase, self.config.model.max_output_tokens);
        let context_window_tokens = context_budget::configured_context_window_tokens(
            self.config.model.context_window_tokens,
        );
        let mut budget = context_budget::PromptBudget::for_request(
            &system,
            &request_messages,
            &tools,
            requested_output_tokens,
            context_window_tokens,
        );
        let repository_capacity = budget
            .compaction_threshold_tokens
            .saturating_sub(budget.estimated_total_tokens);
        if !self.general_chat
            && let Some(retrieval) = repository_index::retrieve_context(
                &session.repo,
                &session.objective,
                repository_capacity,
            )?
        {
            assembly_provenance.insert(
                "repository_context".to_owned(),
                effective_request::fragment_fingerprint(&retrieval.system_fragment),
            );
            system.push_str("\n\n");
            system.push_str(&retrieval.system_fragment);
            observer(&AgentUpdate::ToolOutput {
                tool: "repository_context".to_owned(),
                output: retrieval.status,
                is_error: false,
            });
            budget = context_budget::PromptBudget::for_request(
                &system,
                &request_messages,
                &tools,
                requested_output_tokens,
                context_window_tokens,
            );
        }
        let mut compacted = false;
        if matches!(
            budget.decision(),
            context_budget::PromptBudgetDecision::Compact
        ) {
            self.compact_session_v2(
                session,
                Some("preserve the current objective, decisions, tool results, and pending work"),
            )?;
            validate_messages(&session.messages, &self.provider.capabilities())?;
            request_messages = messages_with_turn_instruction(session, turn_instruction);
            validate_messages(&request_messages, &self.provider.capabilities())?;
            compacted = true;
        }
        budget = context_budget::PromptBudget::for_request(
            &system,
            &request_messages,
            &tools,
            requested_output_tokens,
            context_window_tokens,
        );
        let max_output_tokens = budget.response_token_budget(requested_output_tokens);
        let mut request = ModelRequest {
            system,
            messages: request_messages,
            tools,
            max_tokens: max_output_tokens,
            temperature_milli: self.config.model.temperature_milli,
        };
        let model_experience = model_experience_contract(phase, &request);
        assembly_provenance.insert(
            "model_experience_contract".to_owned(),
            model_experience.fingerprint(),
        );
        assembly_provenance.insert(
            "model_experience_estimated_tokens".to_owned(),
            model_experience
                .estimated_total_tokens
                .map_or_else(|| "unknown".to_owned(), |tokens| tokens.to_string()),
        );
        let model_experience_measurement =
            model_experience.measurement(CacheObservationV1::Unknown);
        assembly_provenance.insert(
            "model_experience_total_bytes".to_owned(),
            model_experience_measurement
                .total_bytes
                .map_or_else(|| "unknown".to_owned(), |bytes| bytes.to_string()),
        );
        assembly_provenance.insert(
            "model_experience_stable_prefix_bytes".to_owned(),
            model_experience_measurement
                .stable_prefix_bytes
                .map_or_else(|| "unknown".to_owned(), |bytes| bytes.to_string()),
        );
        assembly_provenance.insert("model_experience_cache".to_owned(), "unknown".to_owned());
        for (id, fingerprint) in model_experience.component_fingerprints() {
            assembly_provenance.insert(format!("model_experience_component:{id}"), fingerprint);
        }
        let execution_policy = self.execution_policy.audit_projection();
        let capabilities = self.provider.capabilities();
        let mut active_manifest = effective_request::persist_before_provider_call(
            session,
            &request,
            effective_request::RequestManifestInput {
                phase,
                provider: &self.config.model.provider,
                model: &self.config.model.name,
                capabilities: &capabilities,
                execution_policy: &execution_policy,
                assembly_provenance: assembly_provenance.clone(),
                previous: None,
            },
        )?;
        append_observed(
            session,
            effective_request::started_event(
                &active_manifest,
                &self.config.model.provider,
                &self.config.model.name,
            ),
            &mut observer,
        )?;
        let request_started = std::time::Instant::now();
        let streaming = self.provider.capabilities().streaming;
        let mut stream_transcript = ProviderStreamTranscript::default();
        let mut streamed_text = String::new();
        let mut stream_text_rejected = false;
        let mut early_tool_executions = BTreeMap::<String, EarlyToolExecution>::new();
        let streaming_repo = session.repo.clone();
        let audit_repo = session.repo.clone();
        let audit_session_id = session.id.to_string();
        macro_rules! complete_request {
            ($request:expr, $manifest:expr) => {{
                let mut before_provider_attempt = |attempt: &medusa_provider::ProviderAttemptDescriptor| {
                    effective_request::persist_provider_attempt(
                        &audit_repo,
                        &audit_session_id,
                        $manifest,
                        attempt,
                    )
                    .map(|_| ())
                };
                if !streaming {
                    self.provider.complete_cancellable_for_phase_with_attempts(
                        $request,
                        phase,
                        &self.cancellation,
                        &mut before_provider_attempt,
                    )
                } else {
                    let mut sink = |event: ProviderStreamEvent| {
                        stream_transcript.push(event.clone())?;
                        match event {
                            ProviderStreamEvent::TextDelta { text } if !stream_text_rejected => {
                                streamed_text.push_str(&text);
                                if validate_provider_text(&streamed_text).is_ok() {
                                    observer(&AgentUpdate::AssistantText(text));
                                } else {
                                    stream_text_rejected = true;
                                    observer(&AgentUpdate::AssistantText(
                                        "[provider output rejected: identity or policy contamination]"
                                            .to_owned(),
                                    ));
                                }
                            }
                            ProviderStreamEvent::ToolUseReady { id, name, input }
                                if !early_tool_executions.contains_key(&id)
                                    && stream_dispatch_safe_tool(&name, &input)
                                    && tool_allowed(self.config.agent.mode, &name)
                                    && scoped_tool_names.binary_search(&name).is_ok() =>
                            {
                                let requested_at = std::time::Instant::now();
                                let started = std::time::Instant::now();
                                if let Ok(execution) =
                                    execute_tool_cancellable_with_policy_certified(
                                        &streaming_repo,
                                        &name,
                                        &input,
                                        self.cancellation.as_ref(),
                                        &self.execution_policy,
                                    ) && execution.result.is_ok()
                                {
                                    early_tool_executions.insert(
                                        id,
                                        EarlyToolExecution {
                                            name,
                                            input,
                                            execution,
                                            requested_at,
                                            timing: ToolExecutionTiming {
                                                queue_duration_ns: 0,
                                                execution_duration_ns: duration_ns(started.elapsed()),
                                                cached: false,
                                            },
                                        },
                                    );
                                }
                            }
                            _ => {}
                        }
                        Ok(())
                    };
                    self.provider
                        .complete_streaming_cancellable_for_phase_with_attempts(
                            $request,
                            phase,
                            &self.cancellation,
                            &mut before_provider_attempt,
                            &mut sink,
                        )
                }
            }};
        }
        let response = match complete_request!(&request, &active_manifest) {
            Ok(response) => response,
            Err(error) if context_budget::is_context_limit_rejection(&error.to_string()) => {
                append_observed(
                    session,
                    effective_request::failure_event(&active_manifest, &error),
                    &mut observer,
                )?;
                if !compacted {
                    self.compact_session_v2(
                        session,
                        Some(
                            "recover from the provider context limit while preserving the current objective, decisions, tool results, and pending work",
                        ),
                    )?;
                    validate_messages(&session.messages, &self.provider.capabilities())?;
                    request.messages = messages_with_turn_instruction(session, turn_instruction);
                    validate_messages(&request.messages, &self.provider.capabilities())?;
                    let retry_budget = context_budget::PromptBudget::for_request(
                        &request.system,
                        &request.messages,
                        &request.tools,
                        requested_output_tokens,
                        context_window_tokens,
                    );
                    request.max_tokens =
                        retry_budget.response_token_budget(requested_output_tokens);
                }
                let retry_capabilities = self.provider.capabilities();
                let retry_manifest = effective_request::persist_before_provider_call(
                    session,
                    &request,
                    effective_request::RequestManifestInput {
                        phase,
                        provider: &self.config.model.provider,
                        model: &self.config.model.name,
                        capabilities: &retry_capabilities,
                        execution_policy: &execution_policy,
                        assembly_provenance: assembly_provenance.clone(),
                        previous: Some(&active_manifest),
                    },
                )?;
                append_observed(
                    session,
                    effective_request::started_event(
                        &retry_manifest,
                        &self.config.model.provider,
                        &self.config.model.name,
                    ),
                    &mut observer,
                )?;
                active_manifest = retry_manifest;
                match complete_request!(&request, &active_manifest) {
                    Ok(response) => response,
                    Err(error) => {
                        append_observed(
                            session,
                            effective_request::failure_event(&active_manifest, &error),
                            &mut observer,
                        )?;
                        return Err(error);
                    }
                }
            }
            Err(error) => {
                append_observed(
                    session,
                    effective_request::failure_event(&active_manifest, &error),
                    &mut observer,
                )?;
                return Err(error);
            }
        };
        let turn_usage = crate::session::record_turn_usage(
            session.turn,
            &request,
            &response,
            request_started.elapsed(),
        );
        append_observed(
            session,
            effective_request::response_event(
                &active_manifest,
                response.response_id.clone(),
                attach_model_experience_measurement(
                    serde_json::to_value(turn_usage).map_err(json_error)?,
                    &model_experience,
                ),
            ),
            &mut observer,
        )?;
        append_observed(
            session,
            EventPayload::ProviderExecutionRecorded {
                status: effective_request::augment_execution_status(
                    self.provider.execution_status(),
                    &active_manifest,
                ),
            },
            &mut observer,
        )?;

        let mut assistant_blocks = Vec::new();
        let mut assistant_text = Vec::new();
        let mut calls = VecDeque::new();
        let mut tool_requested_at = BTreeMap::<String, std::time::Instant>::new();
        for block in response.blocks {
            match block {
                ResponseBlock::Text { text } => {
                    let text = if validate_provider_text(&text).is_ok() {
                        text
                    } else {
                        "[provider output rejected: identity or policy contamination]".to_owned()
                    };
                    assistant_text.push(text.clone());
                    assistant_blocks.push(MessageBlock::Text { text });
                }
                ResponseBlock::ToolUse { id, name, input } => {
                    if let Some(early) = early_tool_executions.get(&id)
                        && (early.name != name || early.input != input)
                    {
                        return Err(early_tool_identity_error(&id));
                    }
                    assistant_blocks.push(MessageBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    });
                    let requested_at = early_tool_executions
                        .get(&id)
                        .map_or_else(std::time::Instant::now, |early| early.requested_at);
                    tool_requested_at.insert(id.clone(), requested_at);
                    calls.push_back((id, name, input));
                }
            }
        }
        if !assistant_blocks.is_empty() {
            let message = Message {
                role: Role::Assistant,
                content: assistant_blocks,
            };
            session.messages.push(message.clone());
            append_observed(
                session,
                EventPayload::AssistantMessageRecorded {
                    message: serde_json::to_value(&message).map_err(json_error)?,
                },
                &mut observer,
            )?;
        }
        let fallback_question = calls
            .is_empty()
            .then(|| question_from_assistant_text(&assistant_text.join("\n")))
            .flatten();
        if fallback_question.is_none()
            && !assistant_text.is_empty()
            && (!streaming || streamed_text.is_empty())
        {
            observer(&AgentUpdate::AssistantText(assistant_text.join("\n")));
        }

        if let Some(question) = fallback_question {
            pause_for_question(session, question, &mut observer)?;
            return Ok(StepOutcome::WaitingForUser);
        }

        let had_tool_calls = !calls.is_empty();

        let mut safe_tool_cache = BTreeMap::<String, String>::new();
        while !calls.is_empty() {
            let schedulable = calls
                .iter()
                .map(|(_, name, input)| (name.clone(), input.clone()))
                .collect::<Vec<_>>();
            let positions = calls
                .iter()
                .position(|(_, name, _)| {
                    scoped_tool_names.binary_search(name).is_err()
                        || name == ANALYSIS_WORKSPACE_TOOL
                        || name == "update_plan"
                        || name == "ask_user_question"
                        || name == "desktop_commander"
                        || self
                            .team_context
                            .as_ref()
                            .is_some_and(|team| team.handles(name))
                })
                .or_else(|| {
                    calls
                        .iter()
                        .position(|(id, _, _)| early_tool_executions.contains_key(id))
                })
                .map_or_else(
                    || {
                        crate::tool_dag::select_ready_positions(
                            &schedulable,
                            parallel_tool_limit(self.config.agent.parallel_workers),
                        )
                    },
                    |position| vec![position],
                );
            let batch = crate::tool_dag::drain_positions(&mut calls, &positions);
            let parallel_count = batch.len();
            for (_, name, input) in &batch {
                append_observed(
                    session,
                    EventPayload::ToolCallRequested {
                        tool: audited_tool_name(name, input),
                        arguments: input.clone(),
                    },
                    &mut observer,
                )?;
            }

            let executed = if parallel_count > 1 {
                for (_, name, input) in &batch {
                    let cached = crate::tool_dag::dedup_key(name, input)
                        .is_some_and(|key| safe_tool_cache.contains_key(&key));
                    if !cached {
                        append_observed(
                            session,
                            EventPayload::ToolExecutionStarted {
                                tool: audited_tool_name(name, input),
                            },
                            &mut observer,
                        )?;
                    }
                }
                let repo = session.repo.as_path();
                let mutation_session_id = session.id.to_string();
                let mutation_task_step = active_plan_step_id(session).map(str::to_owned);
                let cache = &safe_tool_cache;
                let requested_at = &tool_requested_at;
                let cancellation = Arc::clone(&self.cancellation);
                let execution_policy = self.execution_policy.clone();
                let executed = map_parallel_ordered(batch, |(id, name, input)| {
                    let started = std::time::Instant::now();
                    let queue_duration_ns = requested_at
                        .get(&id)
                        .map(|requested| duration_ns(started.duration_since(*requested)))
                        .unwrap_or_default();
                    let cached_output = crate::tool_dag::dedup_key(&name, &input)
                        .and_then(|key| cache.get(&key).cloned());
                    let cached = cached_output.is_some();
                    let execution = cached_output.map_or_else(
                        || {
                            execute_session_tool(
                                repo,
                                &name,
                                &input,
                                cancellation.as_ref(),
                                SessionToolAuthority {
                                    execution_policy: &execution_policy,
                                    session_id: &mutation_session_id,
                                    task_step_id: mutation_task_step.as_deref(),
                                    activity_id: &id,
                                },
                            )
                        },
                        |output| {
                            certify_cached_tool_with_policy(
                                repo,
                                &name,
                                &input,
                                cancellation.as_ref(),
                                &execution_policy,
                                output,
                            )
                        },
                    );
                    let timing = ToolExecutionTiming {
                        queue_duration_ns,
                        execution_duration_ns: duration_ns(started.elapsed()),
                        cached,
                    };
                    (id, name, input, execution, Some(timing))
                })?;
                executed
                    .into_iter()
                    .map(|(id, name, input, execution, timing)| {
                        let execution = execution?;
                        Ok((
                            id,
                            name,
                            input,
                            execution.result,
                            Some(execution.receipt),
                            execution.canonical,
                            timing,
                            None,
                        ))
                    })
                    .collect::<MedusaResult<Vec<_>>>()?
            } else {
                let (id, name, input) = batch.into_iter().next().ok_or_else(|| {
                    MedusaError::new(
                        ErrorCode::InternalInvariant,
                        ErrorCategory::Execution,
                        "tool batch was unexpectedly empty",
                    )
                })?;
                let started = std::time::Instant::now();
                let queue_duration_ns = tool_requested_at
                    .get(&id)
                    .map(|requested| duration_ns(started.duration_since(*requested)))
                    .unwrap_or_default();
                let mut measured = false;
                let mut cached = false;
                let mut timing_override = None;
                let mut post_action = None;
                let execution = if scoped_tool_names.binary_search(&name).is_err() {
                    measured = true;
                    append_observed(
                        session,
                        EventPayload::ToolExecutionStarted {
                            tool: audited_tool_name(&name, &input),
                        },
                        &mut observer,
                    )?;
                    execute_engine_tool_with_policy(
                        &name,
                        &input,
                        self.cancellation.as_ref(),
                        &self.execution_policy,
                        |_| {
                            Err(MedusaError::new(
                                ErrorCode::PolicyDenied,
                                ErrorCategory::Policy,
                                format!("tool {name} is revoked or outside the active agent scope"),
                            ))
                        },
                    )?
                } else if let Some(early) = early_tool_executions.remove(&id) {
                    if early.name != name || early.input != input {
                        return Err(early_tool_identity_error(&id));
                    }
                    measured = true;
                    timing_override = Some(early.timing);
                    append_observed(
                        session,
                        EventPayload::ToolExecutionStarted {
                            tool: audited_tool_name(&name, &input),
                        },
                        &mut observer,
                    )?;
                    early.execution
                } else if name == ANALYSIS_WORKSPACE_TOOL {
                    measured = true;
                    append_observed(
                        session,
                        EventPayload::ToolExecutionStarted {
                            tool: audited_tool_name(&name, &input),
                        },
                        &mut observer,
                    )?;
                    let host = self.analysis_host.as_ref();
                    let session_id = session.id.to_string();
                    execute_engine_tool_with_policy(
                        &name,
                        &input,
                        self.cancellation.as_ref(),
                        &self.execution_policy,
                        |canonical_input| {
                            let host = host.ok_or_else(|| {
                                MedusaError::new(
                                    ErrorCode::PolicyDenied,
                                    ErrorCategory::Policy,
                                    "analysis workspace authority is unavailable",
                                )
                            })?;
                            host.execute(&session_id, canonical_input, self.cancellation.as_ref())
                        },
                    )?
                } else if name == "update_plan" {
                    measured = true;
                    append_observed(
                        session,
                        EventPayload::ToolExecutionStarted {
                            tool: audited_tool_name(&name, &input),
                        },
                        &mut observer,
                    )?;
                    execute_engine_tool_with_policy(
                        &name,
                        &input,
                        self.cancellation.as_ref(),
                        &self.execution_policy,
                        |canonical_input| {
                            let plan = plan_from_input(canonical_input);
                            if plan.is_empty() {
                                Ok("Visible task plan update ignored because it was empty."
                                    .to_owned())
                            } else {
                                if session.plan != plan {
                                    let recorded_at = OffsetDateTime::now_utc();
                                    for grant in session.approval_grants.drain(..) {
                                        session.approval_receipts.push(ApprovalReceipt {
                                            decision: ApprovalDecision::Invalidated,
                                            scope: grant.scope,
                                            recorded_at,
                                            reason: "visible plan changed".to_owned(),
                                        });
                                    }
                                }
                                session.plan = plan.clone();
                                post_action = Some(PostToolAction::PlanUpdated(plan));
                                Ok("Visible task plan updated.".to_owned())
                            }
                        },
                    )?
                } else if name == "ask_user_question" {
                    measured = true;
                    append_observed(
                        session,
                        EventPayload::ToolExecutionStarted {
                            tool: audited_tool_name(&name, &input),
                        },
                        &mut observer,
                    )?;
                    execute_engine_tool_with_policy(
                        &name,
                        &input,
                        self.cancellation.as_ref(),
                        &self.execution_policy,
                        |canonical_input| {
                            let question = question_from_input(id.clone(), canonical_input)?;
                            post_action = Some(PostToolAction::AskQuestion(Box::new(question)));
                            Ok("User question prepared.".to_owned())
                        },
                    )?
                } else if self
                    .team_context
                    .as_ref()
                    .is_some_and(|team| team.handles(&name))
                {
                    measured = true;
                    append_observed(
                        session,
                        EventPayload::ToolExecutionStarted {
                            tool: audited_tool_name(&name, &input),
                        },
                        &mut observer,
                    )?;
                    execute_engine_tool_with_policy(
                        &name,
                        &input,
                        self.cancellation.as_ref(),
                        &self.execution_policy,
                        |canonical_input| {
                            self.team_context
                                .as_ref()
                                .ok_or_else(|| {
                                    MedusaError::new(
                                        ErrorCode::InternalInvariant,
                                        ErrorCategory::Internal,
                                        "team tool context disappeared",
                                    )
                                })?
                                .execute(&name, canonical_input)
                        },
                    )?
                } else if name == "desktop_commander" && tool_allowed(self.config.agent.mode, &name)
                {
                    measured = true;
                    append_observed(
                        session,
                        EventPayload::ToolExecutionStarted {
                            tool: audited_tool_name(&name, &input),
                        },
                        &mut observer,
                    )?;
                    execute_engine_tool_with_policy(
                        &name,
                        &input,
                        self.cancellation.as_ref(),
                        &self.execution_policy,
                        |canonical_input| {
                            self.execute_desktop_commander(
                                &session.repo,
                                session.id.as_str(),
                                canonical_input,
                            )
                        },
                    )?
                } else if tool_allowed(self.config.agent.mode, &name) {
                    measured = true;
                    if let Some(output) = crate::tool_dag::dedup_key(&name, &input)
                        .and_then(|key| safe_tool_cache.get(&key).cloned())
                    {
                        cached = true;
                        certify_cached_tool_with_policy(
                            &session.repo,
                            &name,
                            &input,
                            self.cancellation.as_ref(),
                            &self.execution_policy,
                            output,
                        )?
                    } else {
                        append_observed(
                            session,
                            EventPayload::ToolExecutionStarted {
                                tool: audited_tool_name(&name, &input),
                            },
                            &mut observer,
                        )?;
                        execute_session_tool(
                            &session.repo,
                            &name,
                            &input,
                            self.cancellation.as_ref(),
                            SessionToolAuthority {
                                execution_policy: &self.execution_policy,
                                session_id: session.id.as_str(),
                                task_step_id: active_plan_step_id(session),
                                activity_id: &id,
                            },
                        )?
                    }
                } else {
                    let reason = "tool is unavailable in read-only planning mode".to_owned();
                    execute_engine_tool_with_policy(
                        &name,
                        &input,
                        self.cancellation.as_ref(),
                        &self.execution_policy,
                        |_| {
                            Err(MedusaError::new(
                                ErrorCode::PolicyDenied,
                                ErrorCategory::Policy,
                                reason,
                            ))
                        },
                    )?
                };
                let timing = timing_override.or_else(|| {
                    measured.then(|| ToolExecutionTiming {
                        queue_duration_ns,
                        execution_duration_ns: duration_ns(started.elapsed()),
                        cached,
                    })
                });
                vec![(
                    id,
                    name,
                    input,
                    execution.result,
                    Some(execution.receipt),
                    execution.canonical,
                    timing,
                    post_action,
                )]
            };

            let repository_revision_after_mutation = executed
                .iter()
                .any(|(_, name, input, result, _, _, _, _)| {
                    result.is_ok() && crate::tool_dag::invalidates_repository_revision(name, input)
                })
                .then(|| refreshed_repository_revision(&session.repo))
                .flatten();

            let verification_mutation_paths = executed
                .iter()
                .filter(|(_, name, input, result, _, _, _, _)| {
                    result.is_ok() && crate::tool_dag::invalidates_repository_revision(name, input)
                })
                .filter_map(|(_, name, input, _, _, _, _, _)| {
                    matches!(name.as_str(), "fs_write" | "fs_create_dir")
                        .then(|| input.get("path").and_then(serde_json::Value::as_str))
                        .flatten()
                        .filter(|path| !Path::new(path).is_absolute())
                        .map(|path| path.replace('\\', "/"))
                })
                .collect::<Vec<_>>();
            if repository_revision_after_mutation.is_some() {
                crate::verification_contract::refresh_persisted_after_mutation(
                    &session.repo,
                    session.id.as_str(),
                    &verification_mutation_paths,
                    self.config.model.max_output_tokens,
                )?;
            }

            let failed_dependencies = executed
                .iter()
                .filter_map(|(_, name, input, result, _, _, _, _)| {
                    let error = result.as_ref().err()?;
                    let awaiting_approval = error.code == ErrorCode::PolicyDenied
                        && self.config.agent.mode != Mode::ReadOnly
                        && self.execution_policy.denial_reason(name, input).is_none()
                        && interactively_approvable(name, input);
                    (!awaiting_approval).then(|| (name.clone(), input.clone()))
                })
                .collect::<Vec<_>>();
            let blocked =
                crate::tool_dag::drain_failed_dependents(&mut calls, &failed_dependencies);
            let mut executed = executed;
            for (id, name, input) in blocked {
                let error = dependency_failure_error(&name);
                let execution = execute_engine_tool_with_policy(
                    &name,
                    &input,
                    self.cancellation.as_ref(),
                    &self.execution_policy,
                    |_| Err(error),
                )?;
                executed.push((
                    id,
                    name,
                    input,
                    execution.result,
                    Some(execution.receipt),
                    execution.canonical,
                    None,
                    None,
                ));
            }
            if let Some(current_revision) = repository_revision_after_mutation {
                let stale =
                    crate::tool_dag::drain_stale_revision_calls(&mut calls, &current_revision);
                for (id, name, input) in stale {
                    let error = stale_revision_error(&name, &current_revision);
                    let execution = execute_engine_tool_with_policy(
                        &name,
                        &input,
                        self.cancellation.as_ref(),
                        &self.execution_policy,
                        |_| Err(error),
                    )?;
                    executed.push((
                        id,
                        name,
                        input,
                        execution.result,
                        Some(execution.receipt),
                        execution.canonical,
                        None,
                        None,
                    ));
                }
            }

            for (id, name, input, result, receipt, canonical, timing, post_action) in executed {
                let canonical_model_projection = Some(canonical.model_projection(8 * 1024));
                if let Some(receipt) = receipt {
                    journal_certified_tool_execution(
                        session,
                        &id,
                        &name,
                        &input,
                        receipt,
                        &result,
                        &self.execution_policy,
                    )?;
                }
                if let Some(PostToolAction::PlanUpdated(plan)) = post_action.as_ref()
                    && result.is_ok()
                {
                    append_observed(
                        session,
                        EventPayload::PlanUpdated {
                            update: serde_json::to_value(plan).map_err(json_error)?,
                        },
                        &mut observer,
                    )?;
                    observer(&AgentUpdate::Plan(plan.clone()));
                }
                let awaiting_approval = result.as_ref().err().is_some_and(|error| {
                    error.code == ErrorCode::PolicyDenied
                        && self.config.agent.mode != Mode::ReadOnly
                        && self.execution_policy.denial_reason(&name, &input).is_none()
                        && interactively_approvable(&name, &input)
                });
                if let Err(error) = &result
                    && error.code == ErrorCode::PolicyDenied
                {
                    append_observed(
                        session,
                        EventPayload::ToolCallDenied {
                            tool: audited_tool_name(&name, &input),
                            reason: if awaiting_approval {
                                "tool requires explicit user approval".to_owned()
                            } else {
                                error.to_string()
                            },
                        },
                        &mut observer,
                    )?;
                }
                if awaiting_approval {
                    let action = approval_action_label(&name, &input);
                    pause_for_question(
                        session,
                        AgentQuestion {
                            tool_use_id: Some(id.clone()),
                            questions: vec![AgentQuestionItem {
                                header: "Permission".to_owned(),
                                question: format!("Allow Medusa to {action}?"),
                                options: vec![
                                    AgentQuestionOption {
                                        label: "Approve".to_owned(),
                                        description: "Allow this exact action once".to_owned(),
                                    },
                                    AgentQuestionOption {
                                        label: "Deny".to_owned(),
                                        description: "Do not run this action".to_owned(),
                                    },
                                    AgentQuestionOption {
                                        label: "Provide feedback".to_owned(),
                                        description: "Type a different instruction below"
                                            .to_owned(),
                                    },
                                ],
                                multi_select: false,
                            }],
                            legacy_question: None,
                            legacy_options: Vec::new(),
                            approval: Some(PendingToolApproval {
                                grant: ApprovalGrant::exact_action(
                                    &name,
                                    &input,
                                    &session.plan,
                                    OffsetDateTime::now_utc(),
                                ),
                                tool_use_id: id,
                                tool: name,
                                input,
                            }),
                        },
                        &mut observer,
                    )?;
                    return Ok(StepOutcome::WaitingForUser);
                }
                let event_tool = audited_tool_name(&name, &input);
                let exit_code = Some(if result.is_ok() { 0 } else { 1 });
                append_observed(
                    session,
                    EventPayload::ToolExecutionCompleted {
                        tool: event_tool.clone(),
                        exit_code,
                    },
                    &mut observer,
                )?;
                if let Some(PostToolAction::AskQuestion(question)) = post_action
                    && result.is_ok()
                {
                    pause_for_question(session, *question, &mut observer)?;
                    return Ok(StepOutcome::WaitingForUser);
                }
                if let Some(timing) = timing {
                    let profile = crate::tool_dag::profile(&name, &input);
                    append_observed(
                        session,
                        EventPayload::ToolExecutionTimingRecorded {
                            tool_use_id: id.clone(),
                            tool: event_tool,
                            queue_duration_ns: timing.queue_duration_ns,
                            execution_duration_ns: timing.execution_duration_ns,
                            expected_duration_ms: profile.expected_duration_ms,
                            concurrency_cost: profile.concurrency_cost,
                            cached: timing.cached,
                        },
                        &mut observer,
                    )?;
                }
                let (raw_content, is_error) = match result {
                    Ok(output) => (output, false),
                    Err(error) => (error.to_string(), true),
                };
                if !is_error && let Some(key) = crate::tool_dag::dedup_key(&name, &input) {
                    safe_tool_cache
                        .entry(key)
                        .or_insert_with(|| raw_content.clone());
                }
                world_model_observation::record_tool_observation(
                    session,
                    &name,
                    &input,
                    &raw_content,
                    if is_error { 1 } else { 0 },
                );
                // The TUI sees the full body verbatim; the model sees the compact
                // head/tail envelope with a pointer to the on-disk artifact.
                observer(&AgentUpdate::ToolOutput {
                    tool: name.clone(),
                    output: raw_content.clone(),
                    is_error,
                });
                let envelope_cfg = default_envelope_config(&session.repo);
                let model_content = match wrap_envelope(
                    &name,
                    raw_content.as_bytes(),
                    OutputFormat::Plain,
                    &envelope_cfg,
                ) {
                    Ok(env) => {
                        let compact = compact_envelope_for_model(&env);
                        // Persist the artifact path on the session for later reference (cleanup,
                        // replay, and explicit model expansion).
                        session.tool_artifacts.push(env.path.clone());
                        if let Some(mut projection) = canonical_model_projection {
                            projection["rendered"] = serde_json::Value::String(if is_error {
                                format!("[error]\n{compact}")
                            } else {
                                compact.clone()
                            });
                            let expansion_handle =
                                validate_expansion_handle(&session.repo, &env.path)
                                    .ok()
                                    .map(|path| path.display().to_string());
                            projection["rendering"] = serde_json::json!({
                                "line_count": env.line_count,
                                "byte_count": env.byte_count,
                                "expansion_available": expansion_handle.is_some(),
                                "expansion_handle": expansion_handle,
                            });
                            serde_json::to_string(&projection).unwrap_or_else(|_| {
                                if is_error {
                                    format!("[error]\n{compact}")
                                } else {
                                    compact
                                }
                            })
                        } else if is_error {
                            format!("[error]\n{compact}")
                        } else {
                            compact
                        }
                    }
                    Err(_) => {
                        // Envelope wrap failed (rare — disk full, perms). Fall back
                        // to the raw body so the model still sees output.
                        raw_content.clone()
                    }
                };
                session.messages.push(Message {
                    role: Role::User,
                    content: vec![MessageBlock::ToolResult {
                        tool_use_id: id,
                        content: model_content,
                        is_error,
                    }],
                });
                persist(session)?;
            }
        }

        if response_completes_turn(response.stop_reason.as_deref(), had_tool_calls)
            && !session.messages.last().is_some_and(|message| {
                matches!(
                    message.content.first(),
                    Some(MessageBlock::ToolResult { .. })
                )
            })
        {
            if self.config.agent.mode == Mode::ReadOnly || !has_mutating_tool_result(session) {
                session.updated_at = OffsetDateTime::now_utc();
                persist(session)?;
                return Ok(StepOutcome::TurnComplete);
            }
            append_observed(
                session,
                EventPayload::VerificationStarted {
                    commands: Vec::new(),
                },
                &mut observer,
            )?;
            let changed_paths = successful_mutation_paths(session);
            prepare_paths_for_verification(&session.repo, &changed_paths)?;
            crate::verification_contract::refresh_persisted_after_mutation(
                &session.repo,
                session.id.as_str(),
                &changed_paths,
                self.config.model.max_output_tokens,
            )?;
            let mut verification = match authoritative_verification_for_paths(
                &session.repo,
                &changed_paths,
            ) {
                Ok(verification) => verification,
                Err(error) => {
                    crate::verification_contract::mark_persisted_unavailable(
                        &session.repo,
                        session.id.as_str(),
                        &error.to_string(),
                    )?;
                    let evidence = vec![format!("verification_unavailable={error}")];
                    append_observed(
                        session,
                        EventPayload::VerificationCompleted {
                            passed: false,
                            evidence: evidence.clone(),
                        },
                        &mut observer,
                    )?;
                    session.evidence.extend(evidence.clone());
                    session.messages.push(Message {
                        role: Role::User,
                        content: vec![MessageBlock::Text {
                            text: format!(
                                "Required verification is unavailable. The coding task remains incomplete. Evidence:\n{}",
                                evidence.join("\n")
                            ),
                        }],
                    });
                    session.updated_at = OffsetDateTime::now_utc();
                    persist(session)?;
                    return Ok(StepOutcome::TurnComplete);
                }
            };
            let transaction_ids = medusa_intelligence::finalize_patch_transactions(
                &session.repo,
                verification.passed,
            )?;
            verification
                .evidence
                .extend(transaction_ids.into_iter().map(|transaction_id| {
                    if verification.passed {
                        format!("patch_transaction_committed={transaction_id}")
                    } else {
                        format!("patch_transaction_rolled_back={transaction_id}")
                    }
                }));
            append_observed(
                session,
                EventPayload::VerificationCompleted {
                    passed: verification.passed,
                    evidence: verification.evidence.clone(),
                },
                &mut observer,
            )?;
            session.evidence.extend(verification.evidence.clone());
            let contract = crate::verification_contract::apply_persisted_authoritative_summary(
                &session.repo,
                session.id.as_str(),
                &verification.evidence,
                verification.passed,
            )?;
            let contract_ready = contract.completion_ready(&session.repo)?
                && crate::verification_contract::completion_ready(
                    &session.repo,
                    session.id.as_str(),
                )?;
            if verification.passed && contract_ready && plan_is_complete(session) {
                session.completed = true;
                append_observed(
                    session,
                    EventPayload::SessionCompleted {
                        report_ref: format!("session:{}.json", session.id),
                    },
                    &mut observer,
                )?;
            } else if verification.passed && !contract_ready {
                let unresolved = crate::verification_contract::unresolved_summary(
                    &session.repo,
                    session.id.as_str(),
                )?;
                session.evidence.extend(unresolved.clone());
                session.messages.push(Message {
                    role: Role::User,
                    content: vec![MessageBlock::Text {
                        text: format!(
                            "Authoritative checks ran, but the mandatory verification contract is unresolved. The task remains incomplete. Evidence:\n{}",
                            unresolved.join("\n")
                        ),
                    }],
                });
            } else if !verification.passed {
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
        } else if response_completes_turn(response.stop_reason.as_deref(), had_tool_calls) {
            StepOutcome::TurnComplete
        } else {
            StepOutcome::Continue
        })
    }
}

fn stop_reason_completes_turn(stop_reason: Option<&str>) -> bool {
    matches!(stop_reason.map(str::trim), Some("end_turn" | "stop"))
}

/// Text-only responses are terminal unless the provider explicitly reports truncation.
fn response_completes_turn(stop_reason: Option<&str>, had_tool_calls: bool) -> bool {
    if had_tool_calls {
        return false;
    }
    if stop_reason_completes_turn(stop_reason) {
        return true;
    }
    !matches!(
        stop_reason.map(str::trim),
        Some("length" | "max_tokens" | "content_filter")
    )
}

fn approval_action_label(name: &str, input: &serde_json::Value) -> String {
    match name {
        "fs_write" => format!(
            "write {}",
            input
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("the requested file")
        ),
        "fs_create_dir" => format!(
            "create {}",
            input
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("the requested directory")
        ),
        "shell_run" => format!(
            "run {} {}",
            input
                .get("program")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("the requested command"),
            input
                .get("args")
                .and_then(serde_json::Value::as_array)
                .map(|args| args
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "))
                .unwrap_or_default()
        )
        .trim()
        .to_owned(),
        _ => "run the requested action".to_owned(),
    }
}

fn interactively_approvable(name: &str, input: &serde_json::Value) -> bool {
    match name {
        "fs_write" | "fs_create_dir" => input
            .get("path")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|path| Path::new(path).is_absolute()),
        "shell_run" => {
            let Some(program) = input.get("program").and_then(serde_json::Value::as_str) else {
                return false;
            };
            let Some(args) = input.get("args").and_then(serde_json::Value::as_array) else {
                return false;
            };
            let Some(args) = args
                .iter()
                .map(serde_json::Value::as_str)
                .map(|arg| arg.map(str::to_owned))
                .collect::<Option<Vec<_>>>()
            else {
                return false;
            };
            validate_shell_command_hard_denials(program, &args).is_ok()
        }
        _ => false,
    }
}

#[cfg(test)]
mod streaming_tool_dispatch_tests {
    use std::{fs, path::PathBuf, sync::atomic::AtomicBool};

    use medusa_provider::{
        ModelResponse, ProviderCapabilities, ProviderStreamEvent, ResponseBlock, Usage,
    };
    use serde_json::json;

    use super::*;

    struct DeletingStreamingProvider {
        path: PathBuf,
    }

    impl ModelProvider for DeletingStreamingProvider {
        fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("streaming path must be used")
        }

        fn complete_streaming_cancellable(
            &self,
            _request: &ModelRequest,
            _cancel: &AtomicBool,
            sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
        ) -> MedusaResult<ModelResponse> {
            let input = json!({"path": "streamed.txt"});
            sink(ProviderStreamEvent::ResponseStarted {
                response_id: Some("stream-dispatch".to_owned()),
            })?;
            sink(ProviderStreamEvent::ToolUseReady {
                id: "read-early".to_owned(),
                name: "fs_read".to_owned(),
                input: input.clone(),
            })?;
            fs::remove_file(&self.path).expect("remove source after ready event");
            let response = ModelResponse {
                response_id: Some("stream-dispatch".to_owned()),
                stop_reason: Some("tool_use".to_owned()),
                blocks: vec![ResponseBlock::ToolUse {
                    id: "read-early".to_owned(),
                    name: "fs_read".to_owned(),
                    input,
                }],
                usage: Usage::default(),
            };
            sink(ProviderStreamEvent::Completed {
                response: response.clone(),
            })?;
            Ok(response)
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                tool_calling: true,
                streaming: true,
                ..ProviderCapabilities::default()
            }
        }
    }

    #[test]
    fn safe_tool_executes_when_ready_before_provider_completion() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let path = directory.path().join("streamed.txt");
        fs::write(&path, "before-stream-complete").expect("stream fixture");
        let engine = AgentEngine::new(DeletingStreamingProvider { path }, Config::default());
        let mut session = engine
            .create_session(directory.path(), "read streamed.txt".to_owned())
            .expect("create session");
        let mut observed = Vec::new();
        engine
            .step_with_observer(&mut session, |update| observed.push(update.clone()))
            .expect("streaming step");
        assert!(observed.iter().any(|update| matches!(
            update,
            AgentUpdate::ToolOutput { tool, output, is_error: false }
                if tool == "fs_read" && output.contains("before-stream-complete")
        )));
        let receipt_index = session.events.iter().position(|event| {
            matches!(
                &event.payload,
                EventPayload::WorkerEvidenceRecorded { evidence }
                    if evidence["kind"] == serde_json::json!("certified_tool_execution")
            )
        });
        let completed_index = session
            .events
            .iter()
            .position(|event| matches!(event.payload, EventPayload::ToolExecutionCompleted { .. }));
        assert!(
            receipt_index.is_some_and(|receipt| completed_index.is_some_and(|done| receipt < done))
        );
    }
}

#[cfg(test)]
mod phase_budget_tests {
    use std::sync::{Arc, Mutex};

    use medusa_provider::{ModelResponse, Usage};

    use super::*;

    struct PhaseRecordingProvider {
        phases: Arc<Mutex<Vec<(ProviderExecutionPhase, u32)>>>,
    }

    impl ModelProvider for PhaseRecordingProvider {
        fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("phase-aware cancellable path must be used")
        }

        fn complete_cancellable_for_phase(
            &self,
            request: &ModelRequest,
            phase: ProviderExecutionPhase,
            _cancel: &AtomicBool,
        ) -> MedusaResult<ModelResponse> {
            self.phases
                .lock()
                .expect("phase lock")
                .push((phase, request.max_tokens));
            Ok(ModelResponse {
                response_id: Some("phase-budget".to_owned()),
                stop_reason: Some("stop".to_owned()),
                blocks: Vec::new(),
                usage: Usage::default(),
            })
        }
    }

    #[test]
    fn phase_output_budgets_are_bounded_and_distinct() {
        let configured = 32_768;
        assert_eq!(
            phase_output_token_budget(ProviderExecutionPhase::Implementation, configured),
            configured
        );
        assert_eq!(
            phase_output_token_budget(ProviderExecutionPhase::Repair, configured),
            16_384
        );
        assert_eq!(
            phase_output_token_budget(ProviderExecutionPhase::Planning, configured),
            8_192
        );
        assert_eq!(
            phase_output_token_budget(ProviderExecutionPhase::HighRiskReview, configured),
            8_192
        );
        assert_eq!(
            phase_output_token_budget(ProviderExecutionPhase::Summarization, configured),
            4_096
        );
        assert_eq!(
            phase_output_token_budget(ProviderExecutionPhase::Formatting, configured),
            4_096
        );
        assert_eq!(
            phase_output_token_budget(ProviderExecutionPhase::Planning, 2_048),
            2_048
        );
        assert_eq!(
            phase_output_token_budget(ProviderExecutionPhase::HighRiskReview, 4_096),
            2_048
        );
        assert_eq!(
            phase_output_token_budget(ProviderExecutionPhase::Planning, 1_024),
            1_024
        );
    }

    #[test]
    fn repair_phase_and_budget_reach_provider_entrypoint() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let phases = Arc::new(Mutex::new(Vec::new()));
        let engine = AgentEngine::new(
            PhaseRecordingProvider {
                phases: Arc::clone(&phases),
            },
            Config::default(),
        );
        let mut session = engine
            .create_session(directory.path(), "repair failed verification".to_owned())
            .expect("create session");

        engine
            .step_for_provider_phase(&mut session, ProviderExecutionPhase::Repair)
            .expect("repair step");

        assert_eq!(
            *phases.lock().expect("phase lock"),
            vec![(
                ProviderExecutionPhase::Repair,
                phase_output_token_budget(
                    ProviderExecutionPhase::Repair,
                    Config::default().model.max_output_tokens,
                ),
            )]
        );
    }
}

#[cfg(test)]
mod terminal_stop_reason_tests {
    use std::{collections::VecDeque, sync::Mutex};

    use medusa_provider::{ModelResponse, ResponseBlock, Usage};

    use super::*;

    struct ScriptedStopProvider {
        responses: Mutex<VecDeque<ModelResponse>>,
    }

    impl ScriptedStopProvider {
        fn new(stop_reason: Option<&str>) -> Self {
            Self {
                responses: Mutex::new(
                    [ModelResponse {
                        response_id: Some("stop-reason-fixture".to_owned()),
                        stop_reason: stop_reason.map(str::to_owned),
                        blocks: vec![ResponseBlock::Text {
                            text: "Evidence-backed delegated report complete.".to_owned(),
                        }],
                        usage: Usage::default(),
                    }]
                    .into(),
                ),
            }
        }
    }

    impl ModelProvider for ScriptedStopProvider {
        fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
            self.responses
                .lock()
                .expect("scripted stop provider lock")
                .pop_front()
                .ok_or_else(|| {
                    MedusaError::new(
                        ErrorCode::DependencyUnavailable,
                        ErrorCategory::Internal,
                        "scripted stop response exhausted",
                    )
                })
        }
    }

    fn read_only_step(stop_reason: Option<&str>) -> StepOutcome {
        let directory = tempfile::tempdir().expect("temporary repository");
        let mut config = Config::default();
        config.agent.mode = Mode::ReadOnly;
        let engine = AgentEngine::new(ScriptedStopProvider::new(stop_reason), config);
        let mut session = engine
            .create_session(directory.path(), "inspect the repository".to_owned())
            .expect("create delegated session");
        engine.step(&mut session).expect("run delegated step")
    }

    #[test]
    fn openai_stop_completes_a_read_only_turn() {
        assert_eq!(read_only_step(Some("stop")), StepOutcome::TurnComplete);
    }

    #[test]
    fn anthropic_end_turn_still_completes_a_read_only_turn() {
        assert_eq!(read_only_step(Some("end_turn")), StepOutcome::TurnComplete);
    }

    #[test]
    fn truncated_provider_output_does_not_complete_the_turn() {
        assert_eq!(read_only_step(Some("length")), StepOutcome::Continue);
    }

    #[test]
    fn text_without_a_finish_reason_completes_a_read_only_turn() {
        assert_eq!(read_only_step(None), StepOutcome::TurnComplete);
    }
}
