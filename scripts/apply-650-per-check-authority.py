from pathlib import Path
import re

verification = Path('crates/medusa-agent/src/verification.rs')
text = verification.read_text()
text = text.replace(
    'pub fn targeted_verification(repo: &Path) -> MedusaResult<VerificationResult> {',
    'fn targeted_verification(repo: &Path) -> MedusaResult<VerificationResult> {',
    1,
)
helper_anchor = '''fn run_supervised_command<S: AsRef<std::ffi::OsStr>>(
'''
helper = '''#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExecutedVerificationCommand {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub passed: bool,
}

pub(crate) fn execute_verification_command(
    repo: &Path,
    program: &str,
    args: &[String],
) -> MedusaResult<ExecutedVerificationCommand> {
    let program = platform_program(program);
    let output = run_supervised_command(repo, program, args, VERIFICATION_TIMEOUT)?;
    Ok(ExecutedVerificationCommand {
        exit_code: output.status.code(),
        timed_out: output.timed_out,
        duration_ms: output.duration.as_millis() as u64,
        stdout: output.stdout,
        stderr: output.stderr,
        passed: output.status.success() && !output.timed_out,
    })
}

pub(crate) fn required_browser_verification(repo: &Path) -> MedusaResult<VerificationResult> {
    let mut result = VerificationResult {
        passed: true,
        evidence: Vec::new(),
    };
    append_browser_verification(repo, BrowserVerificationDecision::Run, &mut result)?;
    Ok(result)
}

'''
if text.count(helper_anchor) != 1:
    raise SystemExit('verification command helper anchor missing')
text = text.replace(helper_anchor, helper + helper_anchor, 1)
accessibility_anchor = '''    match client.request(BrowserRequest::Screenshot { full_page: true })? {
'''
accessibility = '''    let accessibility = client.request(BrowserRequest::Evaluate {
        expression: r#"JSON.stringify({missing_alt:document.querySelectorAll('img:not([alt])').length,unlabeled_controls:Array.from(document.querySelectorAll('button,input,select,textarea,a[href]')).filter((element)=>!(element.getAttribute('aria-label')||element.getAttribute('aria-labelledby')||element.textContent?.trim()||element.getAttribute('title'))).length})"#.to_owned(),
    })?;
    match accessibility {
        BrowserResponse::Evaluate { value } => {
            let report = value
                .as_str()
                .and_then(|serialized| serde_json::from_str::<serde_json::Value>(serialized).ok())
                .unwrap_or_else(|| value.clone());
            let missing_alt = report
                .get("missing_alt")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(u64::MAX);
            let unlabeled_controls = report
                .get("unlabeled_controls")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(u64::MAX);
            let clean = missing_alt == 0 && unlabeled_controls == 0;
            result.evidence.push(format!(
                "browser_accessibility=missing_alt:{missing_alt},unlabeled_controls:{unlabeled_controls}"
            ));
            result.passed &= clean;
        }
        BrowserResponse::Error { code, message } => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_accessibility_error={code}:{message}"));
        }
        other => {
            result.passed = false;
            result
                .evidence
                .push(format!("browser_unexpected_accessibility={other:?}"));
        }
    }

'''
if text.count(accessibility_anchor) != 1:
    raise SystemExit('browser accessibility anchor missing')
text = text.replace(accessibility_anchor, accessibility + accessibility_anchor, 1)
verification.write_text(text)

