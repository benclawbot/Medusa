#[path = "action_history.rs"]
mod action_history;
#[path = "model.rs"]
mod model;
#[path = "snapshot_builder.rs"]
mod snapshot_builder;
#[allow(unused_imports)]
#[path = "verification_evidence.rs"]
mod verification_evidence;

pub use action_history::{
    ReviewAuditDecision, ReviewAuditError, ReviewAuditEvent, ReviewAuditScope,
    record_authorized_action,
};
pub use model::*;
pub use snapshot_builder::{
    ChangeEvidence, HunkEvidence, ReviewSnapshotBuildError, ReviewSnapshotInput,
    build_review_snapshot,
};
pub use verification_evidence::{
    DiagnosticSeverity, FileVerificationSummary, VerificationDiagnostic, VerificationEvidence,
    VerificationEvidenceError, VerificationOutcome, VerificationReport,
    associate_verification_evidence,
};
