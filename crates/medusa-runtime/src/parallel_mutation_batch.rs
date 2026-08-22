use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
};

use medusa_agent::{
    authoritative_verification_for_components_at, prepare_components_for_verification,
};
use medusa_config::Config;
use medusa_core::hidden_command;
use medusa_evidence::{ChangedComponent, changed_scope_fingerprint};
use medusa_multi_agent_scheduler::mutation_dag::{
    AcceptedTaskEvidence, IntegrationBarrier, MutationDag,
};
use medusa_provider::ConfiguredProvider;
use medusa_workers::{Worker, WorkerManager, WorkerState};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    RuntimeActivity, RuntimeActivityKind, RuntimeEvent,
    multi_agent_coordinator::CoordinatorEvidence,
    mutating_worker_coordinator::{ImplementationEvidence, ParallelImplementationEvidence},
    mutation_transaction::{
        MutationLifecycle, MutationTransaction, ParentReviewAuthorization, PreparedMutationInput,
        authorize_after_parent_review, cancel_transaction,
    },
    production_orchestrator::{AgentRole, ProductionExecutionPlan},
};

const BATCH_WORKER_LABEL: &str = "parallel-batch";
const PARALLEL_METRICS_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ParallelMutationMetrics {
    schema_version: u16,
    dag_fingerprint: String,
    task_count: usize,
    wave_count: usize,
    peak_parallelism: usize,
    conflict_edges: usize,
    serial_worker_ms: u64,
    parallel_worker_ms: u64,
    staging_ms: u64,
    idle_capacity_ms: u64,
    parallel_efficiency_milli: u16,
    wall_time_improvement_milli: u16,
    conflict_rate_milli: u16,
}

