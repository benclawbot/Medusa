use std::{collections::VecDeque, path::Path};

use medusa_config::Config;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use medusa_protocol::{Actor, EventPayload};
use medusa_provider::{
    ImageSource, Message, MessageBlock, ModelProvider, ModelRequest, ProviderCapabilities,
    ResponseBlock, Role,
};
use time::OffsetDateTime;

use crate::{
    evidence::append_event,
    session::{AgentSession, bootstrap, load, persist},
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
        validate_messages(&session.messages, &self.provider.capabilities())?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
