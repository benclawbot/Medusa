use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{AuthorizedRecoveryAction, RecoveryOperation, VerificationState};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryPreflightEvidence {
    pub repository_fingerprint_before: String,
    pub checkpoint_integrity_verified: bool,
    pub repository_preconditions_verified: bool,
    pub conflicting_uncommitted_paths: Vec<String>,
    pub unresolved_risks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryActionOutcome {
    Succeeded,
    Cancelled,
    FailedClosed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryAuditRecord {
    pub schema_version: u32,
    pub recorded_at_unix_ms: i64,
    pub session_id: String,
    pub operation: RecoveryOperation,
    pub checkpoint_id: Option<String>,
    pub confirmation_recorded: bool,
    pub authorization_reason: String,
    pub preflight: RecoveryPreflightEvidence,
    pub outcome: RecoveryActionOutcome,
    pub repository_fingerprint_after: Option<String>,
    pub verification_outcome: VerificationState,
    pub evidence_fingerprint: String,
}

impl RecoveryAuditRecord {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn new(
        recorded_at_unix_ms: i64,
        action: &AuthorizedRecoveryAction,
        preflight: RecoveryPreflightEvidence,
        outcome: RecoveryActionOutcome,
        repository_fingerprint_after: Option<String>,
        verification_outcome: VerificationState,
    ) -> Self {
        let mut record = Self {
            schema_version: Self::SCHEMA_VERSION,
            recorded_at_unix_ms,
            session_id: action.session_id.clone(),
            operation: action.operation,
            checkpoint_id: action.checkpoint_id.clone(),
            confirmation_recorded: action.confirmation_recorded,
            authorization_reason: action.authorization_reason.clone(),
            preflight,
            outcome,
            repository_fingerprint_after,
            verification_outcome,
            evidence_fingerprint: String::new(),
        };
        record.evidence_fingerprint = record.compute_fingerprint();
        record
    }

    pub fn verify(&self) -> bool {
        self.schema_version == Self::SCHEMA_VERSION
            && self.evidence_fingerprint == self.compute_fingerprint()
    }

    fn compute_fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.schema_version.to_be_bytes());
        hasher.update(self.recorded_at_unix_ms.to_be_bytes());
        hash_field(&mut hasher, &self.session_id);
        hash_field(&mut hasher, recovery_operation_id(self.operation));
        hash_optional(&mut hasher, self.checkpoint_id.as_deref());
        hasher.update([u8::from(self.confirmation_recorded)]);
        hash_field(&mut hasher, &self.authorization_reason);
        hash_field(&mut hasher, &self.preflight.repository_fingerprint_before);
        hasher.update([u8::from(self.preflight.checkpoint_integrity_verified)]);
        hasher.update([u8::from(self.preflight.repository_preconditions_verified)]);
        for path in &self.preflight.conflicting_uncommitted_paths {
            hash_field(&mut hasher, path);
        }
        hasher.update([0xff]);
        for risk in &self.preflight.unresolved_risks {
            hash_field(&mut hasher, risk);
        }
        hash_recovery_outcome(&mut hasher, &self.outcome);
        hash_optional(&mut hasher, self.repository_fingerprint_after.as_deref());
        hash_field(
            &mut hasher,
            verification_state_id(self.verification_outcome),
        );
        hex::encode(hasher.finalize())
    }
}

fn recovery_operation_id(operation: RecoveryOperation) -> &'static str {
    match operation {
        RecoveryOperation::Inspect => "recovery-operation/inspect/v1",
        RecoveryOperation::Resume => "recovery-operation/resume/v1",
        RecoveryOperation::RestoreCheckpoint => "recovery-operation/restore-checkpoint/v1",
        RecoveryOperation::RetryVerification => "recovery-operation/retry-verification/v1",
        RecoveryOperation::Abandon => "recovery-operation/abandon/v1",
    }
}

