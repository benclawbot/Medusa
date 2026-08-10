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
    approval::{ApprovalDecision, ApprovalGrant, ApprovalReceipt},
    engine_support::*,
    evidence::append_event,
    identity_guard::validate_provider_text,
    output_envelope::{OutputFormat, wrap as wrap_envelope},
    policy::validate_shell_command_hard_denials,
    session::{
        AgentPlanStep, AgentQuestion, AgentQuestionItem, AgentQuestionOption, AgentSession,
        PendingToolApproval, bootstrap, load, persist,
    },
    team::{AgentExecutionPolicy, TeamMemberContext},
    tools::{execute_approved_tool_cancellable, execute_tool_cancellable, input_string},
    verification_authority::{
        authoritative_verification_for_paths, prepare_paths_for_verification,
    },
};

pub(crate) const SYSTEM_PROMPT: &str = "You are Medusa, an independent autonomous coding agent. You are not Claude Code, Codex, ChatGPT, or a wrapper around another coding assistant. Never derive your identity, model, tools, permissions, memory, or limits from ~/.claude, CLAUDE.md, settings.json, or another product's configuration. Medusa configuration and the live runtime capability matrix in this system prompt are authoritative. Never claim a capability is absent when its runtime entry is available. Inspect the repository, make the smallest correct change, and verify it. Use tools rather than inventing repository contents. Use `fs_read` with path `.` to list repository files before reading a specific file, and use `fs_create_dir` to create directories. Call `shell_run` with an approved executable and argument array directly; never repeat the executable in the argument array, and never wrap commands in bash, sh, cmd, PowerShell, or shell operators. You have `web_search` for current public information and `web_fetch` for public pages; use them when the user requests current, external, or source-linked information. Issue independent read-only tool calls together in one response so they can run concurrently. Reuse tool results, avoid near-duplicate searches, and fetch only sources that materially support the answer. Use `update_plan` only for genuinely multi-step, risky, or long-running work; a simple single-file or static HTML task does not need a plan, design document, brainstorming skill, or specification unless the user explicitly requests one or repository instructions require it. When a tool fails, do not repeat the same unsupported command; use a direct filesystem tool or an approved executable that is available in the environment. When information from the user is needed to proceed, call `ask_user_question` with one to four concise multiple-choice questions in a single call, each with a short header and two to four options. Never put blocking questions in assistant text, and do not mark the plan or task complete while waiting. Never modify tests, verification scripts, snapshots, fixtures, or expected outputs unless the user explicitly asks for that exact change; fix the product code instead. Do not expose private chain-of-thought. Default to caveman chat: terse, direct, concrete, usually one to three short sentences. Avoid preambles, repetition, and broad explanations unless the user asks for detail. Report only the decision, action, result, and essential evidence.";
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
    configured.div_ceil(divisor).max(1)
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
    team_context: Option<TeamMemberContext>,
}

