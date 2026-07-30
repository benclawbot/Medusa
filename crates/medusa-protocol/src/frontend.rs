//! Serializable control and presentation contracts shared by every Medusa frontend.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{CURRENT_PROTOCOL_VERSION, ProtocolVersion};

/// The frontend protocol follows the repository-wide wire protocol version.
pub const FRONTEND_PROTOCOL_VERSION: ProtocolVersion = CURRENT_PROTOCOL_VERSION;

/// A connected frontend implementation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendKind {
    Tui,
    Desktop,
    Telegram,
    Headless,
    Other,
}

/// Whether an attached frontend may mutate the live session.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentMode {
    Owner,
    ReadOnly,
}

/// A structured approval decision forwarded to the authoritative runtime.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    ApproveOnce,
    Deny,
}

/// Versioned command accepted by the live-session control plane.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum FrontendCommand {
    CreateSession {
        repository_profile: String,
        objective: Option<String>,
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
    Submit {
        text: String,
        #[serde(default)]
        attachment_ids: Vec<String>,
    },
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
    /// Rejects malformed commands before they reach runtime policy.
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::CreateSession {
                repository_profile,
                ..
            } if repository_profile.trim().is_empty() => {
                Err("repository profile cannot be empty")
            }
            Self::ResumeSession { session_id } | Self::CancelWorker { worker_id: session_id }
                if session_id.trim().is_empty() =>
            {
                Err("command identifier cannot be empty")
            }
            Self::Attach { session_id, .. } if session_id.trim().is_empty() => {
                Err("session id cannot be empty")
            }
            Self::Submit {
                text,
                attachment_ids,
            } if text.trim().is_empty() && attachment_ids.is_empty() => {
                Err("submission must contain text or an attachment")
            }
            Self::AnswerQuestion {
                question_id,
                answer,
            } if question_id.trim().is_empty() || answer.trim().is_empty() => {
                Err("question id and answer cannot be empty")
            }
            Self::ResolveApproval { approval_id, .. } if approval_id.trim().is_empty() => {
                Err("approval id cannot be empty")
            }
            Self::ConfigureModel { model, .. } if model.trim().is_empty() => {
                Err("model cannot be empty")
            }
            Self::SetEffort { effort } if effort.trim().is_empty() => {
                Err("effort cannot be empty")
            }
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

/// Idempotent command envelope suitable for local IPC and remote gateways.
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
    /// Validates compatibility and the transport-level identity fields.
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

/// User-visible lifecycle associated with a presentation event.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PresentationLifecycle {
    Active,
    Waiting,
    Succeeded,
    Failed,
    Cancelled,
    Informational,
}

/// Stable activity classification. Frontends must not infer this from titles.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PresentationActivityKind {
    Assistant,
    RepositoryRead,
    Edit,
    Command,
    Test,
    Verification,
    Approval,
    Worker,
    Integration,
    Recovery,
    Progress,
    Done,
    Error,
}

/// Safe structured activity emitted by the runtime.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PresentationActivity {
    pub activity_id: String,
    pub kind: PresentationActivityKind,
    pub lifecycle: PresentationLifecycle,
    pub title: String,
    #[serde(default)]
    pub details: Vec<String>,
    #[serde(default)]
    pub affected_paths: Vec<String>,
    pub evidence_ref: Option<String>,
}

/// One stable plan step.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PresentationPlanStep {
    pub step_id: String,
    pub title: String,
    pub lifecycle: PresentationLifecycle,
}

/// One worker shown in the shared team view.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PresentationWorker {
    pub worker_id: String,
    pub role: String,
    pub task: String,
    pub lifecycle: PresentationLifecycle,
    pub current_action: Option<String>,
}

/// One question choice suitable for inline controls or text fallback.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PresentationQuestionOption {
    pub option_id: String,
    pub label: String,
    pub value: String,
}

/// Safe question surface shared across frontends.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PresentationQuestion {
    pub question_id: String,
    pub prompt: String,
    #[serde(default)]
    pub options: Vec<PresentationQuestionOption>,
    pub free_text_allowed: bool,
}

/// Exact approval request. Gateways only forward decisions; runtime policy remains authoritative.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PresentationApproval {
    pub approval_id: String,
    pub action: String,
    pub scope: String,
    pub reason: String,
    pub risk: String,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

/// Runtime-emitted artifact that a frontend is authorized to present.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PresentationArtifact {
    pub artifact_id: String,
    pub name: String,
    pub media_type: String,
    pub evidence_ref: String,
    pub caption: Option<String>,
}

/// Stable typed event consumed by all frontend renderers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum FrontendEvent {
    SubmissionAccepted,
    SubmissionQueued {
        position: u32,
    },
    Started,
    AssistantTextDelta {
        text: String,
    },
    AssistantInterim {
        text: String,
    },
    Activity(PresentationActivity),
    Team {
        workers: Vec<PresentationWorker>,
        verification: Option<String>,
    },
    Plan {
        steps: Vec<PresentationPlanStep>,
        current: Option<String>,
    },
    Question(PresentationQuestion),
    ApprovalRequired(PresentationApproval),
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        total_tokens: u64,
        estimated_cost_microusd: u64,
    },
    Progress {
        turn: u32,
        phase: Option<String>,
    },
    SettingsChanged {
        model: String,
        effort: String,
        plan_mode: bool,
    },
    Notice {
        severity: String,
        title: String,
        details: Vec<String>,
    },
    Artifact(PresentationArtifact),
    TurnFinished,
    Completed {
        summary: Option<String>,
    },
    Cancelled {
        reason: Option<String>,
    },
    Failed {
        message: String,
        recovery: Vec<String>,
    },
}

