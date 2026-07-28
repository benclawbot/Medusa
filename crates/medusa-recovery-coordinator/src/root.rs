#[path = "action.rs"]
pub mod action;
#[path = "audit.rs"]
pub mod audit;
#[path = "lib.rs"]
mod legacy;
#[path = "preflight.rs"]
pub mod preflight;
#[path = "service.rs"]
pub mod service;
#[path = "store.rs"]
pub mod store;

pub use action::{AuthorizedRecoveryAction, RecoveryActionRejection, RecoveryActionRequest};
pub use audit::{RecoveryActionOutcome, RecoveryAuditRecord, RecoveryPreflightEvidence};
pub use legacy::*;
pub use preflight::{
    RecoveryPreflightError, RecoveryPreflightReport, RepositoryFileState, RepositorySnapshot,
    build_restore_preflight, snapshot_fingerprint,
};
pub use service::{
    RecoveryActionExecutor, RecoveryActionService, RecoveryExecutionError,
    RecoveryExecutionOutcome, RecoveryExecutionReceipt,
};
pub use store::{RecoveryAuditStore, RecoveryAuditStoreError};
