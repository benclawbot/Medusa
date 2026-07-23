use std::{collections::VecDeque, path::Path, sync::Mutex, thread};

use medusa_config::{Config, Mode};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use medusa_extensions::{DesktopCommanderClient, DesktopCommanderSettings};
use medusa_protocol::{Actor, EventPayload};
use medusa_provider::{Message, MessageBlock, ModelProvider, ModelRequest, ResponseBlock, Role};
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
    tools::{execute_approved_tool, execute_tool, input_string},
    verification::targeted_verification_for_paths,
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

fn parallel_safe_tool(name: &str) -> bool {
    matches!(
        name,
        "fs_read" | "search_text" | "skill_read" | "web_search" | "web_fetch"
    )
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
}

fn audited_tool_name(name: &str, input: &serde_json::Value) -> String {
    if name == "desktop_commander" {
        if let Some(tool) = input.get("tool").and_then(serde_json::Value::as_str) {
            return format!("desktop_commander:{tool}");
        }
    }
    name.to_owned()
}

impl<P: ModelProvider> AgentEngine<P> {
    #[must_use]
    pub fn new(provider: P, config: Config) -> Self {
        Self {
            provider,
            config,
            desktop_commander_settings: DesktopCommanderSettings::from_env(),
            desktop_commander: Mutex::new(None),
        }
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
            plan: Vec::new(),
            pending_question: None,
            messages: vec![Message {
                role: Role::User,
                content,
            }],
            events: Vec::new(),
            evidence: Vec::new(),
            tool_artifacts: Vec::new(),
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
        load(repo, session)
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
            session.approval_receipts.push(ApprovalReceipt {
                decision: decision.clone(),
                scope: approval.grant.scope.clone(),
                recorded_at: now,
                reason: if approved {
                    "user approved exact action".to_owned()
                } else {
                    format!("user denied action: {answer}")
                },
            });
            let (content, is_error) = if decision == ApprovalDecision::Approved {
                session.approval_grants.push(approval.grant);
                match execute_approved_tool(&session.repo, &approval.tool, &approval.input) {
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

    pub fn run_to_completion(&self, session: &mut AgentSession) -> MedusaResult<()> {
        while !session.completed && session.turn < self.config.agent.max_turns {
            match self.step(session)? {
                StepOutcome::WaitingForUser => {
                    return Err(MedusaError::new(
                        ErrorCode::DependencyUnavailable,
                        ErrorCategory::Execution,
                        "agent is waiting for a user response",
                    ));
                }
                StepOutcome::TurnComplete => return Ok(()),
                StepOutcome::Continue | StepOutcome::Completed => {}
            }
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
        self.step_with_observer(session, |_| {})
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
        let response = self.provider.complete(&ModelRequest {
            system: system_prompt_with_context(
                self.config.agent.mode,
                &session.repo,
                additional_system_context,
            ),
            messages: session.messages.clone(),
            tools: available_tools(self.config.agent.mode, &self.desktop_commander_settings),
            max_tokens: self.config.model.max_output_tokens,
            temperature_milli: self.config.model.temperature_milli,
        })?;
        append_observed(
            session,
            EventPayload::ModelResponseReceived {
                response_id: response.response_id.clone(),
                usage: serde_json::to_value(response.usage).map_err(json_error)?,
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
        let fallback_question = calls
            .is_empty()
            .then(|| question_from_assistant_text(&assistant_text.join("\n")))
            .flatten();
        if fallback_question.is_none() && !assistant_text.is_empty() {
            observer(&AgentUpdate::AssistantText(assistant_text.join("\n")));
        }

        if let Some(question) = fallback_question {
            pause_for_question(session, question, &mut observer)?;
            return Ok(StepOutcome::WaitingForUser);
        }

        while !calls.is_empty() {
            let parallel_count = calls
                .iter()
                .take(MAX_PARALLEL_TOOL_CALLS)
                .take_while(|(_, name, _)| {
                    parallel_safe_tool(name) && tool_allowed(self.config.agent.mode, name)
                })
                .count();
            let batch_len = parallel_count.max(1);
            let batch = calls.drain(..batch_len).collect::<Vec<_>>();
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
                let repo = session.repo.as_path();
                map_parallel_ordered(batch, |(id, name, input)| {
                    let result = execute_tool(repo, &name, &input);
                    (id, name, input, result)
                })?
            } else {
                let (id, name, input) = batch.into_iter().next().ok_or_else(|| {
                    MedusaError::new(
                        ErrorCode::InternalInvariant,
                        ErrorCategory::Execution,
                        "tool batch was unexpectedly empty",
                    )
                })?;
                let result = if name == "update_plan" {
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
                        observer(&AgentUpdate::Plan(plan));
                        Ok("Visible task plan updated.".to_owned())
                    }
                } else if name == "ask_user_question" {
                    match question_from_input(id.clone(), &input) {
                        Ok(question) => {
                            pause_for_question(session, question, &mut observer)?;
                            return Ok(StepOutcome::WaitingForUser);
                        }
                        Err(error) => Err(error),
                    }
                } else if name == "desktop_commander" && tool_allowed(self.config.agent.mode, &name)
                {
                    self.execute_desktop_commander(&session.repo, &input)
                } else if tool_allowed(self.config.agent.mode, &name) {
                    execute_tool(&session.repo, &name, &input)
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
                vec![(id, name, input, result)]
            };

            for (id, name, input, result) in executed {
                if let Err(error) = &result
                    && error.code == ErrorCode::PolicyDenied
                    && self.config.agent.mode != Mode::ReadOnly
                    && interactively_approvable(&name, &input)
                {
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
                append_observed(
                    session,
                    EventPayload::ToolExecutionCompleted {
                        tool: event_tool,
                        exit_code,
                    },
                    &mut observer,
                )?;
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

        if response.stop_reason.as_deref() == Some("end_turn")
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
            let verification = targeted_verification_for_paths(
                &session.repo,
                &successful_mutation_paths(session),
            )?;
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
        } else if response.stop_reason.as_deref() == Some("end_turn") {
            StepOutcome::TurnComplete
        } else {
            StepOutcome::Continue
        })
    }
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