impl FrontendEvent {
    /// Rejects empty user-visible identifiers and text at the protocol boundary.
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::SubmissionQueued { position } if *position == 0 => {
                Err("queue position must be greater than zero")
            }
            Self::AssistantTextDelta { text } | Self::AssistantInterim { text }
                if text.is_empty() =>
            {
                Err("assistant text cannot be empty")
            }
            Self::Activity(activity)
                if activity.activity_id.trim().is_empty() || activity.title.trim().is_empty() =>
            {
                Err("activity id and title cannot be empty")
            }
            Self::Question(question)
                if question.question_id.trim().is_empty() || question.prompt.trim().is_empty() =>
            {
                Err("question id and prompt cannot be empty")
            }
            Self::ApprovalRequired(approval)
                if approval.approval_id.trim().is_empty()
                    || approval.action.trim().is_empty()
                    || approval.scope.trim().is_empty()
                    || approval.reason.trim().is_empty() =>
            {
                Err("approval identity and safe description cannot be empty")
            }
            Self::Artifact(artifact)
                if artifact.artifact_id.trim().is_empty()
                    || artifact.name.trim().is_empty()
                    || artifact.media_type.trim().is_empty()
                    || artifact.evidence_ref.trim().is_empty() =>
            {
                Err("artifact identity and evidence cannot be empty")
            }
            Self::SettingsChanged { model, effort, .. }
                if model.trim().is_empty() || effort.trim().is_empty() =>
            {
                Err("model and effort cannot be empty")
            }
            Self::Notice { title, .. } if title.trim().is_empty() => {
                Err("notice title cannot be empty")
            }
            Self::Failed { message, .. } if message.trim().is_empty() => {
                Err("failure message cannot be empty")
            }
            _ => Ok(()),
        }
    }
}

/// Cursor-addressable presentation event envelope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FrontendEventEnvelope {
    pub protocol_version: ProtocolVersion,
    pub event_id: String,
    pub cursor: u64,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub parent_event_id: Option<String>,
    pub correlation_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    pub lifecycle: PresentationLifecycle,
    pub event: FrontendEvent,
}

impl FrontendEventEnvelope {
    /// Validates compatibility, cursor monotonic identity, and event content.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !CURRENT_PROTOCOL_VERSION.accepts(self.protocol_version) {
            return Err("frontend event protocol is incompatible");
        }
        if self.cursor == 0 {
            return Err("frontend event cursor must be greater than zero");
        }
        if self.event_id.trim().is_empty()
            || self.session_id.trim().is_empty()
            || self.correlation_id.trim().is_empty()
        {
            return Err("event identity fields cannot be empty");
        }
        self.event.validate()
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
    fn empty_submission_is_rejected() {
        assert!(
            command(FrontendCommand::Submit {
                text: "".to_owned(),
                attachment_ids: Vec::new(),
            })
            .validate()
            .is_err()
        );
    }

    #[test]
    fn event_envelope_round_trips() {
        let envelope = FrontendEventEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            event_id: "event-1".to_owned(),
            cursor: 1,
            session_id: "session-1".to_owned(),
            turn_id: Some("turn-1".to_owned()),
            parent_event_id: None,
            correlation_id: "correlation-1".to_owned(),
            timestamp: datetime!(2026-07-30 16:01 UTC),
            lifecycle: PresentationLifecycle::Active,
            event: FrontendEvent::Activity(PresentationActivity {
                activity_id: "activity-1".to_owned(),
                kind: PresentationActivityKind::Verification,
                lifecycle: PresentationLifecycle::Succeeded,
                title: "Telegram renderer snapshots".to_owned(),
                details: vec!["24 passed".to_owned()],
                affected_paths: Vec::new(),
                evidence_ref: Some("evidence-1".to_owned()),
            }),
        };
        envelope.validate().expect("valid event");
        let encoded = serde_json::to_string(&envelope).expect("serialize");
        assert_eq!(
            serde_json::from_str::<FrontendEventEnvelope>(&encoded).expect("deserialize"),
            envelope
        );
    }

    #[test]
    fn zero_cursor_is_rejected() {
        let envelope = FrontendEventEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            event_id: "event-1".to_owned(),
            cursor: 0,
            session_id: "session-1".to_owned(),
            turn_id: None,
            parent_event_id: None,
            correlation_id: "correlation-1".to_owned(),
            timestamp: datetime!(2026-07-30 16:01 UTC),
            lifecycle: PresentationLifecycle::Active,
            event: FrontendEvent::Started,
        };
        assert!(envelope.validate().is_err());
    }
}
