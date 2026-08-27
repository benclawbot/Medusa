use medusa_recovery_coordinator::{
    CheckpointPresentation, RecoveryExecutionOutcome, RecoveryOperation, RecoveryPreview,
    VerificationState,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct PersistedRecoveryRecord {
    pub(crate) session_id: String,
    pub(crate) last_durable_step: String,
    pub(crate) interrupted_operation: Option<String>,
    pub(crate) current_repository_fingerprint: String,
    pub(crate) verification: VerificationState,
    pub(crate) approvals_must_be_reestablished: bool,
    pub(crate) containment_must_be_reestablished: bool,
    pub(crate) checkpoints: Vec<CheckpointPresentation>,
    pub(crate) selected_preview: Option<RecoveryPreview>,
}

pub(crate) fn common_outcome(
    operation: RecoveryOperation,
    repository_fingerprint: &str,
) -> Option<RecoveryExecutionOutcome> {
    match operation {
        RecoveryOperation::Inspect => Some(RecoveryExecutionOutcome::succeeded(
            repository_fingerprint.to_owned(),
            VerificationState::Unknown,
        )),
        RecoveryOperation::Resume | RecoveryOperation::RetryVerification => {
            Some(RecoveryExecutionOutcome::succeeded(
                repository_fingerprint.to_owned(),
                VerificationState::Incomplete,
            ))
        }
        RecoveryOperation::Abandon => Some(RecoveryExecutionOutcome::cancelled(
            VerificationState::Incomplete,
        )),
        RecoveryOperation::RestoreCheckpoint => None,
    }
}
