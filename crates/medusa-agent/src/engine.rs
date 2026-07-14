use std::{collections::VecDeque, fs, path::Path};

use medusa_config::{Config, Mode};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use medusa_protocol::{Actor, EventPayload};
use medusa_provider::{
    ImageSource, Message, MessageBlock, ModelProvider, ModelRequest, ProviderCapabilities,
    ResponseBlock, Role,
};
use time::OffsetDateTime;

use crate::{
    evidence::append_event,
    session::{
        AgentPlanStep, AgentPlanStepStatus, AgentQuestion, AgentQuestionItem, AgentQuestionOption,
        AgentSession, bootstrap, load, persist,
    },
    skill_injector,
    skill_loader::SkillBundle,
    tools::{available_skills, built_in_tools, execute_tool},
    verification::targeted_verification,
};

const SYSTEM_PROMPT: &str = "You are Medusa, an autonomous coding agent. Inspect the repository, make the smallest correct change, and verify it. Use tools rather than inventing repository contents. Use `fs_read` with path `.` to list repository files before reading a specific file, and use `fs_create_dir` to create directories. Call `shell_run` with an approved executable and argument array directly; never wrap commands in bash, sh, cmd, PowerShell, or shell operators. You have `web_search` for current public information and `web_fetch` for public pages; use them when the user requests current, external, or source-linked information. For work requiring more than one action, call `update_plan` before meaningful work and update its statuses as you progress. When information from the user is needed to proceed, call `ask_user_question` with one to four concise multiple-choice questions in a single call, each with a short header and two to four options. Never put blocking questions in assistant text, and do not mark the plan or task complete while waiting. Never modify tests, verification scripts, snapshots, fixtures, or expected outputs unless the user explicitly asks for that exact change; fix the product code instead. Do not expose private chain-of-thought; provide concise decisions and evidence.";
const PLAN_SYSTEM_PROMPT: &str = "You are Medusa in read-only planning mode. Inspect the repository and produce a concise, ordered implementation plan grounded in the files you examined. Use `update_plan` to maintain the visible plan as your understanding changes. When clarification is necessary, call `ask_user_question` with one to four concise multiple-choice questions in a single call, each with a short header and two to four options, then wait for its answer before producing a final plan. You can use `web_search` and `web_fetch` for current public information. Do not modify files, create commits, or claim that implementation work has been completed. Only read-only repository and web tools are available. Do not expose private chain-of-thought; provide concise decisions and evidence.";
const MAX_REPOSITORY_INSTRUCTIONS_BYTES: usize = 32_000;

/// Result of one durable model/tool step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepOutcome {
    Continue,
    TurnComplete,
    WaitingForUser,
    Completed,
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
}