fn verification_state_id(state: VerificationState) -> &'static str {
    match state {
        VerificationState::Verified => "verification-state/verified/v1",
        VerificationState::Failed => "verification-state/failed/v1",
        VerificationState::Incomplete => "verification-state/incomplete/v1",
        VerificationState::Unknown => "verification-state/unknown/v1",
    }
}

fn hash_recovery_outcome(hasher: &mut Sha256, outcome: &RecoveryActionOutcome) {
    match outcome {
        RecoveryActionOutcome::Succeeded => {
            hash_field(hasher, "recovery-outcome/succeeded/v1");
        }
        RecoveryActionOutcome::Cancelled => {
            hash_field(hasher, "recovery-outcome/cancelled/v1");
        }
        RecoveryActionOutcome::FailedClosed { reason } => {
            hash_field(hasher, "recovery-outcome/failed-closed/v1");
            hash_field(hasher, reason);
        }
    }
}

fn hash_field(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

fn hash_optional(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_field(hasher, value);
        }
        None => hasher.update([0]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action() -> AuthorizedRecoveryAction {
        AuthorizedRecoveryAction {
            session_id: "session-1".into(),
            operation: RecoveryOperation::RestoreCheckpoint,
            checkpoint_id: Some("cp-1".into()),
            confirmation_recorded: true,
            authorization_reason: "restore confirmed".into(),
        }
    }

    fn preflight() -> RecoveryPreflightEvidence {
        RecoveryPreflightEvidence {
            repository_fingerprint_before: "a".repeat(64),
            checkpoint_integrity_verified: true,
            repository_preconditions_verified: true,
            conflicting_uncommitted_paths: vec!["src/lib.rs".into()],
            unresolved_risks: Vec::new(),
        }
    }

    #[test]
    fn records_choice_preflight_result_and_verification() {
        let record = RecoveryAuditRecord::new(
            1_700_000_000_000,
            &action(),
            preflight(),
            RecoveryActionOutcome::Succeeded,
            Some("b".repeat(64)),
            VerificationState::Verified,
        );
        assert!(record.verify());
        assert_eq!(record.checkpoint_id.as_deref(), Some("cp-1"));
        assert!(record.confirmation_recorded);
    }

    #[test]
    fn cancelled_recovery_preserves_evidence() {
        let record = RecoveryAuditRecord::new(
            1_700_000_000_000,
            &action(),
            preflight(),
            RecoveryActionOutcome::Cancelled,
            None,
            VerificationState::Incomplete,
        );
        assert!(record.verify());
        assert_eq!(record.outcome, RecoveryActionOutcome::Cancelled);
    }

    #[test]
    fn failed_closed_reason_is_part_of_stable_evidence() {
        let mut record = RecoveryAuditRecord::new(
            1_700_000_000_000,
            &action(),
            preflight(),
            RecoveryActionOutcome::FailedClosed {
                reason: "checkpoint drift".into(),
            },
            None,
            VerificationState::Failed,
        );
        assert!(record.verify());
        if let RecoveryActionOutcome::FailedClosed { reason } = &mut record.outcome {
            reason.push_str(" altered");
        }
        assert!(!record.verify());
    }

    #[test]
    fn canonical_enum_ids_are_explicit_and_versioned() {
        assert_eq!(
            recovery_operation_id(RecoveryOperation::RestoreCheckpoint),
            "recovery-operation/restore-checkpoint/v1"
        );
        assert_eq!(
            verification_state_id(VerificationState::Incomplete),
            "verification-state/incomplete/v1"
        );
    }

    #[test]
    fn tampering_is_detected() {
        let mut record = RecoveryAuditRecord::new(
            1_700_000_000_000,
            &action(),
            preflight(),
            RecoveryActionOutcome::Succeeded,
            Some("b".repeat(64)),
            VerificationState::Verified,
        );
        record.authorization_reason.push_str(" altered");
        assert!(!record.verify());
    }
}
