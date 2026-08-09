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

pub(crate) fn prepare_paths_for_verification(repo: &Path, paths: &[String]) -> MedusaResult<()> {
    let components = paths
        .iter()
        .map(|path| ChangedComponent::new(ChangeKind::Modified, path.clone()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(evidence_error)?;
    prepare_components_for_verification(repo, &components)
}

fn rust_edition(repo: &Path) -> &'static str {
    let manifest = fs::read_to_string(repo.join("Cargo.toml")).unwrap_or_default();
    for edition in ["2024", "2021", "2018"] {
        if manifest.contains(&format!("edition = \"{edition}\""))
            || manifest.contains(&format!("edition=\"{edition}\""))
        {
            return edition;
        }
    }
    "2021"
}

fn bounded_failure_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    truncate_utf8(text.trim(), 2048)
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
    let graph_details = graph_verification_assessment(repo, commit, &plan)?;
    let store_root = evidence_root.join(short_hash(&(repository_fingerprint.to_owned() + commit)));
    let store = ArtifactStore::open(&store_root).map_err(evidence_error)?;
    let mut artifacts = BTreeMap::<ArtifactId, ArtifactMetadata>::new();
    let mut reads = Vec::<ArtifactReadReceipt>::new();
    let mut commands = Vec::<CommandReceipt>::new();
    let mut records = Vec::<EvidenceRecord>::new();
    let mut dag = dag_for_plan(repo, commit, &plan)?;
    let repository_state_fingerprint = repository_state_fingerprint(repo, evidence_root)?;
    if let Some(receipt) = load_exact_verification_reuse(
        &store,
        &store_root,
        &plan,
        &dag,
        &repository_state_fingerprint,
    )? {
        let mut summary = receipt.summary_lines();
        summary.push("verification_reuse=exact-persisted-receipt".to_owned());
        return Ok(AuthoritativeVerificationResult { receipt, summary });
    }
    let mut materials = BTreeMap::<String, CheckMaterial>::new();
    let mut browser_material: Option<CheckMaterial> = None;

    while materials.len() < plan.checks.len() {
        let ready_ids = dag
            .ready_nodes()
            .into_iter()
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        if ready_ids.is_empty() {
            let blocked_ids = plan
                .checks
                .iter()
                .filter(|check| !materials.contains_key(&check.id))
                .map(|check| check.id.clone())
                .collect::<Vec<_>>();
            for check_id in blocked_ids {
                materials.insert(
                    check_id,
                    rejected_material("verification blocked by failed prerequisite".to_owned()),
                );
            }
            break;
        }
        let ready_checks = ready_ids
            .iter()
            .filter_map(|id| plan.checks.iter().find(|check| check.id == *id))
            .collect::<Vec<_>>();
        let mut command_results = execute_command_wave(repo, &ready_checks);

        for check in ready_checks {
            dag.mark_running(&check.id).map_err(invalid)?;
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
                _ => match command_results.remove(&check.id) {
                    Some(Ok(executed)) => materialize_command(
                        repository_fingerprint,
                        commit,
                        check,
                        executed,
                        &store,
                        &mut artifacts,
                        &mut reads,
                        &mut commands,
                        &mut records,
                    )?,
                    Some(Err(error)) => rejected_material(error),
                    None => rejected_material(format!(
                        "verification check {} has no executable adapter",
                        check.id
                    )),
                },
            };
            let input = dag
                .node(&check.id)
                .ok_or_else(|| invalid(format!("missing verification DAG node {}", check.id)))?
                .input
                .clone();
            dag.record_receipt(DagReceipt {
                node_id: check.id.clone(),
                input,
                passed: material.passed,
                duration_ms: material
                    .command
                    .as_ref()
                    .map_or(0, |receipt| receipt.duration_ms),
                artifact_refs: material
                    .artifact_ids
                    .iter()
                    .map(|artifact_id| artifact_id.0.clone())
                    .collect(),
            })
            .map_err(invalid)?;
            materials.insert(check.id.clone(), material);
        }
    }

    let mut checks = Vec::with_capacity(plan.checks.len());
    for (check_index, check) in plan.checks.iter().enumerate() {
        let material = materials
            .remove(&check.id)
            .ok_or_else(|| invalid(format!("missing verification material for {}", check.id)))?;
        let mut details = material.details;
        details.push(format!("planner_reason={}", check.reason));
        details.push("verification_execution=dependency-dag".to_owned());
        if check_index == 0 {
            details.extend(graph_details.iter().cloned());
        }
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
    let persisted_dag =
        PersistedVerificationDag::new(dag, &receipt.fingerprint, &repository_state_fingerprint)?;
    fs::write(
        store_root.join("verification-dag.json"),
        serde_json::to_vec_pretty(&persisted_dag).map_err(|error| {
            MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                error.to_string(),
            )
        })?,
    )?;
    Ok(AuthoritativeVerificationResult { receipt, summary })
}