impl<P: ModelProvider> AgentEngine<P> {
    #[must_use]
    pub fn new(provider: P, config: Config) -> Self {
        Self { provider, config }
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
        let content = match question.tool_use_id {
            Some(tool_use_id) => vec![MessageBlock::ToolResult {
                tool_use_id,
                content: format!("User response: {answer}"),
                is_error: false,
            }],
            None => vec![MessageBlock::Text {
                text: format!("User response to the clarification question: {answer}"),
            }],
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
            system: system_prompt(self.config.agent.mode, &session.repo),
            messages: session.messages.clone(),
            tools: available_tools(self.config.agent.mode),
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

        let mut assistant_blocks = Vec::new();
        let mut assistant_text = Vec::new();
        let mut calls = VecDeque::new();
        for block in response.blocks {
            match block {
                ResponseBlock::Text { text } => {
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

        while let Some((id, name, input)) = calls.pop_front() {
            append_observed(
                session,
                EventPayload::ToolCallRequested {
                    tool: name.clone(),
                    arguments: input.clone(),
                },
                &mut observer,
            )?;
            let result = if name == "update_plan" {
                let plan = plan_from_input(&input);
                if plan.is_empty() {
                    Ok("Visible task plan update ignored because it was empty.".to_owned())
                } else {
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
            } else if tool_allowed(self.config.agent.mode, &name) {
                execute_tool(&session.repo, &name, &input)
            } else {
                let reason = "tool is unavailable in read-only planning mode".to_owned();
                append_observed(
                    session,
                    EventPayload::ToolCallDenied {
                        tool: name.clone(),
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
            let (content, is_error, exit_code) = match result {
                Ok(output) => (output, false, Some(0)),
                Err(error) => (error.to_string(), true, Some(1)),
            };
            append_observed(
                session,
                EventPayload::ToolExecutionCompleted {
                    tool: name.clone(),
                    exit_code,
                },
                &mut observer,
            )?;
            observer(&AgentUpdate::ToolOutput {
                tool: name,
                output: content.clone(),
                is_error,
            });
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
            let verification = targeted_verification(&session.repo)?;
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

fn content_with_session_goal(mut content: Vec<MessageBlock>, objective: &str) -> Vec<MessageBlock> {
    content.insert(
        0,
        MessageBlock::Text {
            text: format!("Current session goal: {objective}"),
        },
    );
    content
}

fn system_prompt(mode: Mode, repo: &Path) -> String {
    let base = if mode == Mode::ReadOnly {
        PLAN_SYSTEM_PROMPT
    } else {
        SYSTEM_PROMPT
    };
    let mut prompt = format!("{base}\n\nWorkspace: {}", repo.display());
    let instructions = repository_instructions(repo);
    if instructions.is_empty() {
        prompt.push_str("\n\nNo repository instruction files were found.");
    } else {
        prompt.push_str("\n\nRepository instructions (follow them as project constraints; the user request and system rules take precedence):\n");
        prompt.push_str(&instructions);
    }
    let skills = available_skills(repo);
    if skills.is_empty() {
        prompt.push_str("\n\nNo Medusa or Claude skills are installed for this workspace or user.");
    } else {
        prompt.push_str(
            "\n\nAvailable skills: call `skill_read` before applying a relevant skill.\n",
        );
        for skill in skills {
            prompt.push_str(&format!(
                "- {} ({}){}\n",
                skill.name,
                skill.scope,
                skill
                    .description
                    .map(|description| format!(": {description}"))
                    .unwrap_or_default()
            ));
        }
    }
    prompt
}

fn available_tools(mode: Mode) -> Vec<medusa_provider::ToolDefinition> {
    built_in_tools()
        .into_iter()
        .filter(|tool| tool_allowed(mode, &tool.name))
        .collect()
}

fn tool_allowed(mode: Mode, tool: &str) -> bool {
    mode != Mode::ReadOnly
        || matches!(
            tool,
            "fs_read"
                | "search_text"
                | "code_index"
                | "web_search"
                | "web_fetch"
                | "skill_read"
                | "update_plan"
                | "ask_user_question"
        )
}

fn question_from_input(
    tool_use_id: String,
    input: &serde_json::Value,
) -> MedusaResult<AgentQuestion> {
    let questions = input
        .get("questions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid_question("questions must be an array"))?;
    if questions.is_empty() || questions.len() > 4 {
        return Err(invalid_question(
            "ask_user_question accepts between 1 and 4 questions",
        ));
    }
    let questions = questions
        .iter()
        .map(question_item_from_input)
        .collect::<MedusaResult<Vec<_>>>()?;
    Ok(AgentQuestion {
        tool_use_id: Some(tool_use_id),
        questions,
        legacy_question: None,
        legacy_options: Vec::new(),
    })
}

fn question_item_from_input(input: &serde_json::Value) -> MedusaResult<AgentQuestionItem> {
    let header = input
        .get("header")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_question("every question header must not be empty"))?;
    let header = compact_question_header(header);
    let question = input
        .get("question")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.chars().count() <= 500)
        .ok_or_else(|| invalid_question("every question must be 1 to 500 characters"))?;
    let options = input
        .get("options")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid_question("every question needs an options array"))?;
    if !(2..=4).contains(&options.len()) {
        return Err(invalid_question(
            "every question needs between 2 and 4 options",
        ));
    }
    let options = options
        .iter()
        .map(|option| {
            let label = option
                .get("label")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty() && value.chars().count() <= 80)
                .ok_or_else(|| invalid_question("every option label must be 1 to 80 characters"))?;
            let description = option
                .get("description")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| value.chars().count() <= 240)
                .ok_or_else(|| {
                    invalid_question("every option description must be at most 240 characters")
                })?;
            Ok(AgentQuestionOption {
                label: label.to_owned(),
                description: description.to_owned(),
            })
        })
        .collect::<MedusaResult<Vec<_>>>()?;
    Ok(AgentQuestionItem {
        header,
        question: question.to_owned(),
        options,
        multi_select: input
            .get("multiSelect")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

fn compact_question_header(header: &str) -> String {
    const MAX_HEADER_CHARS: usize = 12;

    let normalized = header.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX_HEADER_CHARS {
        return normalized;
    }

    let mut compact = String::new();
    for word in normalized.split(' ') {
        let candidate = if compact.is_empty() {
            word.to_owned()
        } else {
            format!("{compact} {word}")
        };
        if candidate.chars().count() > MAX_HEADER_CHARS {
            break;
        }
        compact = candidate;
    }
    if compact.is_empty() {
        normalized.chars().take(MAX_HEADER_CHARS).collect()
    } else {
        compact
    }
}

fn question_from_assistant_text(text: &str) -> Option<AgentQuestion> {
    let question = text.lines().find_map(|line| {
        let line = line.trim().trim_start_matches(|character: char| {
            character.is_ascii_digit() || matches!(character, '-' | '*' | ' ' | '#' | '.' | ')')
        });
        let question_end = line.find('?')?;
        let candidate = line[..=question_end]
            .replace("**", "")
            .replace("__", "")
            .replace('`', "");
        let normalized = candidate.to_ascii_lowercase();
        let asks_for_input = [
            "which ", "what ", "where ", "when ", "who ", "why ", "how ", "do you ", "does ",
            "would ", "could ", "can you ", "should ", "please ", "provide ", "confirm ",
        ]
        .iter()
        .any(|prefix| normalized.starts_with(prefix));
        asks_for_input.then_some(candidate)
    })?;
    Some(AgentQuestion {
        tool_use_id: None,
        questions: vec![AgentQuestionItem {
            header: "Question".to_owned(),
            question,
            options: Vec::new(),
            multi_select: false,
        }],
        legacy_question: None,
        legacy_options: Vec::new(),
    })
}

fn pause_for_question<F>(
    session: &mut AgentSession,
    question: AgentQuestion,
    observer: &mut F,
) -> MedusaResult<()>
where
    F: FnMut(&AgentUpdate),
{
    session.pending_question = Some(question.clone());
    append_observed(
        session,
        EventPayload::SessionPaused {
            reason: "waiting for a user response".to_owned(),
        },
        observer,
    )?;
    observer(&AgentUpdate::Question(question));
    session.updated_at = OffsetDateTime::now_utc();
    persist(session)
}

fn invalid_question(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn plan_from_input(input: &serde_json::Value) -> Vec<AgentPlanStep> {
    let Some(steps) = input.get("steps").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    steps
        .iter()
        .take(8)
        .enumerate()
        .map(|(index, step)| {
            let title = step
                .get("title")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .map(compact_plan_title)
                .unwrap_or_else(|| format!("Step {}", index.saturating_add(1)));
            let status = match step
                .get("status")
                .and_then(serde_json::Value::as_str)
                .map(|status| status.trim().to_ascii_lowercase())
                .as_deref()
            {
                Some("pending") => AgentPlanStepStatus::Pending,
                Some("in_progress" | "in progress") => AgentPlanStepStatus::InProgress,
                Some("completed") => AgentPlanStepStatus::Completed,
                Some("failed") => AgentPlanStepStatus::Failed,
                _ => AgentPlanStepStatus::Pending,
            };
            AgentPlanStep { title, status }
        })
        .collect()
}

fn compact_plan_title(title: &str) -> String {
    const MAX_TITLE_CHARS: usize = 140;
    title.chars().take(MAX_TITLE_CHARS).collect()
}

fn has_mutating_tool_result(session: &AgentSession) -> bool {
    session.events.iter().any(|event| {
        matches!(
            &event.payload,
            EventPayload::ToolExecutionCompleted {
                tool,
                exit_code: Some(0)
            } if matches!(tool.as_str(), "fs_create_dir" | "fs_write" | "patch_apply" | "symbol_rename" | "git_checkpoint")
        )
    })
}

fn plan_is_complete(session: &AgentSession) -> bool {
    session.plan.is_empty()
        || session
            .plan
            .iter()
            .all(|step| step.status == AgentPlanStepStatus::Completed)
}

fn repository_instructions(repo: &Path) -> String {
    let mut remaining = MAX_REPOSITORY_INSTRUCTIONS_BYTES;
    let mut output = String::new();
    for name in ["AGENTS.md", "CLAUDE.md", "MEDUSA.md", ".medusa/AGENTS.md"] {
        if remaining == 0 {
            break;
        }
        let path = repo.join(name);
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let content = truncate_for_prompt(&content, remaining);
        remaining = remaining.saturating_sub(content.len());
        output.push_str(&format!("\n--- {name} ---\n{content}\n"));
    }
    output
}

fn truncate_for_prompt(content: &str, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content.to_owned();
    }
    let mut end = max_bytes;
    while !content.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}\n[truncated]", &content[..end])
}

/// Updates a session goal without requiring a live model provider.
pub fn update_session_objective(session: &mut AgentSession, objective: String) -> MedusaResult<()> {
    session.objective = objective.clone();
    append_event(
        session,
        Actor::User,
        EventPayload::GoalUpdated { objective },
    )?;
    session.updated_at = OffsetDateTime::now_utc();
    persist(session)
}

/// Compacts durable session history without requiring a live model provider.
pub fn compact_session(session: &mut AgentSession, focus: Option<&str>) -> MedusaResult<()> {
    let original_messages = session.messages.len();
    let mut entries = session
        .messages
        .iter()
        .flat_map(|message| {
            message
                .content
                .iter()
                .map(move |block| (message.role, block))
        })
        .map(|(role, block)| {
            let speaker = match role {
                Role::User => "User",
                Role::Assistant => "Assistant",
            };
            format!("{speaker}: {}", compact_block_text(block))
        })
        .collect::<Vec<_>>();
    const MAX_ENTRIES: usize = 24;
    if entries.len() > MAX_ENTRIES {
        entries = entries.split_off(entries.len() - MAX_ENTRIES);
    }
    let focus = focus
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("\nFocus for the next turn: {value}"))
        .unwrap_or_default();
    let summary = format!(
        "This is a compacted Medusa session.\nCurrent goal: {}{}\n\nRecent durable context:\n{}",
        session.objective,
        focus,
        entries.join("\n")
    );
    session.messages = vec![Message {
        role: Role::User,
        content: vec![MessageBlock::Text { text: summary }],
    }];
    append_event(
        session,
        Actor::Coordinator,
        EventPayload::ConversationCompacted {
            original_messages: u32::try_from(original_messages).unwrap_or(u32::MAX),
            retained_messages: 1,
        },
    )?;
    session.updated_at = OffsetDateTime::now_utc();
    persist(session)
}

fn compact_message_text(content: &[MessageBlock]) -> String {
    content
        .iter()
        .map(compact_block_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn compact_block_text(block: &MessageBlock) -> String {
    const MAX_BLOCK_CHARS: usize = 600;
    let text = match block {
        MessageBlock::Text { text } => text.clone(),
        MessageBlock::Image { alt_text, .. } => {
            format!("[image: {}]", alt_text.as_deref().unwrap_or("attachment"))
        }
        MessageBlock::ToolUse { name, input, .. } => format!("used {name} with {input}"),
        MessageBlock::ToolResult {
            content, is_error, ..
        } => format!(
            "tool {}: {content}",
            if *is_error { "error" } else { "result" }
        ),
    };
    let compact = text.replace('\n', " ");
    if compact.chars().count() <= MAX_BLOCK_CHARS {
        compact
    } else {
        compact
            .chars()
            .take(MAX_BLOCK_CHARS.saturating_sub(3))
            .chain("...".chars())
            .collect()
    }
}

fn append_observed<F>(
    session: &mut AgentSession,
    payload: EventPayload,
    observer: &mut F,
) -> MedusaResult<()>
where
    F: FnMut(&AgentUpdate),
{
    append_event(session, Actor::Coordinator, payload.clone())?;
    observer(&AgentUpdate::Event(payload));
    Ok(())
}

fn validate_messages(
    messages: &[Message],
    capabilities: &ProviderCapabilities,
) -> MedusaResult<()> {
    for message in messages {
        validate_user_content(&message.content, capabilities)?;
    }
    Ok(())
}

fn validate_user_content(
    content: &[MessageBlock],
    capabilities: &ProviderCapabilities,
) -> MedusaResult<()> {
    let images = content
        .iter()
        .filter_map(|block| match block {
            MessageBlock::Image { source, .. } => Some(source),
            _ => None,
        })
        .collect::<Vec<_>>();
    if images.is_empty() {
        return Ok(());
    }
    if !capabilities.image_input {
        return Err(MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Validation,
            "the active provider cannot consume image attachments; submission was blocked",
        ));
    }
    if capabilities
        .max_images_per_request
        .is_some_and(|limit| images.len() > limit as usize)
    {
        return Err(MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Validation,
            format!(
                "prompt contains {} images, exceeding the provider limit",
                images.len()
            ),
        ));
    }
    for source in images {
        if let ImageSource::Base64 { media_type, data } = source {
            if !capabilities.supported_image_media_types.is_empty()
                && !capabilities
                    .supported_image_media_types
                    .iter()
                    .any(|supported| supported == media_type)
            {
                return Err(MedusaError::new(
                    ErrorCode::DependencyUnavailable,
                    ErrorCategory::Validation,
                    format!("provider does not support image media type {media_type}"),
                ));
            }
            if capabilities
                .max_image_bytes
                .is_some_and(|limit| estimated_base64_bytes(data) > limit)
            {
                return Err(MedusaError::new(
                    ErrorCode::DependencyUnavailable,
                    ErrorCategory::Validation,
                    "image attachment exceeds the provider byte limit",
                ));
            }
        }
    }
    Ok(())
}

fn estimated_base64_bytes(data: &str) -> u64 {
    let padding = data
        .as_bytes()
        .iter()
        .rev()
        .take_while(|byte| **byte == b'=')
        .count() as u64;
    (data.len() as u64).saturating_mul(3) / 4 - padding.min(2)
}

fn json_error(error: serde_json::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        error.to_string(),
    )
}

/// Inputs assembled by [`build_user_turn_input`]: the original user prompt
/// and a system-prompt section that lists the skills currently in scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnInput {
    pub user_prompt: String,
    pub system_prompt_section: String,
}

/// Prepares the inputs for one user turn by rendering the loaded-skill
/// banner into the system prompt section while leaving the user prompt
/// untouched. The matcher/loader pipeline that produces `bundle` is wired
/// into `AgentSession` in Task 11; this helper is intentionally a free
/// function so it can be exercised independently.
#[must_use]
pub fn build_user_turn_input(user_prompt: &str, bundle: &SkillBundle) -> TurnInput {
    TurnInput {
        user_prompt: user_prompt.to_owned(),
        system_prompt_section: skill_injector::render(bundle),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use tempfile::tempdir;

    fn image_block(media_type: &str, data: &str) -> MessageBlock {
        MessageBlock::Image {
            source: ImageSource::Base64 {
                media_type: media_type.to_owned(),
                data: data.to_owned(),
            },
            alt_text: Some("screenshot".to_owned()),
        }
    }

    #[test]
    fn image_submission_is_blocked_for_text_only_provider() {
        let error = validate_user_content(
            &[image_block("image/png", "AAEC")],
            &ProviderCapabilities::default(),
        )
        .expect_err("block unsupported image");
        assert!(error.message.contains("cannot consume image"));
    }

    #[test]
    fn supported_image_content_passes_validation() {
        let capabilities = ProviderCapabilities {
            image_input: true,
            supported_image_media_types: vec!["image/png".to_owned()],
            max_image_bytes: Some(1024),
            max_images_per_request: Some(2),
        };
        validate_user_content(&[image_block("image/png", "AAEC")], &capabilities)
            .expect("accept supported image");
    }

    #[test]
    fn unsupported_media_type_is_rejected() {
        let capabilities = ProviderCapabilities {
            image_input: true,
            supported_image_media_types: vec!["image/png".to_owned()],
            max_image_bytes: None,
            max_images_per_request: None,
        };
        let error = validate_user_content(&[image_block("image/tiff", "AAEC")], &capabilities)
            .expect_err("reject unsupported type");
        assert!(error.message.contains("image/tiff"));
    }

    #[test]
    fn web_tools_are_available_in_standard_and_planning_modes() {
        for mode in [Mode::Yolo, Mode::ReadOnly] {
            let tools = available_tools(mode)
                .into_iter()
                .map(|tool| tool.name)
                .collect::<Vec<_>>();
            assert!(tools.contains(&"web_search".to_owned()));
            assert!(tools.contains(&"web_fetch".to_owned()));
            assert!(tools.contains(&"skill_read".to_owned()));
            assert!(tools.contains(&"update_plan".to_owned()));
        }
    }

    #[test]
    fn initial_model_turn_includes_the_durable_session_goal() {
        let content = content_with_session_goal(
            vec![MessageBlock::Text {
                text: "Build the portfolio page".to_owned(),
            }],
            "Create a responsive portfolio",
        );
        assert!(matches!(
            content.first(),
            Some(MessageBlock::Text { text }) if text == "Current session goal: Create a responsive portfolio"
        ));
        assert!(matches!(
            content.get(1),
            Some(MessageBlock::Text { text }) if text == "Build the portfolio page"
        ));
    }

    #[test]
    fn workspace_instructions_and_skills_are_added_to_the_model_context() {
        let directory = tempdir().expect("temporary directory");
        fs::write(
            directory.path().join("AGENTS.md"),
            "Run the focused test suite.",
        )
        .expect("write instructions");
        let skill = directory.path().join(".medusa/skills/release/SKILL.md");
        fs::create_dir_all(skill.parent().expect("skill directory"))
            .expect("create skill directory");
        fs::write(
            &skill,
            "description: Release preparation\nUse the release checklist.",
        )
        .expect("write skill");

        let prompt = system_prompt(Mode::Yolo, directory.path());
        assert!(prompt.contains("Run the focused test suite."));
        assert!(prompt.contains("release (project): Release preparation"));
        assert!(prompt.contains("call `skill_read`"));
    }

    #[test]
    fn plain_text_clarification_falls_back_to_one_clean_question() {
        let question = question_from_assistant_text(
            "Please answer these:\n1. **What kind of website is it?**\n2. **Who is the audience?**",
        )
        .expect("question");
        assert_eq!(question.tool_use_id, None);
        assert_eq!(question.questions.len(), 1);
        assert_eq!(
            question.questions[0].question,
            "What kind of website is it?"
        );
    }
}
