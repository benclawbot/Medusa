use std::{collections::BTreeMap, fs, path::Path, process::Command};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_evidence::{
    ArtifactId, ArtifactMetadata, ArtifactReadReceipt, ArtifactStore, ChangeKind, ChangedComponent,
    EvidenceBundle, EvidenceKind, EvidenceRecord, EvidenceSource, VerificationCheckKind,
    VerificationCheckReceipt, VerificationPlanner, VerificationReceipt, VerificationStatus,
    validate_artifact_semantics,
};
use sha2::{Digest, Sha256};

use crate::verification::{VerificationResult, targeted_verification_for_paths};

#[derive(Clone, Debug)]
pub struct AuthoritativeVerificationResult {
    pub receipt: VerificationReceipt,
    pub summary: Vec<String>,
}

pub fn authoritative_verification_for_components(
    repo: &Path,
    repository_fingerprint: &str,
    commit: &str,
    components: &[ChangedComponent],
) -> MedusaResult<AuthoritativeVerificationResult> {
    if repository_fingerprint.trim().is_empty() || commit.trim().is_empty() {
        return Err(invalid("authoritative verification requires repository and commit identity"));
    }
    let plan = VerificationPlanner::plan(
        repo,
        repository_fingerprint,
        commit,
        components,
        &[],
    )
    .map_err(evidence_error)?;
    let changed_paths = plan
        .components
        .iter()
        .map(|component| component.path.clone())
        .collect::<Vec<_>>();
    let legacy = targeted_verification_for_paths(repo, &changed_paths)?;
    let store_root = repo
        .join(".medusa/evidence")
        .join(short_hash(&(repository_fingerprint.to_owned() + commit)));
    let store = ArtifactStore::open(&store_root).map_err(evidence_error)?;

    let mut artifacts = BTreeMap::<ArtifactId, ArtifactMetadata>::new();
    let mut reads = Vec::<ArtifactReadReceipt>::new();
    let mut records = Vec::<EvidenceRecord>::new();

    let legacy_bytes = legacy.evidence.join("\n").into_bytes();
    let legacy_artifact = store
        .put_bytes("text/plain; charset=utf-8", "medusa-agent-targeted-verification", &legacy_bytes)
        .map_err(evidence_error)?;
    let (_, legacy_read) = store
        .read_range(
            &legacy_artifact.id,
            0,
            legacy_artifact.byte_len.max(1).min(legacy_artifact.byte_len),
            "medusa-agent-verification-authority",
        )
        .map_err(evidence_error)?;
    let legacy_record = EvidenceRecord::new(
        EvidenceKind::Observation,
        "repository-targeted verification produced bounded output",
        repository_fingerprint,
        commit,
        "medusa-agent-verification-authority",
        if legacy.passed {
            VerificationStatus::Verified
        } else {
            VerificationStatus::Rejected
        },
        vec![EvidenceSource::ArtifactRange {
            artifact_id: legacy_artifact.id.clone(),
            read_receipt_id: legacy_read.id.clone(),
            offset: legacy_read.offset,
            length: legacy_read.length,
            content_hash: legacy_read.content_hash.clone(),
        }],
    )
    .map_err(evidence_error)?;
    artifacts.insert(legacy_artifact.id.clone(), legacy_artifact.clone());
    reads.push(legacy_read);
    records.push(legacy_record.clone());

    let mut semantic_passed = true;
    let mut semantic_ids = Vec::new();
    let mut semantic_artifacts = Vec::new();
    let mut semantic_details = Vec::new();
    for component in &plan.components {
        if component.kind == ChangeKind::Deleted || !requires_semantic_artifact(component) {
            continue;
        }
        let path = repo.join(&component.path);
        let result = validate_artifact_semantics(&path).map_err(evidence_error)?;
        semantic_passed &= result.passed;
        semantic_details.push(format!(
            "artifact_semantics={}:class={:?}:passed={}",
            component.path, result.class, result.passed
        ));
        semantic_details.extend(result.details.iter().cloned());
        if path.is_file() {
            let bytes = fs::read(&path)?;
            let metadata = store
                .put_bytes(media_type_for_path(&path), "medusa-agent-artifact-validator", &bytes)
                .map_err(evidence_error)?;
            let (_, read) = store
                .read_page(&metadata.id, 0, "medusa-agent-artifact-validator")
                .map_err(evidence_error)?;
            let record = EvidenceRecord::new(
                EvidenceKind::Observation,
                format!("semantic artifact validation for {}", component.path),
                repository_fingerprint,
                commit,
                "medusa-agent-artifact-validator",
                if result.passed {
                    VerificationStatus::Verified
                } else {
                    VerificationStatus::Rejected
                },
                vec![EvidenceSource::ArtifactRange {
                    artifact_id: metadata.id.clone(),
                    read_receipt_id: read.id.clone(),
                    offset: read.offset,
                    length: read.length,
                    content_hash: read.content_hash.clone(),
                }],
            )
            .map_err(evidence_error)?;
            semantic_ids.push(record.id.clone());
            semantic_artifacts.push(metadata.id.clone());
            artifacts.insert(metadata.id.clone(), metadata);
            reads.push(read);
            records.push(record);
        }
    }

    let browser_passed = !plan.components.iter().any(|component| component.effective_ui)
        || legacy
            .evidence
            .iter()
            .any(|line| line == "browser_result=passed");
    let mut checks = Vec::with_capacity(plan.checks.len());
    for check in &plan.checks {
        let (passed, evidence_ids, artifact_ids, mut details) = match check.kind {
            VerificationCheckKind::ArtifactSemantic => (
                semantic_passed,
                semantic_ids.clone(),
                semantic_artifacts.clone(),
                semantic_details.clone(),
            ),
            VerificationCheckKind::BrowserBehavior | VerificationCheckKind::Accessibility => (
                legacy.passed && browser_passed,
                vec![legacy_record.id.clone()],
                vec![legacy_artifact.id.clone()],
                vec![format!("browser_behavior_passed={browser_passed}")],
            ),
            _ => (
                legacy.passed,
                vec![legacy_record.id.clone()],
                vec![legacy_artifact.id.clone()],
                Vec::new(),
            ),
        };
        details.push(format!("planner_reason={}", check.reason));
        checks.push(VerificationCheckReceipt::new(
            check,
            passed,
            None,
            evidence_ids,
            artifact_ids,
            details,
        ));
    }

    let mut bundle = EvidenceBundle::new(repository_fingerprint, commit);
    bundle.artifacts = artifacts.into_values().collect();
    bundle.reads = reads;
    bundle.records = records;
    bundle.refresh();
    let receipt = VerificationReceipt::new(plan, checks, bundle).map_err(evidence_error)?;
    let mut summary = receipt.summary_lines();
    summary.extend(legacy.evidence);
    fs::create_dir_all(&store_root)?;
    fs::write(
        store_root.join("verification-receipt.json"),
        serde_json::to_vec_pretty(&receipt).map_err(|error| {
            MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                error.to_string(),
            )
        })?,
    )?;
    Ok(AuthoritativeVerificationResult { receipt, summary })
}