fn load_exact_verification_reuse(
    store: &ArtifactStore,
    store_root: &Path,
    plan: &VerificationPlan,
    current_dag: &VerificationDag,
    repository_state_fingerprint: &str,
) -> MedusaResult<Option<VerificationReceipt>> {
    let receipt_path = store_root.join("verification-receipt.json");
    let dag_path = store_root.join("verification-dag.json");
    let (receipt_bytes, dag_bytes) = match (fs::read(&receipt_path), fs::read(&dag_path)) {
        (Ok(receipt), Ok(dag)) => (receipt, dag),
        _ => return Ok(None),
    };
    let receipt = match serde_json::from_slice::<VerificationReceipt>(&receipt_bytes) {
        Ok(receipt) => receipt,
        Err(_) => return Ok(None),
    };
    let persisted = match serde_json::from_slice::<PersistedVerificationDag>(&dag_bytes) {
        Ok(persisted) => persisted,
        Err(_) => return Ok(None),
    };
    if persisted.validate().is_err()
        || persisted.verification_receipt_fingerprint != receipt.fingerprint
        || persisted.repository_state_fingerprint != repository_state_fingerprint
        || !persistent_reuse_allowed(plan)
        || receipt.validate().is_err()
        || !receipt.passed
        || receipt.plan != *plan
        || !persisted.dag.authoritative_complete()
        || !current_dag.exact_receipts_reusable_from(&persisted.dag)
        || !receipt_artifacts_available(store, &receipt)
    {
        return Ok(None);
    }
    Ok(Some(receipt))
}

fn persisted_verification_dag_fingerprint(
    persisted: &PersistedVerificationDag,
) -> MedusaResult<String> {
    let mut hasher = Sha256::new();
    hasher.update(persisted.schema_version.to_le_bytes());
    hasher.update(persisted.verification_receipt_fingerprint.as_bytes());
    hasher.update(persisted.repository_state_fingerprint.as_bytes());
    hasher.update(serde_json::to_vec(&persisted.dag).map_err(|error| {
        MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            error.to_string(),
        )
    })?);
    Ok(format!("{:x}", hasher.finalize()))
}

fn persistent_reuse_allowed(plan: &VerificationPlan) -> bool {
    !plan.checks.is_empty()
        && plan.checks.iter().all(|check| {
            check.kind == VerificationCheckKind::ArtifactSemantic && check.program.is_none()
        })
}

fn receipt_artifacts_available(store: &ArtifactStore, receipt: &VerificationReceipt) -> bool {
    receipt.evidence.artifacts.iter().all(|expected| {
        store
            .metadata(&expected.id)
            .is_ok_and(|actual| actual == *expected)
    })
}

fn repository_state_fingerprint(repo: &Path, evidence_root: &Path) -> MedusaResult<String> {
    let mut paths = repository_state_paths(repo)?;
    paths.sort();
    paths.dedup();
    let evidence_root = evidence_root
        .canonicalize()
        .unwrap_or_else(|_| evidence_root.to_path_buf());
    let mut hasher = Sha256::new();
    for relative in paths {
        let path = repo.join(&relative);
        let absolute = path.canonicalize().unwrap_or_else(|_| path.clone());
        if absolute.starts_with(&evidence_root) || relative.starts_with(".git") {
            continue;
        }
        hasher.update(relative.to_string_lossy().as_bytes());
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                hasher.update(b"symlink");
                match fs::read_link(&path) {
                    Ok(target) => hasher.update(target.to_string_lossy().as_bytes()),
                    Err(_) => hasher.update(b"unreadable"),
                }
            }
            Ok(metadata) if metadata.is_file() => {
                hasher.update(b"file");
                match fs::read(&path) {
                    Ok(bytes) => hasher.update(bytes),
                    Err(_) => hasher.update(b"unreadable"),
                }
            }
            Ok(_) => hasher.update(b"other"),
            Err(_) => hasher.update(b"missing"),
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn repository_state_paths(repo: &Path) -> MedusaResult<Vec<PathBuf>> {
    if git_repository(repo) {
        let output = Command::new("git")
            .args([
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ])
            .current_dir(repo)
            .output()?;
        if output.status.success() {
            return Ok(output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|entry| !entry.is_empty())
                .map(|entry| PathBuf::from(String::from_utf8_lossy(entry).into_owned()))
                .collect());
        }
    }
    let mut result = Vec::new();
    collect_repository_paths(repo, repo, &mut result)?;
    Ok(result)
}

