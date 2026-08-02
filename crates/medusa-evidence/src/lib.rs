//! Typed source-bound evidence, durable artifacts, exact change scope, and verification authority.

mod artifact;
mod change;
mod evidence;
mod verification;

pub use artifact::{
    ArtifactId, ArtifactMetadata, ArtifactReadReceipt, ArtifactSearchHit, ArtifactStore,
};
pub use change::{
    ChangeKind, ChangedComponent, changed_scope_fingerprint, is_effective_ui_path,
    is_generated_path, normalize_components,
};
pub use evidence::{
    EvidenceBundle, EvidenceDependency, EvidenceId, EvidenceKind, EvidenceRecord, EvidenceSource,
    VerificationStatus,
};
pub use verification::{
    ArtifactSemanticClass, ArtifactSemanticResult, CommandReceipt, VerificationCheck,
    VerificationCheckKind, VerificationCheckReceipt, VerificationExemption, VerificationPlan,
    VerificationPlanner, VerificationReceipt, validate_artifact_semantics,
};

use serde::Serialize;
use sha2::{Digest, Sha256};

pub const SCHEMA_VERSION: u16 = 1;
pub const DEFAULT_PAGE_SIZE: u64 = 16 * 1024;
pub const MAX_SEARCH_HITS: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum EvidenceError {
    #[error("evidence validation failed: {0}")]
    Validation(String),
    #[error("evidence resource not found: {0}")]
    NotFound(String),
    #[error("evidence storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("evidence serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, EvidenceError>;

pub(crate) fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub(crate) fn fingerprint(value: &impl Serialize) -> String {
    hash_bytes(&serde_json::to_vec(value).expect("serializable evidence fingerprint input"))
}

pub(crate) fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| EvidenceError::Validation("evidence path has no parent".to_owned()))?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("tmp-{}", ulid::Ulid::new()));
    std::fs::write(&temporary, bytes)?;
    #[cfg(windows)]
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(temporary, path)?;
    Ok(())
}

pub(crate) fn write_json_atomic(path: &std::path::Path, value: &impl Serialize) -> Result<()> {
    write_atomic(path, &serde_json::to_vec_pretty(value)?)
}
