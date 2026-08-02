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
    VerificationCheck, VerificationCheckKind, VerificationCheckReceipt, VerificationPlanner,
    VerificationReceipt, VerificationStatus, validate_artifact_semantics,
};
use sha2::{Digest, Sha256};

use crate::verification::{
    ExecutedVerificationCommand, VerificationResult, execute_verification_command,
    required_browser_verification,
};

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

pub fn authoritative_verification_for_components(
    repo: &Path,
    repository_fingerprint: &str,
    commit: &str,
    components: &[ChangedComponent],
) -> MedusaResult<AuthoritativeVerificationResult> {
    authoritative_verification_for_components_at(
        repo,
        &repo.join(".medusa/evidence"),
        repository_fingerprint,
        commit,
        components,
    )
}

pub fn authoritative_verification_for_components_at(
    repo: &Path,
    evidence_root: &Path,
    repository_fingerprint: &str,
    commit: &str,
    components: &[ChangedComponent],
) -> MedusaResult<AuthoritativeVerificationResult> {
    if repository_fingerprint.trim().is_empty() || commit.trim().is_empty() {
        return Err(invalid(
            "authoritative verification requires repository and commit identity",
        ));
    }
    let plan = VerificationPlanner::plan(repo, repository_fingerprint, commit, components, &[])
        .map_err(evidence_error)?;
    let store_root = evidence_root.join(short_hash(&(repository_fingerprint.to_owned() + commit)));
    let store = ArtifactStore::open(&store_root).map_err(evidence_error)?;
    let mut artifacts = BTreeMap::<ArtifactId, ArtifactMetadata>::new();
    let mut reads = Vec::<ArtifactReadReceipt>::new();
    let mut commands = Vec::<CommandReceipt>::new();
    let mut records = Vec::<EvidenceRecord>::new();
    let mut checks = Vec::with_capacity(plan.checks.len());
    let mut browser_material: Option<CheckMaterial> = None;

    for check in &plan.checks {
        let material = match check.kind {
            VerificationCheckKind::ArtifactSemantic => semantic_material(
                repo,
                repository_fingerprint,
                commit,
                &plan.components,
                &store,
                &mut artifacts,
                &mut reads,
                &mut records,
            )?,
            VerificationCheckKind::BrowserBehavior | VerificationCheckKind::Accessibility => {
                if let Some(material) = browser_material.clone() {
                    material
                } else {
                    let material = browser_material_for(
                        repo,
                        repository_fingerprint,
                        commit,
                        &store,
                        &mut artifacts,
                        &mut reads,
                        &mut records,
                    )?;
                    browser_material = Some(material.clone());
                    material
                }
            }
            _ => command_material(
                repo,
                repository_fingerprint,
                commit,
                check,
                &store,
                &mut artifacts,
                &mut reads,
                &mut commands,
                &mut records,
            )?,
        };
        let mut details = material.details;
        details.push(format!("planner_reason={}", check.reason));
        checks.push(VerificationCheckReceipt::new(
            check,
            material.passed,
            material.command,
            material.evidence_ids,
            material.artifact_ids,
            details,
        ));
    }

    let mut bundle = EvidenceBundle::new(repository_fingerprint, commit);
    bundle.artifacts = artifacts.into_values().collect();
    bundle.reads = reads;
    bundle.commands = commands;
    bundle.records = records;
    bundle.refresh();
    let receipt = VerificationReceipt::new(plan, checks, bundle).map_err(evidence_error)?;
    let summary = receipt.summary_lines();
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

#[allow(clippy::too_many_arguments)]
fn command_material(
    repo: &Path,
    repository_fingerprint: &str,
    commit: &str,
    check: &VerificationCheck,
    store: &ArtifactStore,
    artifacts: &mut BTreeMap<ArtifactId, ArtifactMetadata>,
    reads: &mut Vec<ArtifactReadReceipt>,
    commands: &mut Vec<CommandReceipt>,
    records: &mut Vec<EvidenceRecord>,
) -> MedusaResult<CheckMaterial> {
    let Some(program) = check.program.as_deref() else {
        return Ok(rejected_material(format!(
            "verification check {} has no executable adapter",
            check.id
        )));
    };
    let working_directory = if check.working_directory == "." {
        repo.to_path_buf()
    } else {
        repo.join(&check.working_directory)
    };
    let executed = match execute_verification_command(&working_directory, program, &check.args) {
        Ok(executed) => executed,
        Err(error) => return Ok(rejected_material(error.to_string())),
    };
    materialize_command(
        repository_fingerprint,
        commit,
        check,
        executed,
        store,
        artifacts,
        reads,
        commands,
        records,
    )
}

#[allow(clippy::too_many_arguments)]
fn materialize_command(
    repository_fingerprint: &str,
    commit: &str,
    check: &VerificationCheck,
    executed: ExecutedVerificationCommand,
    store: &ArtifactStore,
    artifacts: &mut BTreeMap<ArtifactId, ArtifactMetadata>,
    reads: &mut Vec<ArtifactReadReceipt>,
    commands: &mut Vec<CommandReceipt>,
    records: &mut Vec<EvidenceRecord>,
) -> MedusaResult<CheckMaterial> {
    let (stdout_id, stdout_source) = store_bytes(
        store,
        "text/plain; charset=utf-8",
        "medusa-verification-command-stdout",
        &executed.stdout,
        "medusa-verification-command-review",
        artifacts,
        reads,
    )?;
    let (stderr_id, stderr_source) = store_bytes(
        store,
        "text/plain; charset=utf-8",
        "medusa-verification-command-stderr",
        &executed.stderr,
        "medusa-verification-command-review",
        artifacts,
        reads,
    )?;
    let command = CommandReceipt::new(
        check,
        executed.exit_code,
        executed.timed_out,
        executed.duration_ms,
        stdout_id.clone(),
        stderr_id.clone(),
    );
    let mut sources = vec![EvidenceSource::CommandReceipt {
        receipt_id: command.id.clone(),
    }];
    sources.extend(stdout_source);
    sources.extend(stderr_source);
    let record = EvidenceRecord::new(
        EvidenceKind::Observation,
        format!("verification command {} completed", check.id),
        repository_fingerprint,
        commit,
        "medusa-verification-command-runner",
        if command.passed {
            VerificationStatus::Verified
        } else {
            VerificationStatus::Rejected
        },
        sources,
    )
    .map_err(evidence_error)?;
    let details = vec![
        format!(
            "command_program={}",
            check.program.as_deref().unwrap_or_default()
        ),
        format!("command_args={}", check.args.join(" ")),
        format!("command_working_directory={}", check.working_directory),
        format!("command_exit_code={:?}", command.exit_code),
        format!("command_timed_out={}", command.timed_out),
        format!("command_duration_ms={}", command.duration_ms),
    ];
    let material = CheckMaterial {
        passed: command.passed,
        command: Some(command.clone()),
        evidence_ids: vec![record.id.clone()],
        artifact_ids: vec![stdout_id, stderr_id],
        details,
    };
    commands.push(command);
    records.push(record);
    Ok(material)
}

#[allow(clippy::too_many_arguments)]
fn browser_material_for(
    repo: &Path,
    repository_fingerprint: &str,
    commit: &str,
    store: &ArtifactStore,
    artifacts: &mut BTreeMap<ArtifactId, ArtifactMetadata>,
    reads: &mut Vec<ArtifactReadReceipt>,
    records: &mut Vec<EvidenceRecord>,
) -> MedusaResult<CheckMaterial> {
    let result = match required_browser_verification(repo) {
        Ok(result) => result,
        Err(error) => return Ok(rejected_material(error.to_string())),
    };
    let evidence_bytes = result.evidence.join("\n").into_bytes();
    let (evidence_id, source) = store_bytes(
        store,
        "text/plain; charset=utf-8",
        "medusa-browser-verification",
        &evidence_bytes,
        "medusa-browser-verification-review",
        artifacts,
        reads,
    )?;
    let mut artifact_ids = vec![evidence_id];
    for line in &result.evidence {
        let Some(path) = line.strip_prefix("browser_screenshot=") else {
            continue;
        };
        let screenshot = PathBuf::from(path);
        if screenshot.is_file() {
            let bytes = fs::read(&screenshot)?;
            let (id, _) = store_bytes(
                store,
                media_type_for_path(&screenshot),
                "medusa-browser-screenshot",
                &bytes,
                "medusa-browser-screenshot-review",
                artifacts,
                reads,
            )?;
            artifact_ids.push(id);
        }
    }
    let record = EvidenceRecord::new(
        EvidenceKind::Observation,
        "required browser and accessibility verification completed",
        repository_fingerprint,
        commit,
        "medusa-browser-verifier",
        if result.passed {
            VerificationStatus::Verified
        } else {
            VerificationStatus::Rejected
        },
        source.into_iter().collect(),
    )
    .map_err(evidence_error)?;
    let material = CheckMaterial {
        passed: result.passed,
        command: None,
        evidence_ids: vec![record.id.clone()],
        artifact_ids,
        details: result.evidence,
    };
    records.push(record);
    Ok(material)
}

#[allow(clippy::too_many_arguments)]
fn semantic_material(
    repo: &Path,
    repository_fingerprint: &str,
    commit: &str,
    components: &[ChangedComponent],
    store: &ArtifactStore,
    artifacts: &mut BTreeMap<ArtifactId, ArtifactMetadata>,
    reads: &mut Vec<ArtifactReadReceipt>,
    records: &mut Vec<EvidenceRecord>,
) -> MedusaResult<CheckMaterial> {
    let mut passed = true;
    let mut evidence_ids = Vec::new();
    let mut artifact_ids = Vec::new();
    let mut details = Vec::new();
    for component in components {
        if component.kind == ChangeKind::Deleted || !requires_semantic_artifact(component) {
            continue;
        }
        let path = repo.join(&component.path);
        let result = validate_artifact_semantics(&path).map_err(evidence_error)?;
        passed &= result.passed;
        details.push(format!(
            "artifact_semantics={}:class={:?}:passed={}",
            component.path, result.class, result.passed
        ));
        details.extend(result.details.iter().cloned());
        if path.is_file() {
            let bytes = fs::read(&path)?;
            let (artifact_id, source) = store_bytes(
                store,
                media_type_for_path(&path),
                "medusa-artifact-semantic-validator",
                &bytes,
                "medusa-artifact-semantic-review",
                artifacts,
                reads,
            )?;
            let record = EvidenceRecord::new(
                EvidenceKind::Observation,
                format!("semantic artifact validation for {}", component.path),
                repository_fingerprint,
                commit,
                "medusa-artifact-semantic-validator",
                if result.passed {
                    VerificationStatus::Verified
                } else {
                    VerificationStatus::Rejected
                },
                source.into_iter().collect(),
            )
            .map_err(evidence_error)?;
            artifact_ids.push(artifact_id);
            evidence_ids.push(record.id.clone());
            records.push(record);
        }
    }
    Ok(CheckMaterial {
        passed,
        command: None,
        evidence_ids,
        artifact_ids,
        details,
    })
}

#[allow(clippy::too_many_arguments)]
fn store_bytes(
    store: &ArtifactStore,
    media_type: &str,
    producer: &str,
    bytes: &[u8],
    reader: &str,
    artifacts: &mut BTreeMap<ArtifactId, ArtifactMetadata>,
    reads: &mut Vec<ArtifactReadReceipt>,
) -> MedusaResult<(ArtifactId, Option<EvidenceSource>)> {
    let metadata = store
        .put_bytes(media_type, producer, bytes)
        .map_err(evidence_error)?;
    let id = metadata.id.clone();
    artifacts.insert(id.clone(), metadata.clone());
    if metadata.byte_len == 0 {
        return Ok((id, None));
    }
    let (_, read) = store
        .read_range(&id, 0, metadata.byte_len, reader)
        .map_err(evidence_error)?;
    let source = EvidenceSource::ArtifactRange {
        artifact_id: id.clone(),
        read_receipt_id: read.id.clone(),
        offset: read.offset,
        length: read.length,
        content_hash: read.content_hash.clone(),
    };
    reads.push(read);
    Ok((id, Some(source)))
}

fn rejected_material(reason: String) -> CheckMaterial {
    CheckMaterial {
        passed: false,
        command: None,
        evidence_ids: Vec::new(),
        artifact_ids: Vec::new(),
        details: vec![format!("verification_error={reason}")],
    }
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
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
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

    #[test]
    fn each_planned_command_has_typed_outputs_and_receipt() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join(".medusa")).unwrap();
        fs::write(directory.path().join("file.txt"), "changed\n").unwrap();
        fs::write(
            directory.path().join(".medusa/verification.json"),
            r#"{"checks":[{"kind":"repository_defined","program":"rustc","args":["--version"],"working_directory":".","reason":"prove command receipts"}]}"#,
        )
        .unwrap();
        let component = ChangedComponent::new(ChangeKind::Modified, "file.txt").unwrap();
        let evidence_root = directory.path().join("durable-evidence");
        let result = authoritative_verification_for_components_at(
            directory.path(),
            &evidence_root,
            "repo",
            "commit",
            &[component],
        )
        .unwrap();
        assert!(result.receipt.passed);
        assert_eq!(result.receipt.evidence.commands.len(), 1);
        assert!(result.receipt.checks[0].command.is_some());
        assert!(result.receipt.evidence.artifacts.len() >= 2);
        assert!(evidence_root.exists());
    }
}
