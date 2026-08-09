use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_evidence::{
    ArtifactId, ArtifactMetadata, ArtifactReadReceipt, ArtifactStore, ChangeKind, ChangedComponent,
    CommandReceipt, EvidenceBundle, EvidenceKind, EvidenceRecord, EvidenceSource,
    VerificationCheck, VerificationCheckKind, VerificationCheckReceipt, VerificationPlan,
    VerificationPlanner, VerificationReceipt, VerificationStatus, validate_artifact_semantics,
};
use medusa_intelligence::{RepositoryGraph, RepositoryGraphFreshness, ReviewImpact};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const COMMAND_PREVIEW_MAX_BYTES: usize = 4 * 1024;
const COMMAND_PREVIEW_MAX_LINES: usize = 32;

#[path = "verification_schedule.rs"]
mod verification_schedule;

use crate::{
    verification::{
        ExecutedVerificationCommand, VerificationResult, execute_verification_command,
        required_browser_verification,
    },
    verification_dag::{VerificationDag, VerificationReceipt as DagReceipt},
};
use verification_schedule::{dag_for_plan, execute_command_wave};

#[derive(Clone, Debug)]
pub struct AuthoritativeVerificationResult {
    pub receipt: VerificationReceipt,
    pub summary: Vec<String>,
}

#[derive(Clone)]
struct CheckMaterial {
    passed: bool,
    command: Option<CommandReceipt>,
    evidence_ids: Vec<medusa_evidence::EvidenceId>,
    artifact_ids: Vec<ArtifactId>,
    details: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PersistedVerificationDag {
    schema_version: u16,
    verification_receipt_fingerprint: String,
    repository_state_fingerprint: String,
    dag: VerificationDag,
    fingerprint: String,
}

impl PersistedVerificationDag {
    fn new(
        dag: VerificationDag,
        verification_receipt_fingerprint: &str,
        repository_state_fingerprint: &str,
    ) -> MedusaResult<Self> {
        let mut persisted = Self {
            schema_version: 1,
            verification_receipt_fingerprint: verification_receipt_fingerprint.to_owned(),
            repository_state_fingerprint: repository_state_fingerprint.to_owned(),
            dag,
            fingerprint: String::new(),
        };
        persisted.fingerprint = persisted_verification_dag_fingerprint(&persisted)?;
        Ok(persisted)
    }

    fn validate(&self) -> MedusaResult<()> {
        if self.schema_version != 1
            || self.verification_receipt_fingerprint.trim().is_empty()
            || self.repository_state_fingerprint.trim().is_empty()
            || self.fingerprint != persisted_verification_dag_fingerprint(self)?
        {
            return Err(invalid("persisted verification DAG is stale or corrupted"));
        }
        Ok(())
    }
}

pub fn prepare_components_for_verification(
    repo: &Path,
    components: &[ChangedComponent],
) -> MedusaResult<()> {
    let mut rust_paths = components
        .iter()
        .filter(|component| component.kind != ChangeKind::Deleted)
        .map(|component| component.path.as_str())
        .filter(|path| Path::new(path).extension().and_then(|value| value.to_str()) == Some("rs"))
        .filter(|path| repo.join(path).is_file())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    rust_paths.sort();
    rust_paths.dedup();
    if rust_paths.is_empty() {
        return Ok(());
    }
    let edition = rust_edition(repo);
    let mut args = vec!["--edition".to_owned(), edition.to_owned()];
    args.extend(rust_paths.iter().cloned());
    let result = execute_verification_command(repo, "rustfmt", &args)?;
    if !result.passed {
        let stdout = bounded_failure_text(&result.stdout);
        let stderr = bounded_failure_text(&result.stderr);
        return Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!(
                "trusted rustfmt preparation failed for {}: stdout={stdout}; stderr={stderr}",
                rust_paths.join(",")
            ),
        ));
    }
    Ok(())
}
