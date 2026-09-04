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
}