#[allow(clippy::too_many_arguments)]
pub fn prepare_combined(
    repo: &Path,
    config: &Config,
    session_api_key: Option<String>,
    plan: &ProductionExecutionPlan,
    preflight: &CoordinatorEvidence,
    dag: &MutationDag,
    cancel: &Arc<AtomicBool>,
    events: &Sender<RuntimeEvent>,
) -> Result<ImplementationEvidence, String> {
    dag.validate().map_err(str::to_owned)?;
    let batch_root = batch_root(
        preflight
            .state_path
            .parent()
            .ok_or_else(|| "parallel preflight has no execution root".to_owned())?,
        &dag.fingerprint,
    );
    fs::create_dir_all(&batch_root).map_err(|error| error.to_string())?;
    let prepared_evidence_path = batch_root.join("prepared-implementation-evidence.json");
    if prepared_evidence_path.is_file() {
        let evidence: ImplementationEvidence = serde_json::from_slice(
            &fs::read(&prepared_evidence_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        validate_resumed_aggregate(&evidence, plan, preflight, dag)?;
        return Ok(evidence);
    }

    let parallel = crate::mutating_worker_coordinator::run_parallel_implementations(
        repo,
        config,
        session_api_key.clone(),
        plan,
        preflight,
        dag,
        cancel,
        events,
    )?;
    validate_parallel_evidence(dag, &parallel)?;
    let provider = ConfiguredProvider::manager_from_config(config, session_api_key)
        .map_err(|error| error.to_string())?;
    let barrier = authorize_children(repo, config, &provider, dag, &parallel, cancel, events)?;
    persist_json(&batch_root.join("integration-barrier.json"), &barrier)?;

    let staging_started = std::time::Instant::now();
    let manager = WorkerManager::new(repo, batch_root.join("worktrees"))
        .map_err(|error| error.to_string())?;
    manager.require_clean().map_err(|error| error.to_string())?;
    let base_head = manager
        .repository_head()
        .map_err(|error| error.to_string())?;
    if base_head != dag.repository_revision {
        return Err(format!(
            "parallel staging base drifted from {} to {base_head}",
            dag.repository_revision
        ));
    }
    let worker_id = format!(
        "batch-{}",
        &dag.fingerprint[..dag.fingerprint.len().min(16)]
    );
    let mut staging = match manager.open_or_create_worker(BATCH_WORKER_LABEL, &worker_id) {
        Ok(worker) => worker,
        Err(first_error) => {
            let stale = Worker {
                id: worker_id.clone(),
                branch: format!("medusa/{BATCH_WORKER_LABEL}-{worker_id}"),
                worktree: batch_root.join("worktrees").join(&worker_id),
                state: WorkerState::Ready,
                commit: None,
                stdout: String::new(),
                stderr: String::new(),
            };
            let _ = manager.cleanup(&[stale]);
            manager
                .open_or_create_worker(BATCH_WORKER_LABEL, &worker_id)
                .map_err(|second_error| {
                    format!(
                        "could not recover parallel staging worker after {first_error}: {second_error}"
                    )
                })?
        }
    };
    reset_staging_for_replay(&staging.worktree, &base_head)?;

    for task_id in &barrier.ordered_tasks {
        if cancel.load(Ordering::SeqCst) {
            cleanup_staging(&manager, &staging, &base_head);
            return Err(
                "parallel staging was cancelled before deterministic composition".to_owned(),
            );
        }
        let child = parallel
            .children
            .iter()
            .find(|child| child.task_id == *task_id)
            .ok_or_else(|| format!("parallel staging lost child {task_id}"))?;
        if let Err(error) =
            cherry_pick_without_commit(&staging.worktree, &child.evidence.prepared_commit)
        {
            cleanup_staging(&manager, &staging, &base_head);
            return Err(format!(
                "deterministic staging failed for {task_id}: {error}"
            ));
        }
    }

    let initial_components = manager
        .changed_components_since(&staging, &base_head)
        .map_err(|error| error.to_string())?;
    validate_aggregate_scope(plan, &initial_components)?;
    prepare_components_for_verification(&staging.worktree, &initial_components)
        .map_err(|error| format!("parallel aggregate preparation failed: {error}"))?;
    let changed_components = manager
        .changed_components_since(&staging, &base_head)
        .map_err(|error| error.to_string())?;
    if changed_scope_fingerprint(&initial_components)
        != changed_scope_fingerprint(&changed_components)
    {
        cleanup_staging(&manager, &staging, &base_head);
        return Err(
            "parallel aggregate preparation changed the accepted mutation scope".to_owned(),
        );
    }
    let changed_paths = component_paths(&changed_components);
    let worktree_identity = format!(
        "parallel-staging:{}:{}",
        base_head,
        changed_scope_fingerprint(&changed_components)
    );
    let verification = authoritative_verification_for_components_at(
        &staging.worktree,
        &batch_root.join("evidence/staging"),
        &preflight.repository_fingerprint,
        &worktree_identity,
        &changed_components,
    )
    .map_err(|error| format!("parallel aggregate verification could not run: {error}"))?;
    if !verification.receipt.passed {
        cleanup_staging(&manager, &staging, &base_head);
        return Err(format!(
            "parallel aggregate verification failed: {}",
            verification.summary.join(" | ")
        ));
    }

    staging = manager
        .finalize_worker(
            staging,
            &base_head,
            &format!("Medusa parallel aggregate {}", dag.fingerprint),
        )
        .map_err(|error| error.to_string())?;
    let aggregate_root = batch_root.join("aggregate-transaction");
    let implementation_summary = parallel
        .children
        .iter()
        .map(|child| format!("{}: {}", child.task_id, child.evidence.summary))
        .collect::<Vec<_>>()
        .join("\n\n");
    let task_id = plan
        .contracts
        .iter()
        .find(|contract| contract.role == AgentRole::Implementer)
        .map(|contract| contract.task_id.clone())
        .ok_or_else(|| "parallel aggregate lost parent implementer contract".to_owned())?;
    let transaction = MutationTransaction::open_or_prepare(
        &aggregate_root,
        repo,
        PreparedMutationInput {
            plan_fingerprint: plan.fingerprint.clone(),
            repository_fingerprint: preflight.repository_fingerprint.clone(),
            task_id: task_id.clone(),
            base_head: base_head.clone(),
            worker: staging.clone(),
            changed_paths: changed_paths.clone(),
            changed_components: changed_components.clone(),
            implementation_summary: implementation_summary.clone(),
            worktree_verification_evidence: verification.summary.clone(),
            worktree_verification_receipt: verification.receipt.clone(),
            speculative: false,
        },
    )?;
    if transaction.snapshot().lifecycle != MutationLifecycle::ReviewPending {
        return Err("parallel aggregate did not reach parent-review pending state".to_owned());
    }
    let evidence = ImplementationEvidence {
        plan_fingerprint: plan.fingerprint.clone(),
        repository_fingerprint: preflight.repository_fingerprint.clone(),
        task_id,
        worker_id: staging.id.clone(),
        delegation_contract_id: String::new(),
        delegation_contract_fingerprint: String::new(),
        delegation_attempt_fingerprint: String::new(),
        session_id: format!("parallel-{}", hash(&parallel.dag_fingerprint)),
        turns: parallel
            .children
            .iter()
            .map(|child| child.evidence.turns)
            .sum(),
        summary: implementation_summary,
        changed_paths,
        changed_components,
        verification_evidence: verification.summary,
        verification_receipt: verification.receipt,
        base_head,
        prepared_commit: transaction.snapshot().prepared_commit.clone(),
        prepared_tree: transaction.snapshot().prepared_tree.clone(),
        patch_fingerprint: transaction.snapshot().patch_fingerprint.clone(),
        review_context: transaction.review_context()?,
        transaction_path: transaction.path().to_path_buf(),
        state_path: prepared_evidence_path.clone(),
    };
    persist_json(&prepared_evidence_path, &evidence)?;
    let metrics = parallel_metrics(
        dag,
        &parallel,
        u64::try_from(staging_started.elapsed().as_millis()).unwrap_or(u64::MAX),
    );
    persist_json(&batch_root.join("parallel-mutation-metrics.json"), &metrics)?;

    for child in &parallel.children {
        let _ = cancel_transaction(
            &child.evidence.transaction_path,
            "authorized child superseded by deterministic aggregate transaction",
            events,
        );
    }
    let child_workers = parallel
        .children
        .iter()
        .filter_map(|child| {
            MutationTransaction::open(&child.evidence.transaction_path)
                .ok()
                .map(|transaction| transaction.snapshot().worker.clone())
        })
        .collect::<Vec<_>>();
    let _ = manager.cleanup(&child_workers);
    let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
        id: Some(dag.fingerprint.clone()),
        kind: RuntimeActivityKind::Done,
        title: "Parallel aggregate prepared".to_owned(),
        details: vec![
            format!("tasks={:?}", barrier.ordered_tasks),
            format!("aggregate_commit={}", evidence.prepared_commit),
            format!("changed_paths={:?}", evidence.changed_paths),
            format!(
                "parallel_efficiency_milli={}",
                metrics.parallel_efficiency_milli
            ),
            format!(
                "wall_time_improvement_milli={}",
                metrics.wall_time_improvement_milli
            ),
            format!("conflict_rate_milli={}", metrics.conflict_rate_milli),
            format!("idle_capacity_ms={}", metrics.idle_capacity_ms),
            format!("staging_ms={}", metrics.staging_ms),
            "primary repository remains unchanged pending combined parent review".to_owned(),
        ],
    }));
    Ok(evidence)
}