fn collect_repository_paths(
    root: &Path,
    directory: &Path,
    result: &mut Vec<PathBuf>,
) -> MedusaResult<()> {
    let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        if relative.starts_with(".git") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            collect_repository_paths(root, &path, result)?;
        } else {
            result.push(relative);
        }
    }
    Ok(())
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

fn graph_verification_assessment(
    repo: &Path,
    commit: &str,
    plan: &VerificationPlan,
) -> MedusaResult<Vec<String>> {
    if !git_repository(repo) {
        return Ok(Vec::new());
    }

    let graph = RepositoryGraph::open(repo)?;
    if graph.freshness() != RepositoryGraphFreshness::Current {
        return Err(invalid(
            "authoritative verification refused stale repository graph evidence",
        ));
    }
    let synthetic_worktree = commit.starts_with("worktree:");
    if !synthetic_worktree && graph.snapshot().repository_revision != commit {
        return Err(invalid(format!(
            "authoritative verification graph revision {} does not match requested commit {commit}",
            graph.snapshot().repository_revision
        )));
    }

    let changed_paths = plan
        .components
        .iter()
        .flat_map(|component| component.all_paths())
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let impact = graph.related_tests(&changed_paths);
    if impact.freshness != RepositoryGraphFreshness::Current
        || impact.repository_revision != graph.snapshot().repository_revision
        || impact.graph_revision != graph.snapshot().graph_revision
    {
        return Err(invalid(
            "authoritative verification refused stale or cross-revision test-impact evidence",
        ));
    }

    for command in &impact.value.commands {
        if !plan_covers_graph_command(plan, command) {
            return Err(invalid(format!(
                "authoritative verification plan does not cover graph-selected check: {command}"
            )));
        }
    }

    let review = ReviewImpact::analyze(&graph.snapshot().index, &changed_paths);
    let has_build = plan
        .checks
        .iter()
        .any(|check| check.kind == VerificationCheckKind::Build);
    let has_unit = plan
        .checks
        .iter()
        .any(|check| check.kind == VerificationCheckKind::Unit);
    let has_repository_defined = plan
        .checks
        .iter()
        .any(|check| check.kind == VerificationCheckKind::RepositoryDefined && check.required);
    if review.public_api_risk && !((has_build && has_unit) || has_repository_defined) {
        return Err(invalid(
            "public API risk requires build and unit verification coverage or a required repository-defined verifier",
        ));
    }

    let mut details = vec![
        format!("repository_graph_revision={}", impact.graph_revision),
        format!("repository_graph_freshness={:?}", impact.freshness),
        format!("repository_graph_adapter={}", impact.source_adapter),
        format!(
            "repository_graph_confidence_milli={}",
            impact.confidence_milli
        ),
        format!(
            "repository_graph_public_api_risk={}",
            review.public_api_risk
        ),
        format!(
            "repository_graph_affected_paths={}",
            review
                .affected_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(",")
        ),
    ];
    details.extend(
        impact
            .evidence_refs
            .iter()
            .map(|reference| format!("repository_graph_evidence={reference}")),
    );
    details.extend(
        impact
            .value
            .commands
            .iter()
            .map(|command| format!("repository_graph_required_check={command}")),
    );
    details.extend(
        impact
            .value
            .reasons
            .iter()
            .map(|reason| format!("repository_graph_check_reason={reason}")),
    );
    Ok(details)
}

fn plan_covers_graph_command(plan: &VerificationPlan, command: &str) -> bool {
    if command.starts_with("cargo test") {
        return plan.checks.iter().any(|check| {
            check.program.as_deref() == Some("cargo")
                && check.args.first().is_some_and(|arg| arg == "test")
                && cargo_scope_covers(check, command)
        });
    }
    if command.starts_with("cargo clippy") {
        return plan.checks.iter().any(|check| {
            check.program.as_deref() == Some("cargo")
                && check.args.first().is_some_and(|arg| arg == "clippy")
                && cargo_scope_covers(check, command)
        });
    }
    if command.starts_with("python -m pytest") {
        return plan.checks.iter().any(|check| {
            check.program.as_deref() == Some("python")
                && check.args.first().is_some_and(|arg| arg == "-m")
                && check.args.get(1).is_some_and(|arg| arg == "pytest")
        });
    }
    false
}

