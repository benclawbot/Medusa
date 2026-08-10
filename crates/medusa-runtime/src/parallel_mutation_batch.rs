use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{atomic::AtomicBool, mpsc::Sender},
};

use medusa_agent::authoritative_verification_for_components_at;
use medusa_config::Config;
use medusa_evidence::ChangedComponent;
use medusa_multi_agent_scheduler::mutation_dag::{
    AcceptedTaskEvidence, BatchIntegrationReceipt, IntegrationBarrier, MutationDag,
};
use medusa_provider::{Message, MessageBlock, ModelProvider, ModelRequest, ResponseBlock, Role};
use medusa_review_model::{
    ParentReviewDecision, ParentReviewResponse, ParentReviewResponseError, final_parent_review_line,
    validate_parent_review_response,
};
use medusa_workers::{IntegrationReceipt, Worker, WorkerManager};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    RuntimeActivity, RuntimeActivityKind, RuntimeEvent,
    mutating_worker_coordinator::ParallelImplementationEvidence,
    mutation_transaction::{
        MutationLifecycle, MutationTransaction, ParentReviewAuthorization,
        authorize_after_parent_review,
    },
};

const BATCH_SCHEMA_VERSION: u16 = 1;
const MAX_BATCH_REVIEW_OUTPUT_TOKENS: u32 = 4_096;
const BATCH_REVIEW_SYSTEM_PROMPT: &str = "You are Medusa's read-only combined mutation batch reviewer. You have no tools. Review the complete set of individually reviewed and independently verified immutable child patches as one candidate. Reject only for a concrete cross-task defect, incompatible combined behavior, stale dependency evidence, or scope/integration mismatch. Do not execute tools or claim integration already occurred. End with the exact typed parent-review JSON envelope and write nothing after it.";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParallelBatchCompletion {
    pub receipt: BatchIntegrationReceipt,
    pub integrations: Vec<IntegrationReceipt>,
    pub changed_paths: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParallelBatchOutcome {
    RevisionRequested(String),
    Reconciled(ParallelBatchCompletion),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CombinedReviewJournal {
    schema_version: u16,
    request_fingerprint: String,
    response_text: String,
    fingerprint: String,
}

#[allow(clippy::too_many_arguments)]
pub fn complete<P: ModelProvider>(
    repo: &Path,
    config: &Config,
    provider: &P,
    dag: &MutationDag,
    evidence: &ParallelImplementationEvidence,
    repository_fingerprint: &str,
    cancel: &AtomicBool,
    events: &Sender<RuntimeEvent>,
) -> Result<ParallelBatchOutcome, String> {
    dag.validate().map_err(str::to_owned)?;
    if evidence.dag_fingerprint != dag.fingerprint {
        return Err("parallel implementation evidence belongs to another mutation DAG".to_owned());
    }
    let ordered = dag.deterministic_integration_order();
    let child_ids = evidence
        .children
        .iter()
        .map(|child| child.task_id.clone())
        .collect::<Vec<_>>();
    if child_ids != ordered {
        return Err("parallel child evidence is not in deterministic DAG integration order".to_owned());
    }

    let batch_root = batch_root(repo, &dag.fingerprint);
    fs::create_dir_all(&batch_root).map_err(|error| error.to_string())?;
    let manager = WorkerManager::new(repo, batch_root.join("worktrees"))
        .map_err(|error| error.to_string())?;
    manager.require_clean().map_err(|error| error.to_string())?;
    let base_head = manager.repository_head().map_err(|error| error.to_string())?;
    if base_head != dag.repository_revision {
        return Err(format!(
            "parallel mutation base drifted from {} to {base_head}",
            dag.repository_revision
        ));
    }

    let mut accepted = Vec::with_capacity(evidence.children.len());
    let mut prepared_trees = BTreeMap::new();
    let mut workers = Vec::with_capacity(evidence.children.len());
    let mut original_workers = Vec::with_capacity(evidence.children.len());
    let mut combined_packet = Vec::new();

    for (index, child) in evidence.children.iter().enumerate() {
        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("parallel mutation batch was cancelled during parent review".to_owned());
        }
        let task = dag
            .tasks
            .iter()
            .find(|task| task.id == child.task_id)
            .ok_or_else(|| format!("mutation DAG lost task {}", child.task_id))?;
        let before = MutationTransaction::open(&child.evidence.transaction_path)?;
        if before.snapshot().base_head != base_head
            || before.snapshot().prepared_commit != child.evidence.prepared_commit
            || before.snapshot().prepared_tree != child.evidence.prepared_tree
        {
            return Err(format!(
                "prepared transaction identity changed for {}",
                child.task_id
            ));
        }
        combined_packet.push(render_child_packet(&child.task_id, &before)?);
        match authorize_after_parent_review(
            &child.evidence.transaction_path,
            repo,
            provider,
            config,
            cancel,
            events,
        )? {
            ParentReviewAuthorization::RevisionRequested(reason) => {
                return Ok(ParallelBatchOutcome::RevisionRequested(format!(
                    "{}: {reason}",
                    child.task_id
                )));
            }
            ParentReviewAuthorization::Authorized => {}
        }
        let authorized = MutationTransaction::open(&child.evidence.transaction_path)?;
        if authorized.snapshot().lifecycle != MutationLifecycle::IntegrationAuthorized {
            return Err(format!(
                "parallel child {} did not reach integration authorization",
                child.task_id
            ));
        }
        let verification = authorized
            .snapshot()
            .verification
            .as_ref()
            .ok_or_else(|| format!("parallel child {} lost verification evidence", child.task_id))?;
        let dependency_fingerprints = task
            .dependencies
            .iter()
            .map(|dependency| {
                prepared_trees
                    .get(dependency)
                    .cloned()
                    .map(|tree| (dependency.clone(), tree))
                    .ok_or_else(|| {
                        format!(
                            "parallel child {} lacks accepted upstream tree for {dependency}",
                            child.task_id
                        )
                    })
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        accepted.push(AcceptedTaskEvidence {
            task_id: child.task_id.clone(),
            prepared_commit: authorized.snapshot().prepared_commit.clone(),
            prepared_tree: authorized.snapshot().prepared_tree.clone(),
            contract_fingerprint: hash(task),
            dependency_fingerprints,
            verification_fingerprint: verification.fingerprint.clone(),
        });
        prepared_trees.insert(
            child.task_id.clone(),
            authorized.snapshot().prepared_tree.clone(),
        );
        let original = authorized.snapshot().worker.clone();
        let mut ordered_worker = original.clone();
        ordered_worker.id = format!("parallel-{index:04}-{}", child.task_id);
        workers.push(ordered_worker);
        original_workers.push(original);
    }

    let barrier = IntegrationBarrier::establish(dag, accepted).map_err(str::to_owned)?;
    let combined_context = format!(
        "Combined immutable mutation candidate. DAG fingerprint: {}. Deterministic order: {:?}. Every child below already passed its own dedicated parent review and independent verification. Review cross-task compatibility and the combined scope before the runtime integrates anything.\n\n{}\n\n{}",
        dag.fingerprint,
        barrier.ordered_tasks,
        combined_packet.join("\n\n"),
        medusa_review_model::PARENT_REVIEW_RESPONSE_REQUIREMENT,
    );
    let combined = combined_review(
        provider,
        config,
        cancel,
        &batch_root.join("combined-parent-review.json"),
        &combined_context,
    )?;
    if combined.decision == ParentReviewDecision::RevisionRequested {
        return Ok(ParallelBatchOutcome::RevisionRequested(format!(
            "combined candidate: {}",
            combined.rationale
        )));
    }

    let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
        id: Some(dag.fingerprint.clone()),
        kind: RuntimeActivityKind::Progress,
        title: "Parallel integration barrier satisfied".to_owned(),
        details: vec![
            format!("tasks={:?}", barrier.ordered_tasks),
            format!("barrier={}", barrier.fingerprint),
            "all child reviews, independent verification, and combined review accepted".to_owned(),
        ],
    }));

    let integrations = manager
        .integrate_successful(&workers)
        .map_err(|error| error.to_string())?;
    if integrations.len() != workers.len() {
        rollback(repo, &base_head)?;
        return Err("parallel integration omitted an authorized child".to_owned());
    }
    let final_head = manager.repository_head().map_err(|error| error.to_string())?;
    let final_tree = manager
        .commit_tree(&final_head)
        .map_err(|error| error.to_string())?;
    let components = integrations
        .iter()
        .flat_map(|receipt| receipt.changed_components.clone())
        .collect::<Vec<ChangedComponent>>();
    let verification = authoritative_verification_for_components_at(
        repo,
        &batch_root.join("evidence/final"),
        repository_fingerprint,
        &final_head,
        &components,
    );
    let verification = match verification {
        Ok(verification) if verification.receipt.passed => verification,
        Ok(verification) => {
            rollback(repo, &base_head)?;
            return Err(format!(
                "final parallel verification failed: {}",
                verification.summary.join(" | ")
            ));
        }
        Err(error) => {
            rollback(repo, &base_head)?;
            return Err(format!("final parallel verification could not run: {error}"));
        }
    };

    let receipt = BatchIntegrationReceipt::complete(
        &barrier,
        base_head,
        barrier.ordered_tasks.clone(),
        final_head,
        final_tree,
        verification.receipt.fingerprint.clone(),
    )
    .map_err(str::to_owned)?;
    persist_json(&batch_root.join("batch-integration-receipt.json"), &receipt)?;
    manager
        .cleanup(&original_workers)
        .map_err(|error| error.to_string())?;

    let mut changed_paths = integrations
        .iter()
        .flat_map(|receipt| receipt.changed_paths.clone())
        .collect::<Vec<_>>();
    changed_paths.sort();
    changed_paths.dedup();
    let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
        id: Some(dag.fingerprint.clone()),
        kind: RuntimeActivityKind::Done,
        title: "Parallel mutation batch reconciled".to_owned(),
        details: vec![
            format!("receipt={}", receipt.fingerprint),
            format!("final_head={}", receipt.final_head),
            format!("changed_paths={changed_paths:?}"),
        ],
    }));
    Ok(ParallelBatchOutcome::Reconciled(ParallelBatchCompletion {
        receipt,
        integrations,
        changed_paths,
    }))
}

