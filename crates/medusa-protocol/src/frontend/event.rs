use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{CURRENT_PROTOCOL_VERSION, ProtocolVersion};

#[cfg(test)]
use super::FRONTEND_PROTOCOL_VERSION;

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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PresentationPlanStep {
    pub step_id: String,
    pub title: String,
    pub lifecycle: PresentationLifecycle,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PresentationWorker {
    pub worker_id: String,
    pub role: String,
    pub task: String,
    pub lifecycle: PresentationLifecycle,
    pub current_action: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PresentationQuestionOption {
    pub option_id: String,
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PresentationQuestion {
    pub question_id: String,
    pub prompt: String,
    #[serde(default)]
    pub options: Vec<PresentationQuestionOption>,
    pub free_text_allowed: bool,
}

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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PresentationArtifact {
    pub artifact_id: String,
    pub name: String,
    pub media_type: String,
    pub evidence_ref: String,
    pub caption: Option<String>,
}

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
        #[serde(default)]
        cache_read_input_tokens: u64,
        #[serde(default)]
        cache_creation_input_tokens: u64,
        total_tokens: u64,
        #[serde(default)]
        duration_ms: u64,
        #[serde(default)]
        tokens_per_second_milli: u64,
        estimated_cost_microusd: u64,
        #[serde(default)]
        provenance: String,
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

    fn event(cursor: u64) -> FrontendEventEnvelope {
        FrontendEventEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            event_id: "event-1".to_owned(),
            cursor,
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
        }
    }

    #[test]
    fn event_envelope_round_trips() {
        let envelope = event(1);
        envelope.validate().expect("valid event");
        let encoded = serde_json::to_string(&envelope).expect("serialize");
        assert_eq!(
            serde_json::from_str::<FrontendEventEnvelope>(&encoded).expect("deserialize"),
            envelope
        );
    }

    #[test]
    fn zero_cursor_is_rejected() {
        assert!(event(0).validate().is_err());
    }
}