planner = Path('crates/medusa-evidence/src/verification.rs')
text = planner.read_text()
old = '''    let path = repo.join(".medusa/verification.json");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let definition: Definition = serde_json::from_slice(&fs::read(path)?)?;
    definition
        .checks
        .into_iter()
        .map(|check| {
            if check.program.trim().is_empty() || check.reason.trim().is_empty() {
                return Err(EvidenceError::Validation(
                    "repository-defined check requires program and reason".to_owned(),
                ));
            }
            let args = check.args.iter().map(String::as_str).collect::<Vec<_>>();
            Ok(VerificationCheck::command(
                check.kind,
                &check.program,
                &args,
                check.working_directory,
                check.reason,
            ))
        })
        .collect()
'''
new = '''    let mut checks = Vec::new();
    #[cfg(windows)]
    if repo.join("verify.ps1").is_file() {
        checks.push(VerificationCheck::command(
            VerificationCheckKind::RepositoryDefined,
            "powershell",
            &["-NoProfile", "-File", "verify.ps1"],
            ".",
            "repository verification script",
        ));
    }
    #[cfg(not(windows))]
    if repo.join("verify.sh").is_file() {
        checks.push(VerificationCheck::command(
            VerificationCheckKind::RepositoryDefined,
            "bash",
            &["verify.sh"],
            ".",
            "repository verification script",
        ));
    }
    let path = repo.join(".medusa/verification.json");
    if !path.is_file() {
        return Ok(checks);
    }
    let definition: Definition = serde_json::from_slice(&fs::read(path)?)?;
    let configured = definition
        .checks
        .into_iter()
        .map(|check| {
            if check.program.trim().is_empty() || check.reason.trim().is_empty() {
                return Err(EvidenceError::Validation(
                    "repository-defined check requires program and reason".to_owned(),
                ));
            }
            let args = check.args.iter().map(String::as_str).collect::<Vec<_>>();
            Ok(VerificationCheck::command(
                check.kind,
                &check.program,
                &args,
                check.working_directory,
                check.reason,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    checks.extend(configured);
    Ok(checks)
'''
if text.count(old) != 1:
    raise SystemExit('repository adapter anchor missing')
planner.write_text(text.replace(old, new, 1))

lib = Path('crates/medusa-agent/src/lib.rs')
text = lib.read_text()
text = text.replace(
    'pub use verification::{VerificationResult, targeted_verification};\n',
    'pub use verification::VerificationResult;\n',
    1,
)
text = text.replace(
    'pub use verification_authority::{AuthoritativeVerificationResult, authoritative_verification_for_components};\n',
    'pub use verification_authority::{AuthoritativeVerificationResult, authoritative_verification_for_components, authoritative_verification_for_components_at};\n',
    1,
)
lib.write_text(text)

runtime = Path('crates/medusa-runtime/src/mutating_worker_coordinator.rs')
text = runtime.read_text()
text = text.replace(
    'authoritative_verification_for_components,\n',
    'authoritative_verification_for_components_at,\n',
    1,
)
old_call = '''        let verification = match authoritative_verification_for_components(
            &worker.worktree,
            &preflight.repository_fingerprint,
            &worktree_identity,
            &changed_components,
        ) {
'''
new_call = '''        let evidence_root = state_path
            .parent()
            .ok_or_else(|| "implementation state path has no execution root".to_owned())?
            .join("evidence/worktree");
        let verification = match authoritative_verification_for_components_at(
            &worker.worktree,
            &evidence_root,
            &preflight.repository_fingerprint,
            &worktree_identity,
            &changed_components,
        ) {
'''
if text.count(old_call) != 1:
    raise SystemExit('worktree authority call anchor missing')
runtime.write_text(text.replace(old_call, new_call, 1))

transaction = Path('crates/medusa-runtime/src/mutation_transaction.rs')
text = transaction.read_text()
text = text.replace(
    'use medusa_agent::{AgentSession, authoritative_verification_for_components};\n',
    'use medusa_agent::{AgentSession, authoritative_verification_for_components, authoritative_verification_for_components_at};\n',
    1,
)
old_call = '''        let verification = authoritative_verification_for_components(
            &verification_root,
            &self.state.repository_fingerprint,
            &self.state.prepared_commit,
            &components,
        );
'''
new_call = '''        let verification = authoritative_verification_for_components_at(
            &verification_root,
            &self.root.join("evidence/independent"),
            &self.state.repository_fingerprint,
            &self.state.prepared_commit,
            &components,
        );
'''
if text.count(old_call) != 1:
    raise SystemExit('independent authority call anchor missing')
transaction.write_text(text.replace(old_call, new_call, 1))

authority = Path('crates/medusa-agent/src/verification_authority.rs')
authority.write_text(r'''use std::{
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
        format!("command_program={}", check.program.as_deref().unwrap_or_default()),
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
''')