fn authorize_children<P: medusa_provider::ModelProvider>(
    repo: &Path,
    config: &Config,
    provider: &P,
    dag: &MutationDag,
    parallel: &ParallelImplementationEvidence,
    cancel: &AtomicBool,
    events: &Sender<RuntimeEvent>,
) -> Result<IntegrationBarrier, String> {
    let mut accepted = Vec::with_capacity(parallel.children.len());
    let mut prepared_trees = BTreeMap::new();
    for child in &parallel.children {
        let task = dag
            .tasks
            .iter()
            .find(|task| task.id == child.task_id)
            .ok_or_else(|| format!("mutation DAG lost task {}", child.task_id))?;
        match authorize_after_parent_review(
            &child.evidence.transaction_path,
            repo,
            provider,
            config,
            cancel,
            events,
        )? {
            ParentReviewAuthorization::RevisionRequested(reason) => {
                return Err(format!("{} requested revision: {reason}", child.task_id));
            }
            ParentReviewAuthorization::Authorized => {}
        }
        let transaction = MutationTransaction::open(&child.evidence.transaction_path)?;
        if transaction.snapshot().lifecycle != MutationLifecycle::IntegrationAuthorized
            || transaction.snapshot().base_head != dag.repository_revision
        {
            return Err(format!(
                "parallel child {} lacks valid integration authorization",
                child.task_id
            ));
        }
        let verification = transaction
            .snapshot()
            .verification
            .as_ref()
            .ok_or_else(|| format!("parallel child {} lost verification receipt", child.task_id))?;
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
            prepared_commit: transaction.snapshot().prepared_commit.clone(),
            prepared_tree: transaction.snapshot().prepared_tree.clone(),
            contract_fingerprint: hash(task),
            dependency_fingerprints,
            verification_fingerprint: verification.fingerprint.clone(),
        });
        prepared_trees.insert(
            child.task_id.clone(),
            transaction.snapshot().prepared_tree.clone(),
        );
    }
    IntegrationBarrier::establish(dag, accepted).map_err(str::to_owned)
}

