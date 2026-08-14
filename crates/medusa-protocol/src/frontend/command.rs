use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::{
    CURRENT_PROTOCOL_VERSION, ProtocolVersion, SessionActionDeliveryPolicy, SessionActionKind,
    SessionActionWakePolicy,
};

#[cfg(test)]
use super::FRONTEND_PROTOCOL_VERSION;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendKind {
    Tui,
    Desktop,
    Telegram,
    Headless,
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentMode {
    Owner,
    ReadOnly,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    ApproveOnce,
    Deny,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum FrontendCommand {
    CreateSession {
        repository_profile: String,
        objective: Option<String>,
        #[serde(default)]
        attachment_ids: Vec<String>,
    },
    ListSessions,
    ResumeSession {
        session_id: String,
    },
    Attach {
        session_id: String,
        mode: AttachmentMode,
        after_cursor: Option<u64>,
    },
    Detach,
    Replay {
        after_cursor: u64,
    },
    PollTransient,
    NewSession,
    RunCommand {
        input: String,
    },
    RecoveryAction {
        operation: String,
        checkpoint_id: Option<String>,
        confirmed_destructive_effects: bool,
    },
    PreviewSelectiveRevert {
        mutation_id: String,
    },
    ApplySelectiveRevert {
        mutation_id: String,
    },
    Submit {
        text: String,
        #[serde(default)]
        attachment_ids: Vec<String>,
    },
    SubmitSessionAction {
        kind: SessionActionKind,
        delivery_policy: SessionActionDeliveryPolicy,
        wake_policy: SessionActionWakePolicy,
        expected_session_revision: u64,
        payload: Value,
    },
    ShowSessionActions,
    AnswerQuestion {
        question_id: String,
        answer: String,
    },
    ResolveApproval {
        approval_id: String,
        decision: ApprovalDecision,
    },
    CancelTurn,
    ConfigureModel {
        provider: Option<String>,
        model: String,
    },
    SetEffort {
        effort: String,
    },
    SetPlanMode {
        enabled: bool,
    },
    ShowStatus,
    SteerWorker {
        worker_id: String,
        instruction: String,
    },
    CancelWorker {
        worker_id: String,
    },
    StopTeam,
    AcknowledgeCursor {
        cursor: u64,
    },
}

impl FrontendCommand {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::CreateSession {
                repository_profile, ..
            } if repository_profile.trim().is_empty() => Err("repository profile cannot be empty"),
            Self::ResumeSession { session_id }
            | Self::CancelWorker {
                worker_id: session_id,
            } if session_id.trim().is_empty() => Err("command identifier cannot be empty"),
            Self::Attach { session_id, .. } if session_id.trim().is_empty() => {
                Err("session id cannot be empty")
            }
            Self::Submit {
                text,
                attachment_ids,
            } if text.trim().is_empty() && attachment_ids.is_empty() => {
                Err("submission must contain text or an attachment")
            }
            Self::SubmitSessionAction { kind, payload, .. } => match kind {
                SessionActionKind::Steer | SessionActionKind::FollowUp
                    if payload
                        .get("text")
                        .and_then(Value::as_str)
                        .is_none_or(|text| text.trim().is_empty()) =>
                {
                    Err("steer/follow-up action requires non-empty text")
                }
                SessionActionKind::ReplaceFollowUp
                    if payload
                        .get("text")
                        .and_then(Value::as_str)
                        .is_none_or(|text| text.trim().is_empty())
                        || payload
                            .get("replaces_action_id")
                            .and_then(Value::as_str)
                            .is_none_or(|action_id| action_id.trim().is_empty()) =>
                {
                    Err("replacement follow-up requires text and replaces_action_id")
                }
                SessionActionKind::GoalAdjustment
                    if payload
                        .get("objective")
                        .and_then(Value::as_str)
                        .is_none_or(|text| text.trim().is_empty()) =>
                {
                    Err("goal-adjustment action requires a non-empty objective")
                }
                SessionActionKind::Cancel
                    if *payload != Value::Null && *payload != serde_json::json!({}) =>
                {
                    Err("cancel action does not accept a payload")
                }
                _ => Ok(()),
            },
            Self::AnswerQuestion {
                question_id,
                answer,
            } if question_id.trim().is_empty() || answer.trim().is_empty() => {
                Err("question id and answer cannot be empty")
            }
            Self::ResolveApproval { approval_id, .. } if approval_id.trim().is_empty() => {
                Err("approval id cannot be empty")
            }
            Self::RunCommand { input }
                if input.trim().is_empty() || !input.trim_start().starts_with('/') =>
            {
                Err("runtime command must be a slash command")
            }
            Self::RecoveryAction { operation, .. } if operation.trim().is_empty() => {
                Err("recovery operation cannot be empty")
            }
            Self::PreviewSelectiveRevert { mutation_id }
            | Self::ApplySelectiveRevert { mutation_id }
                if mutation_id.trim().is_empty() =>
            {
                Err("mutation id cannot be empty")
            }
            Self::ConfigureModel { model, .. } if model.trim().is_empty() => {
                Err("model cannot be empty")
            }
            Self::SetEffort { effort } if effort.trim().is_empty() => Err("effort cannot be empty"),
            Self::SteerWorker {
                worker_id,
                instruction,
            } if worker_id.trim().is_empty() || instruction.trim().is_empty() => {
                Err("worker id and steering instruction cannot be empty")
            }
            Self::AcknowledgeCursor { cursor } if *cursor == 0 => {
                Err("acknowledged cursor must be greater than zero")
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FrontendCommandEnvelope {
    pub protocol_version: ProtocolVersion,
    pub command_id: String,
    pub idempotency_key: String,
    pub frontend: FrontendKind,
    pub client_id: String,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    pub command: FrontendCommand,
}

impl FrontendCommandEnvelope {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !CURRENT_PROTOCOL_VERSION.accepts(self.protocol_version) {
            return Err("frontend command protocol is incompatible");
        }
        if self.command_id.trim().is_empty()
            || self.idempotency_key.trim().is_empty()
            || self.client_id.trim().is_empty()
        {
            return Err("command identity fields cannot be empty");
        }
        self.command.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn command(command: FrontendCommand) -> FrontendCommandEnvelope {
        FrontendCommandEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            command_id: "command-1".to_owned(),
            idempotency_key: "telegram:42:9".to_owned(),
            frontend: FrontendKind::Telegram,
            client_id: "telegram-user-42".to_owned(),
            session_id: Some("session-1".to_owned()),
            turn_id: None,
            timestamp: datetime!(2026-07-30 16:00 UTC),
            command,
        }
    }

    #[test]
    fn command_envelope_round_trips() {
        let envelope = command(FrontendCommand::Submit {
            text: "inspect the failing test".to_owned(),
            attachment_ids: vec!["attachment-1".to_owned()],
        });
        envelope.validate().expect("valid command");
        let encoded = serde_json::to_string(&envelope).expect("serialize");
        assert_eq!(
            serde_json::from_str::<FrontendCommandEnvelope>(&encoded).expect("deserialize"),
            envelope
        );
    }

    #[test]
    fn session_action_command_round_trips() {
        let envelope = command(FrontendCommand::SubmitSessionAction {
            kind: SessionActionKind::Steer,
            delivery_policy: SessionActionDeliveryPolicy::NextSafeTurnBoundary,
            wake_policy: SessionActionWakePolicy::OnBoundary,
            expected_session_revision: 7,
            payload: serde_json::json!({"text":"use the new constraint"}),
        });
        envelope.validate().expect("valid action command");
        let encoded = serde_json::to_string(&envelope).expect("serialize");
        assert_eq!(
            serde_json::from_str::<FrontendCommandEnvelope>(&encoded).expect("deserialize"),
            envelope
        );
    }

    #[test]
    fn replacement_action_requires_target_identity() {
        assert!(
            command(FrontendCommand::SubmitSessionAction {
                kind: SessionActionKind::ReplaceFollowUp,
                delivery_policy: SessionActionDeliveryPolicy::WhenIdle,
                wake_policy: SessionActionWakePolicy::OnBoundary,
                expected_session_revision: 7,
                payload: serde_json::json!({"text":"replace it"}),
            })
            .validate()
            .is_err()
        );
    }

    #[test]
    fn empty_submission_is_rejected() {
        assert!(
            command(FrontendCommand::Submit {
                text: String::new(),
                attachment_ids: Vec::new(),
            })
            .validate()
            .is_err()
        );
    }
}
