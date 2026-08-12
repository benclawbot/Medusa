use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{RecoveryOperation, RecoveryView};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryActionRequest {
    pub session_id: String,
    pub operation: RecoveryOperation,
    pub checkpoint_id: Option<String>,
    pub confirmed_destructive_effects: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizedRecoveryAction {
    pub session_id: String,
    pub operation: RecoveryOperation,
    pub checkpoint_id: Option<String>,
    pub confirmation_recorded: bool,
    pub authorization_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum RecoveryActionRejection {
    #[error("recovery request targets a different session")]
    SessionMismatch,
    #[error("requested recovery action is unavailable: {0}")]
    ActionUnavailable(String),
    #[error("restore requires a selected checkpoint")]
    MissingCheckpoint,
    #[error("restore checkpoint does not match the current preview")]
    CheckpointPreviewMismatch,
    #[error("explicit confirmation is required before destructive recovery")]
    ConfirmationRequired,
}

impl RecoveryView {
    pub fn authorize_action(
        &self,
        request: &RecoveryActionRequest,
    ) -> Result<AuthorizedRecoveryAction, RecoveryActionRejection> {
        if request.session_id != self.session_id {
            return Err(RecoveryActionRejection::SessionMismatch);
        }

        let availability = self.action(request.operation).ok_or_else(|| {
            RecoveryActionRejection::ActionUnavailable("action is unknown".into())
        })?;
        if !availability.enabled {
            return Err(RecoveryActionRejection::ActionUnavailable(
                availability.reason.clone(),
            ));
        }

        if request.operation == RecoveryOperation::RestoreCheckpoint {
            let checkpoint_id = request
                .checkpoint_id
                .as_deref()
                .ok_or(RecoveryActionRejection::MissingCheckpoint)?;
            let preview = self
                .selected_preview
                .as_ref()
                .ok_or(RecoveryActionRejection::MissingCheckpoint)?;
            if preview.checkpoint_id != checkpoint_id {
                return Err(RecoveryActionRejection::CheckpointPreviewMismatch);
            }
        }

        if availability.requires_confirmation && !request.confirmed_destructive_effects {
            return Err(RecoveryActionRejection::ConfirmationRequired);
        }

        Ok(AuthorizedRecoveryAction {
            session_id: request.session_id.clone(),
            operation: request.operation,
            checkpoint_id: request.checkpoint_id.clone(),
            confirmation_recorded: availability.requires_confirmation
                && request.confirmed_destructive_effects,
            authorization_reason: availability.reason.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CheckpointPresentation, FileChangeKind, RecoveryFileChange, RecoveryPreview,
        RecoveryViewInput, VerificationState,
    };

    fn view(conflicting: bool, containment_recheck: bool) -> RecoveryView {
        RecoveryView::build(RecoveryViewInput {
            session_id: "session-1".into(),
            last_durable_step: "implement".into(),
            interrupted_operation: Some("cargo test".into()),
            current_repository_fingerprint: "b".repeat(64),
            verification: VerificationState::Incomplete,
            approvals_must_be_reestablished: false,
            containment_must_be_reestablished: containment_recheck,
            checkpoints: vec![CheckpointPresentation {
                id: "cp-1".into(),
                sequence: 1,
                created_at_unix_ms: 1_700_000_000_000,
                task_step: "implement".into(),
                reason: "durable progress".into(),
                repository_fingerprint: "a".repeat(64),
                verification: VerificationState::Incomplete,
                provenance: "execution-checkpoint/v1".into(),
                integrity_verified: true,
            }],
            selected_preview: Some(RecoveryPreview {
                checkpoint_id: "cp-1".into(),
                files: vec![RecoveryFileChange {
                    path: "src/lib.rs".into(),
                    kind: FileChangeKind::Modified,
                    would_overwrite_uncommitted_work: conflicting,
                }],
                unresolved_risks: Vec::new(),
                repository_matches_checkpoint_base: true,
            }),
            source_corrupt: false,
        })
    }

    #[test]
    fn destructive_restore_requires_explicit_confirmation() {
        let result = view(true, false).authorize_action(&RecoveryActionRequest {
            session_id: "session-1".into(),
            operation: RecoveryOperation::RestoreCheckpoint,
            checkpoint_id: Some("cp-1".into()),
            confirmed_destructive_effects: false,
        });
        assert_eq!(result, Err(RecoveryActionRejection::ConfirmationRequired));
    }

    #[test]
    fn confirmed_restore_is_authorized_and_records_confirmation() {
        let action = view(true, false)
            .authorize_action(&RecoveryActionRequest {
                session_id: "session-1".into(),
                operation: RecoveryOperation::RestoreCheckpoint,
                checkpoint_id: Some("cp-1".into()),
                confirmed_destructive_effects: true,
            })
            .unwrap();
        assert!(action.confirmation_recorded);
        assert_eq!(action.checkpoint_id.as_deref(), Some("cp-1"));
    }

    #[test]
    fn stale_checkpoint_selection_fails_closed() {
        let result = view(false, false).authorize_action(&RecoveryActionRequest {
            session_id: "session-1".into(),
            operation: RecoveryOperation::RestoreCheckpoint,
            checkpoint_id: Some("cp-other".into()),
            confirmed_destructive_effects: true,
        });
        assert_eq!(
            result,
            Err(RecoveryActionRejection::CheckpointPreviewMismatch)
        );
    }

    #[test]
    fn disabled_actions_preserve_the_view_reason() {
        let result = view(false, true).authorize_action(&RecoveryActionRequest {
            session_id: "session-1".into(),
            operation: RecoveryOperation::Resume,
            checkpoint_id: None,
            confirmed_destructive_effects: false,
        });
        assert!(matches!(
            result,
            Err(RecoveryActionRejection::ActionUnavailable(_))
        ));
    }
}