fn validate_parallel_evidence(
    dag: &MutationDag,
    parallel: &ParallelImplementationEvidence,
) -> Result<(), String> {
    if parallel.dag_fingerprint != dag.fingerprint
        || parallel
            .children
            .iter()
            .map(|child| child.task_id.clone())
            .collect::<Vec<_>>()
            != dag.deterministic_integration_order()
    {
        return Err("parallel child evidence does not match accepted DAG order".to_owned());
    }
    Ok(())
}

fn validate_aggregate_scope(
    plan: &ProductionExecutionPlan,
    components: &[ChangedComponent],
) -> Result<(), String> {
    let contract = plan
        .contracts
        .iter()
        .find(|contract| contract.role == AgentRole::Implementer)
        .ok_or_else(|| "parallel aggregate has no parent implementer contract".to_owned())?;
    let paths = component_paths(components);
    if paths.is_empty()
        || paths.iter().any(|path| {
            !contract.allowed_write_paths.iter().any(|allowed| {
                path == allowed
                    || path
                        .strip_prefix(allowed)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            })
        })
    {
        return Err(format!(
            "parallel aggregate escaped parent write scope: {paths:?} not within {:?}",
            contract.allowed_write_paths
        ));
    }
    Ok(())
}

fn validate_resumed_aggregate(
    evidence: &ImplementationEvidence,
    plan: &ProductionExecutionPlan,
    preflight: &CoordinatorEvidence,
    dag: &MutationDag,
) -> Result<(), String> {
    if evidence.plan_fingerprint != plan.fingerprint
        || evidence.repository_fingerprint != preflight.repository_fingerprint
        || evidence.base_head != dag.repository_revision
        || evidence.transaction_path.as_os_str().is_empty()
    {
        return Err("durable parallel aggregate evidence is stale".to_owned());
    }
    let transaction = MutationTransaction::open(&evidence.transaction_path)?;
    if transaction.snapshot().prepared_commit != evidence.prepared_commit
        || transaction.snapshot().prepared_tree != evidence.prepared_tree
        || !matches!(
            transaction.snapshot().lifecycle,
            MutationLifecycle::ReviewPending
                | MutationLifecycle::ReviewAccepted
                | MutationLifecycle::VerificationPending
                | MutationLifecycle::Verified
                | MutationLifecycle::IntegrationAuthorized
                | MutationLifecycle::Integrated
                | MutationLifecycle::Reconciled
        )
    {
        return Err("durable parallel aggregate transaction no longer matches evidence".to_owned());
    }
    Ok(())
}