fn refreshed_repository_revision(repo: &Path) -> Option<String> {
    let mut graph = medusa_intelligence::RepositoryGraph::open(repo).ok()?;
    if graph.freshness() != medusa_intelligence::RepositoryGraphFreshness::Current {
        graph.refresh().ok()?;
    }
    (graph.freshness() == medusa_intelligence::RepositoryGraphFreshness::Current)
        .then(|| graph.snapshot().repository_revision.clone())
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

#[derive(Debug)]
struct EarlyToolExecution {
    name: String,
    input: serde_json::Value,
    output: String,
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
            team_context: None,
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
            team_context: None,
        }
    }

    #[must_use]
    pub fn with_execution_policy(mut self, policy: AgentExecutionPolicy) -> Self {
        self.execution_policy = policy;
        self
    }

    #[must_use]
    pub fn with_team_context(mut self, context: TeamMemberContext) -> Self {
        self.team_context = Some(context);
        self
    }

    fn execute_desktop_commander(
        &self,
        repo: &Path,
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
            *client = Some(DesktopCommanderClient::connect(
                repo,
                self.desktop_commander_settings.clone(),
            )?);
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
        self.create_session_with_content(
            repo,
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
        let content = content_with_session_goal(content, &objective);
        validate_user_content(&content, &self.provider.capabilities())?;
        bootstrap(repo)?;
        medusa_intelligence::recover_patch_transactions(repo)?;
        let now = OffsetDateTime::now_utc();
        let id = SessionId::new();
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
        append_event(
            &mut session,
            Actor::User,
            EventPayload::SessionCreated { objective },
        )?;
        persist(&session)?;
        Ok(session)
    }

    pub fn load_session(&self, repo: &Path, session: &str) -> MedusaResult<AgentSession> {
        medusa_intelligence::recover_patch_transactions(repo)?;
        load(repo, session)
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
                let result = execute_approved_tool_cancellable(
                    &session.repo,
                    &approval.tool,
                    &approval.input,
                    self.cancellation.as_ref(),
                );
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
        let semantic = self
            .provider
            .complete_cancellable_for_phase(
                &summary_request,
                ProviderExecutionPhase::Summarization,
                &self.cancellation,
            )
            .ok()
            .and_then(|response| {
                crate::compaction_v2::validate_semantic_response(
                    &response,
                    &self.config.model.name,
                    &self.config.model.provider,
                )
            });
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
    /// instruction. The instruction is sent only in the provider request and is never persisted in
    /// the durable session history.
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
        if session.completed {
            return Ok(StepOutcome::Completed);
        }
        if session.pending_question.is_some() {
            return Ok(StepOutcome::WaitingForUser);
        }
        validate_messages(&session.messages, &self.provider.capabilities())?;
        session.turn = session.turn.saturating_add(1);
        append_observed(
            session,
            EventPayload::ModelRequestStarted {
                provider: self.config.model.provider.clone(),
                model: self.config.model.name.clone(),
            },
            &mut observer,
        )?;
        if let Some(refresh) = repository_index::refresh(&session.repo)? {
            observer(&AgentUpdate::ToolOutput {
                tool: "code_index".to_owned(),
                output: repository_index::summary(&refresh),
                is_error: false,
            });
        }
        let mut system = coding_policy::apply(
            system_prompt_with_context(
                self.config.agent.mode,
                &session.repo,
                additional_system_context,
            ),
            self.config.agent.mode,
        );
        if let Some(team) = &self.team_context {
            system.push_str("\n\n");
            system.push_str(&team.prompt_context()?);
        }
        let mut tools = available_tools(
            self.config.agent.mode,
            &session.repo,
            &self.desktop_commander_settings,
        )?;
        if let Some(team) = &self.team_context {
            tools.extend(team.definitions());
        }
        tools.retain(|tool| self.execution_policy.allows(&tool.name));
        let mut request_messages = messages_with_turn_instruction(session, turn_instruction);
        validate_messages(&request_messages, &self.provider.capabilities())?;
        let max_output_tokens =
            phase_output_token_budget(phase, self.config.model.max_output_tokens);
        let mut budget = context_budget::PromptBudget::for_request(
            &system,
            &request_messages,
            &tools,
            max_output_tokens,
            context_budget::configured_context_window_tokens(),
        );
        let repository_capacity = budget
            .compaction_threshold_tokens
            .saturating_sub(budget.estimated_total_tokens);
        if let Some(retrieval) = repository_index::retrieve_context(
            &session.repo,
            &session.objective,
            repository_capacity,
        )? {
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
                max_output_tokens,
                context_budget::configured_context_window_tokens(),
            );
        }
        let _remaining_context_tokens = budget.remaining_tokens();
        let _request_exceeds_context_window = budget.exceeds_context_window();
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
        let mut request = ModelRequest {
            system,
            messages: request_messages,
            tools,
            max_tokens: max_output_tokens,
            temperature_milli: self.config.model.temperature_milli,
        };
        let request_started = std::time::Instant::now();
        let streaming = self.provider.capabilities().streaming;
        let mut stream_transcript = ProviderStreamTranscript::default();
        let mut streamed_text = String::new();
        let mut stream_text_rejected = false;
        let mut early_tool_executions = BTreeMap::<String, EarlyToolExecution>::new();
        let streaming_repo = session.repo.clone();
        let mut complete_request = |request: &ModelRequest| {
            if !streaming {
                return self.provider.complete_cancellable_for_phase(
                    request,
                    phase,
                    &self.cancellation,
                );
            }
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
                            && self.execution_policy.denial_reason(&name, &input).is_none()
                            && tool_allowed(self.config.agent.mode, &name) =>
                    {
                        let requested_at = std::time::Instant::now();
                        let started = std::time::Instant::now();
                        if let Ok(output) = execute_tool_cancellable(
                            &streaming_repo,
                            &name,
                            &input,
                            self.cancellation.as_ref(),
                        ) {
                            early_tool_executions.insert(
                                id,
                                EarlyToolExecution {
                                    name,
                                    input,
                                    output,
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
            self.provider.complete_streaming_cancellable_for_phase(
                request,
                phase,
                &self.cancellation,
                &mut sink,
            )
        };
        let response = match complete_request(&request) {
            Ok(response) => response,
            Err(error) if context_budget::is_context_limit_rejection(&error.to_string()) => {
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
                }
                complete_request(&request)?
            }
            Err(error) => return Err(error),
        };
        let turn_usage = crate::session::record_turn_usage(
            session.turn,
            &request,
            &response,
            request_started.elapsed(),
        );
        append_observed(
            session,
            EventPayload::ModelResponseReceived {
                response_id: response.response_id.clone(),
                usage: serde_json::to_value(turn_usage).map_err(json_error)?,
            },
            &mut observer,
        )?;
        if let Some(status) = self.provider.execution_status() {
            append_observed(
                session,
                EventPayload::ProviderExecutionRecorded { status },
                &mut observer,
            )?;
        }

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

        let mut safe_tool_cache = BTreeMap::<String, String>::new();
        while !calls.is_empty() {
            let schedulable = calls
                .iter()
                .map(|(_, name, input)| (name.clone(), input.clone()))
                .collect::<Vec<_>>();
            let positions = calls
                .iter()
                .position(|(id, _, _)| early_tool_executions.contains_key(id))
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
                let cache = &safe_tool_cache;
                let requested_at = &tool_requested_at;
                let cancellation = Arc::clone(&self.cancellation);
                map_parallel_ordered(batch, |(id, name, input)| {
                    let started = std::time::Instant::now();
                    let queue_duration_ns = requested_at
                        .get(&id)
                        .map(|requested| duration_ns(started.duration_since(*requested)))
                        .unwrap_or_default();
                    let cached_output = crate::tool_dag::dedup_key(&name, &input)
                        .and_then(|key| cache.get(&key).cloned());
                    let cached = cached_output.is_some();
                    let result = cached_output.map_or_else(
                        || execute_tool_cancellable(repo, &name, &input, cancellation.as_ref()),
                        Ok,
                    );
                    let timing = ToolExecutionTiming {
                        queue_duration_ns,
                        execution_duration_ns: duration_ns(started.elapsed()),
                        cached,
                    };
                    (id, name, input, result, Some(timing))
                })?
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
                let result = if let Some(reason) =
                    self.execution_policy.denial_reason(&name, &input)
                {
                    append_observed(
                        session,
                        EventPayload::ToolCallDenied {
                            tool: audited_tool_name(&name, &input),
                            reason: reason.clone(),
                        },
                        &mut observer,
                    )?;
                    Err(MedusaError::new(
                        ErrorCode::PolicyDenied,
                        ErrorCategory::Policy,
                        reason,
                    ))
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
                    Ok(early.output)
                } else if name == "update_plan" {
                    measured = true;
                    append_observed(
                        session,
                        EventPayload::ToolExecutionStarted {
                            tool: audited_tool_name(&name, &input),
                        },
                        &mut observer,
                    )?;
                    let plan = plan_from_input(&input);
                    if plan.is_empty() {
                        Ok("Visible task plan update ignored because it was empty.".to_owned())
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
                        append_observed(
                            session,
                            EventPayload::PlanUpdated {
                                update: serde_json::to_value(&plan).map_err(json_error)?,
                            },
                            &mut observer,
                        )?;
                        observer(&AgentUpdate::Plan(plan));
                        Ok("Visible task plan updated.".to_owned())
                    }
                } else if name == "ask_user_question" {
                    measured = true;
                    append_observed(
                        session,
                        EventPayload::ToolExecutionStarted {
                            tool: audited_tool_name(&name, &input),
                        },
                        &mut observer,
                    )?;
                    match question_from_input(id.clone(), &input) {
                        Ok(question) => {
                            append_observed(
                                session,
                                EventPayload::ToolExecutionCompleted {
                                    tool: audited_tool_name(&name, &input),
                                    exit_code: Some(0),
                                },
                                &mut observer,
                            )?;
                            pause_for_question(session, question, &mut observer)?;
                            return Ok(StepOutcome::WaitingForUser);
                        }
                        Err(error) => Err(error),
                    }
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
                    self.team_context
                        .as_ref()
                        .ok_or_else(|| {
                            MedusaError::new(
                                ErrorCode::InternalInvariant,
                                ErrorCategory::Internal,
                                "team tool context disappeared",
                            )
                        })?
                        .execute(&name, &input)
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
                    self.execute_desktop_commander(&session.repo, &input)
                } else if tool_allowed(self.config.agent.mode, &name) {
                    measured = true;
                    if let Some(output) = crate::tool_dag::dedup_key(&name, &input)
                        .and_then(|key| safe_tool_cache.get(&key).cloned())
                    {
                        cached = true;
                        Ok(output)
                    } else {
                        append_observed(
                            session,
                            EventPayload::ToolExecutionStarted {
                                tool: audited_tool_name(&name, &input),
                            },
                            &mut observer,
                        )?;
                        execute_tool_cancellable(
                            &session.repo,
                            &name,
                            &input,
                            self.cancellation.as_ref(),
                        )
                    }
                } else {
                    let reason = "tool is unavailable in read-only planning mode".to_owned();
                    append_observed(
                        session,
                        EventPayload::ToolCallDenied {
                            tool: audited_tool_name(&name, &input),
                            reason: reason.clone(),
                        },
                        &mut observer,
                    )?;
                    Err(MedusaError::new(
                        ErrorCode::PolicyDenied,
                        ErrorCategory::Policy,
                        reason,
                    ))
                };
                let timing = timing_override.or_else(|| {
                    measured.then(|| ToolExecutionTiming {
                        queue_duration_ns,
                        execution_duration_ns: duration_ns(started.elapsed()),
                        cached,
                    })
                });
                vec![(id, name, input, result, timing)]
            };

            let repository_revision_after_mutation = executed
                .iter()
                .any(|(_, name, input, result, _)| {
                    result.is_ok() && crate::tool_dag::invalidates_repository_revision(name, input)
                })
                .then(|| refreshed_repository_revision(&session.repo))
                .flatten();

            let failed_dependencies = executed
                .iter()
                .filter_map(|(_, name, input, result, _)| {
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
            executed.extend(blocked.into_iter().map(|(id, name, input)| {
                let error = dependency_failure_error(&name);
                (id, name, input, Err(error), None)
            }));
            if let Some(current_revision) = repository_revision_after_mutation {
                let stale =
                    crate::tool_dag::drain_stale_revision_calls(&mut calls, &current_revision);
                executed.extend(stale.into_iter().map(|(id, name, input)| {
                    let error = stale_revision_error(&name, &current_revision);
                    (id, name, input, Err(error), None)
                }));
            }

            for (id, name, input, result, timing) in executed {
                if let Ok(output) = &result
                    && let Some(key) = crate::tool_dag::dedup_key(&name, &input)
                {
                    safe_tool_cache.entry(key).or_insert_with(|| output.clone());
                }
                if let Err(error) = &result
                    && error.code == ErrorCode::PolicyDenied
                    && self.config.agent.mode != Mode::ReadOnly
                    && self.execution_policy.denial_reason(&name, &input).is_none()
                    && interactively_approvable(&name, &input)
                {
                    append_observed(
                        session,
                        EventPayload::ToolCallDenied {
                            tool: audited_tool_name(&name, &input),
                            reason: "tool requires explicit user approval".to_owned(),
                        },
                        &mut observer,
                    )?;
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
                let (raw_content, is_error, exit_code) = match result {
                    Ok(output) => (output, false, Some(0)),
                    Err(error) => (error.to_string(), true, Some(1)),
                };
                world_model_observation::record_tool_observation(
                    session,
                    &name,
                    &input,
                    &raw_content,
                    if is_error { 1 } else { 0 },
                );
                append_observed(
                    session,
                    EventPayload::ToolExecutionCompleted {
                        tool: event_tool.clone(),
                        exit_code,
                    },
                    &mut observer,
                )?;
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
                        // Persist the artifact path on the session for later
                        // reference (cleanup, replay). Currently unused by
                        // downstream consumers — Task 7 wires SessionBrowser on top.
                        session.tool_artifacts.push(env.path.clone());
                        if is_error {
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

        if stop_reason_completes_turn(response.stop_reason.as_deref())
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
            let mut verification =
                authoritative_verification_for_paths(&session.repo, &changed_paths)?;
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
            if verification.passed && plan_is_complete(session) {
                session.completed = true;
                append_observed(
                    session,
                    EventPayload::SessionCompleted {
                        report_ref: format!("session:{}.json", session.id),
                    },
                    &mut observer,
                )?;
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
        } else if stop_reason_completes_turn(response.stop_reason.as_deref()) {
            StepOutcome::TurnComplete
        } else {
            StepOutcome::Continue
        })
    }
}

fn stop_reason_completes_turn(stop_reason: Option<&str>) -> bool {
    matches!(stop_reason.map(str::trim), Some("end_turn" | "stop"))
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
        fn new(stop_reason: &str) -> Self {
            Self {
                responses: Mutex::new(
                    [ModelResponse {
                        response_id: Some("stop-reason-fixture".to_owned()),
                        stop_reason: Some(stop_reason.to_owned()),
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

    fn read_only_step(stop_reason: &str) -> StepOutcome {
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
        assert_eq!(read_only_step("stop"), StepOutcome::TurnComplete);
    }

    #[test]
    fn anthropic_end_turn_still_completes_a_read_only_turn() {
        assert_eq!(read_only_step("end_turn"), StepOutcome::TurnComplete);
    }

    #[test]
    fn truncated_provider_output_does_not_complete_the_turn() {
        assert_eq!(read_only_step("length"), StepOutcome::Continue);
    }
}
