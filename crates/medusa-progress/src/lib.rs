use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressKind {
    Started,
    PlanUpdated,
    ToolStarted,
    ToolFinished,
    CheckpointCreated,
    Blocked,
    Retrying,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProgressEvent {
    pub sequence: u64,
    pub kind: ProgressKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<String>,
}

/// Durability routing for a progress kind: which RuntimeEvent/journal class must
/// carry it so cost, plans, checkpoints, blocks, tool executions, and outcomes
/// survive restart instead of living as presentation-only ephemera.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurableEventClass {
    CanonicalJournal(&'static str),
    DurableProjection(&'static str),
    PresentationOnly(&'static str),
}

impl DurableEventClass {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::CanonicalJournal(label)
            | Self::DurableProjection(label)
            | Self::PresentationOnly(label) => label,
        }
    }

    #[must_use]
    pub const fn is_durable(self) -> bool {
        !matches!(self, Self::PresentationOnly(_))
    }
}

impl ProgressKind {
    /// Maps lifecycle-significant progress (checkpoints, blocks, tool
    /// start/finish, terminal outcomes) to durable classes; only the purely
    /// presentational busy indicator stays ephemeral.
    #[must_use]
    pub const fn durable_event_class(&self) -> DurableEventClass {
        match self {
            Self::Started => DurableEventClass::PresentationOnly("frontend busy indicator"),
            Self::PlanUpdated => DurableEventClass::DurableProjection("plan_updated"),
            Self::ToolStarted => DurableEventClass::DurableProjection("tool_execution_started"),
            Self::ToolFinished => DurableEventClass::DurableProjection("tool_execution_finished"),
            Self::CheckpointCreated => DurableEventClass::CanonicalJournal("checkpoint_created"),
            Self::Blocked => DurableEventClass::CanonicalJournal("execution_blocked"),
            Self::Retrying => DurableEventClass::DurableProjection("execution_retry"),
            Self::Completed => DurableEventClass::CanonicalJournal("execution_completed"),
            Self::Failed => DurableEventClass::CanonicalJournal("execution_failed"),
        }
    }
}

impl ProgressEvent {
    pub fn new(
        sequence: u64,
        kind: ProgressKind,
        message: impl Into<String>,
    ) -> MedusaResult<Self> {
        let message = message.into();
        if message.trim().is_empty() {
            return Err(validation("progress message cannot be empty"));
        }
        Ok(Self {
            sequence,
            kind,
            message,
            step_id: None,
            checkpoint_id: None,
        })
    }
}

fn validation(message: &'static str) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidInput, ErrorCategory::Validation, message)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_message_is_rejected() {
        assert!(ProgressEvent::new(1, ProgressKind::Started, "  ").is_err());
        assert!(ProgressEvent::new(1, ProgressKind::Started, "go").is_ok());
    }

    #[test]
    fn lifecycle_progress_is_durable_not_presentation_only() {
        for kind in [
            ProgressKind::CheckpointCreated,
            ProgressKind::Blocked,
            ProgressKind::ToolStarted,
            ProgressKind::ToolFinished,
            ProgressKind::Completed,
            ProgressKind::Failed,
        ] {
            assert!(
                kind.durable_event_class().is_durable(),
                "{kind:?} must survive restart"
            );
        }
        assert!(!ProgressKind::Started.durable_event_class().is_durable());
    }
}