pub(crate) fn authoritative_verification_for_paths(
    repo: &Path,
    paths: &[String],
) -> MedusaResult<VerificationResult> {
    let components = paths
        .iter()
        .map(|path| ChangedComponent::new(ChangeKind::Modified, path.clone()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(evidence_error)?;
    let commit = git_stdout(repo, &["rev-parse", "HEAD"]).unwrap_or_else(|| "worktree".to_owned());
    let repository_fingerprint = short_hash(&format!("{}:{commit}", repo.display()));
    let result = authoritative_verification_for_components(
        repo,
        &repository_fingerprint,
        &commit,
        &components,
    )?;
    Ok(VerificationResult {
        passed: result.receipt.passed,
        evidence: result.summary,
    })
}

fn requires_semantic_artifact(component: &ChangedComponent) -> bool {
    component.generated
        || Path::new(&component.path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "html" | "json" | "png" | "pdf" | "zip" | "jar" | "docx" | "xlsx" | "pptx"
                )
            })
}

fn media_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "pdf" => "application/pdf",
        "zip" | "jar" | "docx" | "xlsx" | "pptx" => "application/zip",
        _ => "application/octet-stream",
    }
}

fn git_stdout(repo: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .to_owned()
    })
}

fn short_hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn evidence_error(error: medusa_evidence::EvidenceError) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        error.to_string(),
    )
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_nonempty_json_fails_authoritative_receipt() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("verify.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::write(directory.path().join("artifact.json"), "{broken}").unwrap();
        let component = ChangedComponent::new(ChangeKind::Added, "artifact.json").unwrap();
        let result = authoritative_verification_for_components(
            directory.path(),
            "repo",
            "commit",
            &[component],
        )
        .unwrap();
        assert!(!result.receipt.passed);
        assert!(result.receipt.validate().is_ok());
    }
}
