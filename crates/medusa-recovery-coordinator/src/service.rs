use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::{
    AuthorizedRecoveryAction, RecoveryActionOutcome, RecoveryActionRejection,
    RecoveryActionRequest, RecoveryAuditRecord, RecoveryAuditStore, RecoveryAuditStoreError,
    RecoveryPreflightEvidence, RecoveryView, VerificationState,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryExecutionOutcome {
    pub outcome: RecoveryActionOutcome,
    pub repository_fingerprint_after: Option<String>,
    pub verification_outcome: VerificationState,
}

impl RecoveryExecutionOutcome {
    #[must_use]
    pub fn succeeded(
        repository_fingerprint_after: impl Into<String>,
        verification_outcome: VerificationState,
    ) -> Self {
        Self {
            outcome: RecoveryActionOutcome::Succeeded,
            repository_fingerprint_after: Some(repository_fingerprint_after.into()),
            verification_outcome,
        }
    }

    #[must_use]
    pub fn cancelled(verification_outcome: VerificationState) -> Self {
        Self {
            outcome: RecoveryActionOutcome::Cancelled,
            repository_fingerprint_after: None,
            verification_outcome,
        }
    }

    #[must_use]
    pub fn failed_closed(
        reason: impl Into<String>,
        repository_fingerprint_after: Option<String>,
        verification_outcome: VerificationState,
    ) -> Self {
        Self {
            outcome: RecoveryActionOutcome::FailedClosed {
                reason: reason.into(),
            },
            repository_fingerprint_after,
            verification_outcome,
        }
    }
}

pub trait RecoveryActionExecutor {
    type Error: std::error::Error + Send + Sync + 'static;

    fn execute(
        &mut self,
        repository: &Path,
        action: &AuthorizedRecoveryAction,
    ) -> Result<RecoveryExecutionOutcome, Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryExecutionReceipt {
    pub record: RecoveryAuditRecord,
    pub audit_path: PathBuf,
}

#[derive(Debug, Error)]
pub enum RecoveryExecutionError<E>
where
    E: std::error::Error + Send + Sync + 'static,
{
    #[error("recovery action was rejected: {0}")]
    Rejected(#[from] RecoveryActionRejection),
    #[error("recovery action execution failed before an outcome could be recorded: {0}")]
    Executor(E),
    #[error("recovery outcome could not be persisted: {0}")]
    Audit(#[from] RecoveryAuditStoreError),
}

pub struct RecoveryActionService<E> {
    executor: E,
}

impl<E> RecoveryActionService<E>
where
    E: RecoveryActionExecutor,
{
    #[must_use]
    pub fn new(executor: E) -> Self {
        Self { executor }
    }

    pub fn execute_and_audit(
        &mut self,
        repository: &Path,
        view: &RecoveryView,
        request: &RecoveryActionRequest,
        preflight: RecoveryPreflightEvidence,
        recorded_at_unix_ms: i64,
    ) -> Result<RecoveryExecutionReceipt, RecoveryExecutionError<E::Error>> {
        let action = view.authorize_action(request)?;
        let result = self
            .executor
            .execute(repository, &action)
            .map_err(RecoveryExecutionError::Executor)?;
        let record = RecoveryAuditRecord::new(
            recorded_at_unix_ms,
            &action,
            preflight,
            result.outcome,
            result.repository_fingerprint_after,
            result.verification_outcome,
        );
        let audit_path = RecoveryAuditStore::for_repository(repository).append(&record)?;
        Ok(RecoveryExecutionReceipt { record, audit_path })
    }

    #[must_use]
    pub fn into_executor(self) -> E {
        self.executor
    }
}

#[cfg(test)]
mod tests {
    use std::{convert::Infallible, fs};

    use super::*;
    use crate::{CheckpointPresentation, RecoveryOperation, RecoveryViewInput, VerificationState};
    use tempfile::tempdir;

    #[derive(Default)]
    struct RecordingExecutor {
        calls: Vec<AuthorizedRecoveryAction>,
    }

    impl RecoveryActionExecutor for RecordingExecutor {
        type Error = Infallible;

        fn execute(
            &mut self,
            _repository: &Path,
            action: &AuthorizedRecoveryAction,
        ) -> Result<RecoveryExecutionOutcome, Self::Error> {
            self.calls.push(action.clone());
            Ok(RecoveryExecutionOutcome::succeeded(
                "b".repeat(64),
                VerificationState::Verified,
            ))
        }
    }

    fn view() -> RecoveryView {
        RecoveryView::build(RecoveryViewInput {
            session_id: "session-1".into(),
            last_durable_step: "implement".into(),
            interrupted_operation: Some("cargo test".into()),
            current_repository_fingerprint: "a".repeat(64),
            verification: VerificationState::Incomplete,
            approvals_must_be_reestablished: false,
            containment_must_be_reestablished: false,
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
            selected_preview: None,
            source_corrupt: false,
        })
    }

    fn preflight() -> RecoveryPreflightEvidence {
        RecoveryPreflightEvidence {
            repository_fingerprint_before: "a".repeat(64),
            checkpoint_integrity_verified: true,
            repository_preconditions_verified: true,
            conflicting_uncommitted_paths: Vec::new(),
            unresolved_risks: Vec::new(),
        }
    }

    #[test]
    fn successful_action_is_authorized_executed_and_persisted_once() {
        let repo = tempdir().expect("temporary repository");
        let mut service = RecoveryActionService::new(RecordingExecutor::default());
        let request = RecoveryActionRequest {
            session_id: "session-1".into(),
            operation: RecoveryOperation::Resume,
            checkpoint_id: None,
            confirmed_destructive_effects: false,
        };

        let receipt = service
            .execute_and_audit(
                repo.path(),
                &view(),
                &request,
                preflight(),
                1_700_000_000_000,
            )
            .expect("execute recovery");

        assert_eq!(receipt.record.outcome, RecoveryActionOutcome::Succeeded);
        assert!(receipt.record.verify());
        assert!(receipt.audit_path.exists());
        assert_eq!(service.into_executor().calls.len(), 1);
    }

    #[test]
    fn rejected_actions_never_reach_the_executor_or_audit_store() {
        let repo = tempdir().expect("temporary repository");
        let mut service = RecoveryActionService::new(RecordingExecutor::default());
        let request = RecoveryActionRequest {
            session_id: "other-session".into(),
            operation: RecoveryOperation::Resume,
            checkpoint_id: None,
            confirmed_destructive_effects: false,
        };

        assert!(matches!(
            service.execute_and_audit(
                repo.path(),
                &view(),
                &request,
                preflight(),
                1_700_000_000_000
            ),
            Err(RecoveryExecutionError::Rejected(
                RecoveryActionRejection::SessionMismatch
            ))
        ));
        assert!(service.into_executor().calls.is_empty());
        assert!(!repo.path().join(".medusa/recovery-audit").exists());
    }

    #[test]
    fn persisted_receipt_survives_process_independent_readback() {
        let repo = tempdir().expect("temporary repository");
        let mut service = RecoveryActionService::new(RecordingExecutor::default());
        let request = RecoveryActionRequest {
            session_id: "session-1".into(),
            operation: RecoveryOperation::Resume,
            checkpoint_id: None,
            confirmed_destructive_effects: false,
        };
        let receipt = service
            .execute_and_audit(
                repo.path(),
                &view(),
                &request,
                preflight(),
                1_700_000_000_001,
            )
            .unwrap();

        let raw = fs::read(&receipt.audit_path).unwrap();
        let decoded: RecoveryAuditRecord = serde_json::from_slice(&raw).unwrap();
        assert_eq!(decoded, receipt.record);
        assert!(decoded.verify());
    }
}