fn render_child_packet(task_id: &str, transaction: &MutationTransaction) -> Result<String, String> {
    let snapshot = transaction.snapshot();
    let root = transaction
        .path()
        .parent()
        .ok_or_else(|| "parallel child transaction has no durable root".to_owned())?;
    let patch = fs::read_to_string(root.join("prepared.patch")).map_err(|error| error.to_string())?;
    Ok(format!(
        "## Child {task_id}\ncommit={}\ntree={}\nchanged_paths={:?}\npatch_fingerprint={}\n\n{}",
        snapshot.prepared_commit,
        snapshot.prepared_tree,
        snapshot.changed_paths,
        snapshot.patch_fingerprint,
        patch,
    ))
}

fn combined_review<P: ModelProvider>(
    provider: &P,
    config: &Config,
    cancel: &AtomicBool,
    journal_path: &Path,
    context: &str,
) -> Result<medusa_review_model::ParentReviewOutcome, String> {
    let request = ModelRequest {
        system: BATCH_REVIEW_SYSTEM_PROMPT.to_owned(),
        messages: vec![Message {
            role: Role::User,
            content: vec![MessageBlock::Text {
                text: context.to_owned(),
            }],
        }],
        tools: Vec::new(),
        max_tokens: config
            .model
            .max_output_tokens
            .min(MAX_BATCH_REVIEW_OUTPUT_TOKENS),
        temperature_milli: 0,
    };
    let request_fingerprint = hash(&request);
    if journal_path.is_file() {
        let journal: CombinedReviewJournal = serde_json::from_slice(
            &fs::read(journal_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        validate_combined_journal(&journal, &request_fingerprint)?;
        return decode_review(&journal.response_text);
    }
    let response = provider
        .complete_cancellable(&request, cancel)
        .map_err(|error| error.to_string())?;
    let mut text = Vec::new();
    for block in &response.blocks {
        match block {
            ResponseBlock::Text { text: value } => text.push(value.as_str()),
            ResponseBlock::ToolUse { name, .. } => {
                return Err(format!(
                    "combined parent reviewer attempted forbidden tool `{name}`"
                ));
            }
        }
    }
    let response_text = text.join("\n");
    let outcome = decode_review(&response_text)?;
    let mut journal = CombinedReviewJournal {
        schema_version: BATCH_SCHEMA_VERSION,
        request_fingerprint,
        response_text,
        fingerprint: String::new(),
    };
    journal.fingerprint = combined_journal_fingerprint(&journal);
    persist_json(journal_path, &journal)?;
    Ok(outcome)
}

fn decode_review(text: &str) -> Result<medusa_review_model::ParentReviewOutcome, String> {
    let final_line = final_parent_review_line(text).map_err(|error| error.to_string())?;
    let response: ParentReviewResponse = serde_json::from_str(final_line).map_err(|error| {
        ParentReviewResponseError::InvalidEnvelope(error.to_string()).to_string()
    })?;
    validate_parent_review_response(response, final_line).map_err(|error| error.to_string())
}

fn validate_combined_journal(
    journal: &CombinedReviewJournal,
    request_fingerprint: &str,
) -> Result<(), String> {
    if journal.schema_version != BATCH_SCHEMA_VERSION
        || journal.request_fingerprint != request_fingerprint
        || journal.response_text.trim().is_empty()
        || journal.fingerprint != combined_journal_fingerprint(journal)
    {
        return Err("combined parent-review journal is incomplete or stale".to_owned());
    }
    Ok(())
}

fn combined_journal_fingerprint(journal: &CombinedReviewJournal) -> String {
    hash(&(
        journal.schema_version,
        &journal.request_fingerprint,
        &journal.response_text,
    ))
}

fn batch_root(repo: &Path, dag_fingerprint: &str) -> PathBuf {
    repo.join(".medusa")
        .join("parallel-mutation-batches")
        .join(dag_fingerprint)
}

fn rollback(repo: &Path, base_head: &str) -> Result<(), String> {
    let output = Command::new("git")
        .args(["reset", "--hard", base_head])
        .current_dir(repo)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "parallel batch rollback failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

fn persist_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if path.exists() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    fs::rename(temporary, path).map_err(|error| error.to_string())
}

fn hash(value: &impl Serialize) -> String {
    let encoded = serde_json::to_vec(value).unwrap_or_default();
    format!("{:x}", Sha256::digest(encoded))
}