fn parallel_metrics(
    dag: &MutationDag,
    parallel: &ParallelImplementationEvidence,
    staging_ms: u64,
) -> ParallelMutationMetrics {
    let serial_worker_ms = parallel.task_elapsed_ms.values().copied().sum::<u64>();
    let parallel_worker_ms = parallel.parallel_elapsed_ms.max(1);
    let peak_parallelism = u64::try_from(parallel.peak_parallelism.max(1)).unwrap_or(u64::MAX);
    let capacity_ms = parallel_worker_ms.saturating_mul(peak_parallelism);
    let idle_capacity_ms = capacity_ms.saturating_sub(serial_worker_ms);
    let efficiency = if capacity_ms == 0 {
        0
    } else {
        serial_worker_ms
            .saturating_mul(1_000)
            .saturating_div(capacity_ms)
            .min(1_000)
    };
    let improvement = if serial_worker_ms == 0 {
        0
    } else {
        serial_worker_ms
            .saturating_sub(parallel_worker_ms)
            .saturating_mul(1_000)
            .saturating_div(serial_worker_ms)
            .min(1_000)
    };
    let pairs = dag
        .tasks
        .len()
        .saturating_mul(dag.tasks.len().saturating_sub(1))
        / 2;
    let conflict_rate = if pairs == 0 {
        0
    } else {
        dag.conflict_edges
            .len()
            .saturating_mul(1_000)
            .saturating_div(pairs)
            .min(1_000)
    };
    ParallelMutationMetrics {
        schema_version: PARALLEL_METRICS_SCHEMA_VERSION,
        dag_fingerprint: dag.fingerprint.clone(),
        task_count: dag.tasks.len(),
        wave_count: parallel.wave_count,
        peak_parallelism: parallel.peak_parallelism,
        conflict_edges: dag.conflict_edges.len(),
        serial_worker_ms,
        parallel_worker_ms,
        staging_ms,
        idle_capacity_ms,
        parallel_efficiency_milli: u16::try_from(efficiency).unwrap_or(1_000),
        wall_time_improvement_milli: u16::try_from(improvement).unwrap_or(1_000),
        conflict_rate_milli: u16::try_from(conflict_rate).unwrap_or(1_000),
    }
}

fn reset_staging_for_replay(worktree: &Path, base_head: &str) -> Result<(), String> {
    let _ = hidden_command("git")
        .args(["cherry-pick", "--abort"])
        .current_dir(worktree)
        .output();
    for args in [vec!["reset", "--hard", base_head], vec!["clean", "-fd"]] {
        let output = hidden_command("git")
            .args(&args)
            .current_dir(worktree)
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "could not reset parallel staging for deterministic replay: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    }
    Ok(())
}

fn cherry_pick_without_commit(worktree: &Path, commit: &str) -> Result<(), String> {
    let output = hidden_command("git")
        .args(["cherry-pick", "--no-commit", commit])
        .current_dir(worktree)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn cleanup_staging(manager: &WorkerManager, worker: &Worker, base_head: &str) {
    let _ = hidden_command("git")
        .args(["cherry-pick", "--abort"])
        .current_dir(&worker.worktree)
        .output();
    let _ = hidden_command("git")
        .args(["reset", "--hard", base_head])
        .current_dir(&worker.worktree)
        .output();
    let _ = manager.cleanup(std::slice::from_ref(worker));
}

fn component_paths(components: &[ChangedComponent]) -> Vec<String> {
    let mut paths = components
        .iter()
        .flat_map(ChangedComponent::all_paths)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn batch_root(execution_root: &Path, dag_fingerprint: &str) -> PathBuf {
    execution_root.join("parallel-batch").join(dag_fingerprint)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_report_parallel_gain_efficiency_idle_and_conflicts() {
        let parallel = ParallelImplementationEvidence {
            dag_fingerprint: "dag".to_owned(),
            children: Vec::new(),
            task_elapsed_ms: [("a".to_owned(), 100), ("b".to_owned(), 100)]
                .into_iter()
                .collect(),
            parallel_elapsed_ms: 110,
            wave_count: 1,
            peak_parallelism: 2,
        };
        let dag = MutationDag {
            schema_version: 1,
            repository_revision: "base".to_owned(),
            tasks: Vec::new(),
            conflict_edges: Vec::new(),
            max_parallelism: 2,
            fingerprint: "dag".to_owned(),
        };
        let metrics = parallel_metrics(&dag, &parallel, 7);
        assert_eq!(metrics.serial_worker_ms, 200);
        assert_eq!(metrics.parallel_worker_ms, 110);
        assert_eq!(metrics.wall_time_improvement_milli, 450);
        assert_eq!(metrics.parallel_efficiency_milli, 909);
        assert_eq!(metrics.idle_capacity_ms, 20);
        assert_eq!(metrics.staging_ms, 7);
    }
}