fn cargo_scope_covers(check: &VerificationCheck, command: &str) -> bool {
    if command.contains("--workspace") {
        return check.working_directory == "." && check.args.iter().any(|arg| arg == "--workspace");
    }
    if let Some(package) = command
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find_map(|pair| (pair[0] == "-p").then_some(pair[1]))
    {
        return check.working_directory == "."
            || check.working_directory.ends_with(&format!("/{package}"))
            || check.working_directory == format!("crates/{package}")
            || check
                .args
                .windows(2)
                .any(|pair| pair[0] == "-p" && pair[1] == package);
    }
    check.working_directory == "."
}

fn git_repository(repo: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(repo)
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout.starts_with(b"true"))
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
    let mut preview_details = command_output_preview("stdout", &executed.stdout);
    preview_details.extend(command_output_preview("stderr", &executed.stderr));
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
    let mut details = vec![
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
    details.extend(preview_details);
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

fn command_output_preview(stream: &str, bytes: &[u8]) -> Vec<String> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(bytes);
    let mut lines = Vec::new();
    let mut remaining = COMMAND_PREVIEW_MAX_BYTES;
    let mut source_lines = text.lines();
    for source in source_lines.by_ref().take(COMMAND_PREVIEW_MAX_LINES) {
        if remaining == 0 {
            break;
        }
        let normalized = source
            .chars()
            .map(|character| {
                if character == '\t' {
                    ' '
                } else if character.is_control() {
                    '�'
                } else {
                    character
                }
            })
            .collect::<String>();
        let preview = if secret_like(&normalized) {
            "[redacted secret-like output]".to_owned()
        } else {
            truncate_utf8(&normalized, remaining)
        };
        remaining = remaining.saturating_sub(preview.len());
        lines.push(format!(
            "command_{stream}_preview_non_authoritative={preview}"
        ));
    }
    if source_lines.next().is_some()
        || text.len() > COMMAND_PREVIEW_MAX_BYTES
        || lines.len() == COMMAND_PREVIEW_MAX_LINES
    {
        lines.push(format!(
            "command_{stream}_preview_non_authoritative=[truncated; full output is stored as a content-addressed artifact]"
        ));
    }
    lines
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn secret_like(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "api_key",
        "apikey",
        "authorization:",
        "bearer ",
        "password=",
        "secret=",
        "token=",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
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
    fn trusted_rustfmt_preparation_normalizes_only_changed_rust_files() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(directory.path().join("src")).expect("src");
        fs::write(
            directory.path().join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("manifest");
        fs::write(
            directory.path().join("src/lib.rs"),
            "pub fn value()->u32{1}\n\n",
        )
        .expect("source");
        fs::write(directory.path().join("untouched.txt"), "unchanged\n").expect("untouched");
        let component =
            ChangedComponent::new(ChangeKind::Modified, "src/lib.rs").expect("component");
        prepare_components_for_verification(directory.path(), &[component]).expect("preparation");
        assert_eq!(
            fs::read_to_string(directory.path().join("src/lib.rs")).expect("source"),
            "pub fn value() -> u32 {\n    1\n}\n"
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("untouched.txt")).expect("untouched"),
            "unchanged\n"
        );
    }

    #[test]
    fn command_preview_is_bounded_and_redacts_secret_like_lines() {
        let long = "x".repeat(COMMAND_PREVIEW_MAX_BYTES + 512);
        let output = format!("verified-value-42\ntoken=do-not-persist\n{long}\nextra-line");
        let preview = command_output_preview("stdout", output.as_bytes());
        assert!(
            preview
                .iter()
                .any(|line| line.contains("verified-value-42"))
        );
        assert!(
            preview
                .iter()
                .any(|line| line.contains("[redacted secret-like output]"))
        );
        assert!(!preview.iter().any(|line| line.contains("do-not-persist")));
        assert!(preview.iter().any(|line| line.contains("[truncated;")));
        assert!(
            preview.iter().map(String::len).sum::<usize>() < COMMAND_PREVIEW_MAX_BYTES + 2 * 1024
        );
    }

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
    fn exact_hermetic_receipt_reuses_only_with_unchanged_repository_and_artifacts() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("artifact.json"), "{\"ok\":true}\n").unwrap();
        fs::write(directory.path().join("unrelated.txt"), "stable\n").unwrap();
        let component = ChangedComponent::new(ChangeKind::Modified, "artifact.json").unwrap();
        let evidence_root = directory.path().join("durable-evidence");
        let result = authoritative_verification_for_components_at(
            directory.path(),
            &evidence_root,
            "repo",
            "commit",
            &[component.clone()],
        )
        .unwrap();
        assert!(result.receipt.passed);
        assert!(result.receipt.evidence.commands.is_empty());
        assert!(!result.receipt.evidence.artifacts.is_empty());

        let reused = authoritative_verification_for_components_at(
            directory.path(),
            &evidence_root,
            "repo",
            "commit",
            &[component.clone()],
        )
        .unwrap();
        assert!(
            reused
                .summary
                .iter()
                .any(|line| line == "verification_reuse=exact-persisted-receipt")
        );

        fs::write(directory.path().join("unrelated.txt"), "changed\n").unwrap();
        let rerun_after_tree_drift = authoritative_verification_for_components_at(
            directory.path(),
            &evidence_root,
            "repo",
            "commit",
            &[component.clone()],
        )
        .unwrap();
        assert!(rerun_after_tree_drift.receipt.passed);
        assert!(
            !rerun_after_tree_drift
                .summary
                .iter()
                .any(|line| line == "verification_reuse=exact-persisted-receipt")
        );

        let store_root = evidence_root.join(short_hash("repocommit"));
        let object = fs::read_dir(store_root.join("objects"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::remove_file(object).unwrap();
        let rerun_after_artifact_loss = authoritative_verification_for_components_at(
            directory.path(),
            &evidence_root,
            "repo",
            "commit",
            &[component],
        )
        .unwrap();
        assert!(rerun_after_artifact_loss.receipt.passed);
        assert!(
            !rerun_after_artifact_loss
                .summary
                .iter()
                .any(|line| line == "verification_reuse=exact-persisted-receipt")
        );
    }

    #[test]
    fn persistent_reuse_rejects_non_hermetic_repository_defined_checks() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join(".medusa")).unwrap();
        fs::write(directory.path().join("file.txt"), "changed\n").unwrap();
        fs::write(
            directory.path().join(".medusa/verification.json"),
            r#"{"checks":[{"kind":"repository_defined","program":"rustc","args":["--version"],"working_directory":".","reason":"external verifier"}]}"#,
        )
        .unwrap();
        let component = ChangedComponent::new(ChangeKind::Modified, "file.txt").unwrap();
        let evidence_root = directory.path().join("durable-evidence");
        let first = authoritative_verification_for_components_at(
            directory.path(),
            &evidence_root,
            "repo",
            "commit",
            &[component.clone()],
        )
        .unwrap();
        assert!(first.receipt.passed);
        let second = authoritative_verification_for_components_at(
            directory.path(),
            &evidence_root,
            "repo",
            "commit",
            &[component],
        )
        .unwrap();
        assert!(second.receipt.passed);
        assert!(
            !second
                .summary
                .iter()
                .any(|line| line == "verification_reuse=exact-persisted-receipt")
        );
    }

    #[test]
    fn graph_selected_checks_and_public_api_risk_are_covered_by_authoritative_plan() {
        let directory = tempfile::tempdir().expect("repository");
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(directory.path())
                .status()
                .expect("git");
            assert!(status.success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "medusa@example.invalid"]);
        git(&["config", "user.name", "Medusa Test"]);
        fs::create_dir_all(directory.path().join("src")).expect("src");
        fs::write(
            directory.path().join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("manifest");
        fs::write(
            directory.path().join("src/lib.rs"),
            "pub fn answer() -> u8 { 42 }\n",
        )
        .expect("source");
        git(&["add", "."]);
        git(&["commit", "-qm", "fixture"]);
        let commit = git_stdout(directory.path(), &["rev-parse", "HEAD"]).expect("commit");
        let component =
            ChangedComponent::new(ChangeKind::Modified, "src/lib.rs").expect("component");
        let plan = VerificationPlanner::plan(directory.path(), "repo", &commit, &[component], &[])
            .expect("plan");

        let details = graph_verification_assessment(directory.path(), &commit, &plan)
            .expect("graph verification assessment");
        assert!(
            details
                .iter()
                .any(|line| line == "repository_graph_public_api_risk=true")
        );
        assert!(
            details
                .iter()
                .any(|line| line.starts_with("repository_graph_required_check=cargo test"))
        );
        assert!(graph_verification_assessment(directory.path(), "wrong-revision", &plan).is_err());
    }
}
