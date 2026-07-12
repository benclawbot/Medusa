use std::{collections::VecDeque, path::Path};

use medusa_config::Config;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use medusa_protocol::{Actor, EventPayload};
use medusa_provider::{Message, MessageBlock, ModelProvider, ModelRequest, ResponseBlock, Role};
use time::OffsetDateTime;

use crate::{
    evidence::append_event,
    session::{bootstrap, load, persist, AgentSession},
    tools::{built_in_tools, execute_tool},
    verification::targeted_verification,
};

const SYSTEM_PROMPT: &str = "You are Medusa, an autonomous coding agent. Inspect the repository, make the smallest correct change, and verify it. Use tools rather than inventing repository contents. Never modify tests, verification scripts, snapshots, fixtures, or expected outputs unless the user explicitly asks for that exact change; fix the product code instead. Do not expose private chain-of-thought; provide concise decisions and evidence.";

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
        load(repo, session)
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
                ResponseBlock::Text { text } => assistant_blocks.push(MessageBlock::Text { text }),
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

fn json_error(error: serde_json::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        error.to_string(),
    )
}
