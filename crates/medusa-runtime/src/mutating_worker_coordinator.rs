//! Production execution for worktree-isolated mutating implementers.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
};

use medusa_agent::{
    AgentEngine, AgentExecutionPolicy, AgentUpdate, StepOutcome, TeamMemberContext, TeamRole,
    TeamRuntime, WorkerExecutionController, authoritative_verification_for_components_at,
    prepare_components_for_verification,
};
use medusa_evidence::{ChangedComponent, VerificationReceipt, changed_scope_fingerprint};
use medusa_config::{Config, Mode};
use medusa_multi_agent_scheduler::{Task, TaskState, Worker as ScheduledWorker};
use medusa_multi_agent_scheduler::speculation::{
    InvalidationReason, PromotionCheck, SpeculationAssumptions, SpeculationHistory,
    SpeculationLedger, SpeculationState, policy_for as speculation_policy_for,
};
use medusa_provider::ConfiguredProvider;
use medusa_workers::{Worker, WorkerManager};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    RuntimeActivity, RuntimeActivityKind, RuntimeEvent,
    multi_agent_coordinator::{CoordinatorEvidence, WorkerEvidence},
    mutation_transaction::{MutationTransaction, PreparedMutationInput},
    production_orchestrator::{
        AgentContract, AgentRole, ContextPacket, ProductionExecutionPlan, context_for_task,
    },
    team_control::{TeamControlPlane, TeamWorkerRegistration},
};

#[path = "mutating_worker_failure.rs"]
mod failure;
#[path = "mutating_worker_coordinator_support.rs"]
mod support;

use failure::record_attempt_failure;
use support::{
    dependency_outputs, evidence_from_state, implementation_contract, implementation_task,
    implementation_worker_label, load_state, now_ms, validate_changed_paths, validate_preflight,
    validate_state, write_atomic,
};

const LEASE_TIMEOUT_MS: u64 = 10 * 60 * 1_000;
const IMPLEMENTER_TURN_LIMIT: u32 = 24;
const MAX_ATTEMPTS: u32 = 2;
const IMPLEMENTER_ID: &str = "worker-implement";

fn bounded_implementer_turns(configured: u32) -> u32 {
    configured.clamp(1, IMPLEMENTER_TURN_LIMIT)
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

fn has_in_scope_repository_changes(
    worker: &Worker,
    allowed_write_paths: &[String],
) -> Result<bool, String> {
    if allowed_write_paths.is_empty() {
        return Ok(false);
    }
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all", "--"])
        .args(allowed_write_paths)
        .current_dir(&worker.worktree)
        .output()
        .map_err(|error| {
            format!(
                "failed to inspect bounded implementer worktree {}: {error}",
                worker.worktree.display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "git status failed while inspecting bounded implementer worktree: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(!output.stdout.is_empty())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ImplementationStatus {
    Running,
    Retrying,
    Prepared,
    Integrated,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DurableImplementationState {
    plan_fingerprint: String,
    repository_fingerprint: String,
    base_head: String,
    lease_epoch: u64,
    status: ImplementationStatus,
    worker: Worker,
    context_fingerprint: String,
    session_id: String,
    turns: u32,
    summary: String,
    changed_paths: Vec<String>,
    #[serde(default)]
    changed_components: Vec<ChangedComponent>,
    verification_evidence: Vec<String>,
    #[serde(default)]
    verification_receipt: Option<VerificationReceipt>,
    #[serde(default)]
    transaction_path: PathBuf,
    last_error: Option<String>,
    #[serde(default)]
    speculative: bool,
    #[serde(default)]
    speculation_ledger_path: PathBuf,
    #[serde(default)]
    speculation_assumptions_fingerprint: String,
    #[serde(default)]
    speculation_branch: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImplementationEvidence {
    pub plan_fingerprint: String,
    pub repository_fingerprint: String,
    pub task_id: String,
    pub worker_id: String,
    pub session_id: String,
    pub turns: u32,
    pub summary: String,
    pub changed_paths: Vec<String>,
    pub changed_components: Vec<ChangedComponent>,
    pub verification_evidence: Vec<String>,
    pub verification_receipt: VerificationReceipt,
    pub base_head: String,
    pub prepared_commit: String,
    pub prepared_tree: String,
    pub patch_fingerprint: String,
    pub review_context: String,
    pub transaction_path: PathBuf,
    pub state_path: PathBuf,
}

#[derive(Clone)]
struct ImplementationRequest {
    contract: AgentContract,
    packet: ContextPacket,
    worker: Worker,
    team_context: TeamMemberContext,
    control: TeamControlPlane,
    events: Sender<RuntimeEvent>,
    max_model_turns: u32,
}

#[derive(Clone, Debug)]
struct WorkerRun {
    session_id: String,
    turns: u32,
    summary: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpeculationPreparation {
    Skipped { reason: String },
    Prepared { candidate: String, turns: u32, elapsed_ms: u64 },
    Discarded { reason: String },
}

const SPECULATION_INVALIDATED_PREFIX: &str = "speculation invalidated before promotion:";

#[must_use]
pub fn is_speculation_invalidation(error: &str) -> bool {
    error.starts_with(SPECULATION_INVALIDATED_PREFIX)
}

#[derive(Clone, Debug)]
pub struct ParallelChildEvidence {
    pub task_id: String,
    pub evidence: ImplementationEvidence,
}

#[derive(Clone, Debug)]
pub struct ParallelImplementationEvidence {
    pub dag_fingerprint: String,
    pub children: Vec<ParallelChildEvidence>,
}

#[allow(clippy::too_many_arguments)]
pub fn run_parallel_implementations(
    repo: &Path,
    config: &Config,
    session_api_key: Option<String>,
    plan: &ProductionExecutionPlan,
    preflight: &CoordinatorEvidence,
    dag: &medusa_multi_agent_scheduler::mutation_dag::MutationDag,
    cancel: &Arc<AtomicBool>,
    events: &Sender<RuntimeEvent>,
) -> Result<ParallelImplementationEvidence, String> {
    dag.validate().map_err(str::to_owned)?;
    validate_preflight(plan, preflight)?;
    let mut completed = std::collections::BTreeSet::new();
    let mut accepted = std::collections::BTreeMap::new();
    while completed.len() < dag.tasks.len() {
        if cancel.load(Ordering::SeqCst) {
            return Err("parallel mutation batch was cancelled before dispatch".to_owned());
        }
        let wave = dag.runnable_wave(&completed);
        if wave.is_empty() {
            return Err("parallel mutation DAG has no runnable conflict-free wave".to_owned());
        }
        let results = std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(wave.len());
            for task_id in &wave {
                let task = dag
                    .tasks
                    .iter()
                    .find(|task| task.id == *task_id)
                    .ok_or_else(|| format!("parallel mutation task {task_id} disappeared"))?;
                let (child_plan, child_preflight) =
                    crate::parallel_mutation::child_execution(plan, preflight, task)?;
                let api_key = session_api_key.clone();
                let task_id = task_id.clone();
                let events = events.clone();
                handles.push(scope.spawn(move || {
                    let control = TeamControlPlane::default();
                    run_implementation(
                        repo,
                        config,
                        api_key,
                        &child_plan,
                        &child_preflight,
                        cancel,
                        (&control, &events),
                    )
                    .map(|evidence| (task_id, evidence))
                }));
            }
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| "parallel implementer thread terminated unexpectedly".to_owned())?
                })
                .collect::<Result<Vec<_>, String>>()
        })?;
        for (task_id, evidence) in results {
            completed.insert(task_id.clone());
            accepted.insert(task_id, evidence);
        }
    }
    let children = dag
        .deterministic_integration_order()
        .into_iter()
        .map(|task_id| {
            let evidence = accepted
                .remove(&task_id)
                .ok_or_else(|| format!("parallel mutation evidence missing for {task_id}"))?;
            Ok(ParallelChildEvidence { task_id, evidence })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(ParallelImplementationEvidence {
        dag_fingerprint: dag.fingerprint.clone(),
        children,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn run_implementation(
    repo: &Path,
    config: &Config,
    session_api_key: Option<String>,
    plan: &ProductionExecutionPlan,
    preflight: &CoordinatorEvidence,
    cancel: &Arc<AtomicBool>,
    reporting: (&TeamControlPlane, &Sender<RuntimeEvent>),
) -> Result<ImplementationEvidence, String> {
    let (control, events) = reporting;
    coordinate_with_control(
        repo,
        plan,
        preflight,
        cancel,
        control,
        events,
        None,
        |request| execute_production_implementer(config, session_api_key.clone(), cancel, request),
    )
}

/// Prepares at most one disposable implementation while authoritative preflight continues.
///
/// The resulting mutation transaction remains review-pending and has no integration authority.
/// A later normal `run_implementation` call must promote it against the completed dependency
/// evidence before the candidate can enter parent review.
pub fn run_speculative_implementation(
    repo: &Path,
    config: &Config,
    session_api_key: Option<String>,
    plan: &ProductionExecutionPlan,
    cancel: &Arc<AtomicBool>,
    reporting: (&TeamControlPlane, &Sender<RuntimeEvent>),
) -> Result<SpeculationPreparation, String> {
    let (control, events) = reporting;
    let policy = speculation_policy_for(&plan.planning);
    if !policy.eligible {
        return Ok(SpeculationPreparation::Skipped {
            reason: policy.rationale,
        });
    }
    let history_path = speculation_history_path(repo);
    let history = SpeculationHistory::load(&history_path)?;
    if !history.allows_speculation() {
        return Ok(SpeculationPreparation::Skipped {
            reason: "historical speculative waste exceeds retained useful work".to_owned(),
        });
    }
    let repository_fingerprint =
        crate::multi_agent_coordinator::repository_fingerprint(repo)?;
    let root = crate::multi_agent_coordinator::execution_root(
        repo,
        &plan.fingerprint,
        &repository_fingerprint,
    );
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let ledger_path = root.join("speculation-ledger.json");
    let mut ledger = SpeculationLedger::open_or_create(
        &ledger_path,
        &policy,
        repository_fingerprint.clone(),
    )?;
    match &ledger.record().state {
        SpeculationState::Prepared { candidate_fingerprint } => {
            return Ok(SpeculationPreparation::Prepared {
                candidate: candidate_fingerprint.clone(),
                turns: ledger.record().model_turns,
                elapsed_ms: ledger.record().elapsed_ms,
            });
        }
        SpeculationState::Promoted { candidate_fingerprint } => {
            return Ok(SpeculationPreparation::Prepared {
                candidate: candidate_fingerprint.clone(),
                turns: ledger.record().model_turns,
                elapsed_ms: ledger.record().elapsed_ms,
            });
        }
        SpeculationState::Invalidated { detail, .. } | SpeculationState::Discarded { detail } => {
            return Ok(SpeculationPreparation::Skipped {
                reason: detail.clone(),
            });
        }
        SpeculationState::Running => {
            if ledger.recover_interrupted()? {
                update_speculation_history(repo, ledger.record())?;
                discard_speculative_artifacts(repo, &root)?;
                return Ok(SpeculationPreparation::Discarded {
                    reason: "interrupted speculative work was recovered fail-closed".to_owned(),
                });
            }
        }
        SpeculationState::Proposed => ledger.begin()?,
    }
    let assumptions = policy
        .assumptions
        .as_ref()
        .ok_or_else(|| "eligible speculation policy has no assumptions".to_owned())?;
    let preflight = speculative_preflight(plan, &repository_fingerprint, &root, assumptions)?;
    let context = SpeculativeExecutionContext {
        ledger_path: ledger_path.clone(),
        assumptions_fingerprint: assumptions.fingerprint.clone(),
        branch: current_branch(repo)?,
        max_model_turns: policy.budget.max_model_turns,
    };
    let started = std::time::Instant::now();
    let result = coordinate_with_control(
        repo,
        plan,
        &preflight,
        cancel,
        control,
        events,
        Some(&context),
        |request| execute_production_implementer(config, session_api_key.clone(), cancel, request),
    );
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match result {
        Ok(evidence) => {
            ledger.account(evidence.turns, u64::from(evidence.turns), elapsed_ms)?;
            if matches!(ledger.record().state, SpeculationState::Invalidated { .. }) {
                update_speculation_history(repo, ledger.record())?;
                discard_speculative_artifacts(repo, &root)?;
                return Ok(SpeculationPreparation::Discarded {
                    reason: "speculative resource budget was exceeded".to_owned(),
                });
            }
            ledger.prepared(evidence.prepared_commit.clone())?;
            let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                id: Some(plan.fingerprint.clone()),
                kind: RuntimeActivityKind::Done,
                title: "Speculative implementation prepared".to_owned(),
                details: vec![
                    format!("candidate={}", evidence.prepared_commit),
                    format!("turns={}", evidence.turns),
                    format!("elapsed_ms={elapsed_ms}"),
                    "candidate has no integration authority until full preflight promotion"
                        .to_owned(),
                ],
            }));
            Ok(SpeculationPreparation::Prepared {
                candidate: evidence.prepared_commit,
                turns: evidence.turns,
                elapsed_ms,
            })
        }
        Err(error) => {
            let reason = if cancel.load(Ordering::SeqCst) {
                InvalidationReason::Cancellation
            } else {
                InvalidationReason::ConflictingEvidence
            };
            let _ = ledger.account(0, 0, elapsed_ms);
            let _ = ledger.invalidate(reason, error.clone());
            update_speculation_history(repo, ledger.record())?;
            discard_speculative_artifacts(repo, &root)?;
            let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                id: Some(plan.fingerprint.clone()),
                kind: RuntimeActivityKind::Progress,
                title: "Speculative implementation discarded".to_owned(),
                details: vec![error.clone()],
            }));
            Ok(SpeculationPreparation::Discarded { reason: error })
        }
    }
}

#[cfg(test)]
fn coordinate_with_executor<F>(
    repo: &Path,
    plan: &ProductionExecutionPlan,
    preflight: &CoordinatorEvidence,
    cancel: &Arc<AtomicBool>,
    events: &Sender<RuntimeEvent>,
    executor: F,
) -> Result<ImplementationEvidence, String>
where
    F: Fn(ImplementationRequest) -> Result<WorkerRun, String>,
{
    coordinate_with_control(
        repo,
        plan,
        preflight,
        cancel,
        &TeamControlPlane::default(),
        events,
        None,
        executor,
    )
}

#[derive(Clone, Debug)]
struct SpeculativeExecutionContext {
    ledger_path: PathBuf,
    assumptions_fingerprint: String,
    branch: String,
    max_model_turns: u32,
}

fn speculation_history_path(repo: &Path) -> PathBuf {
    repo.join(".medusa")
        .join("speculation")
        .join("medium-risk-resolved-mutation-history.json")
}

fn update_speculation_history(
    repo: &Path,
    record: &medusa_multi_agent_scheduler::speculation::SpeculationRecord,
) -> Result<(), String> {
    let path = speculation_history_path(repo);
    let mut history = SpeculationHistory::load(&path)?;
    history.observe(record);
    history.persist(&path)
}

fn current_branch(repo: &Path) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(repo)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "failed to resolve speculative repository branch: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if branch.is_empty() {
        return Err("speculative repository branch cannot be empty".to_owned());
    }
    Ok(branch)
}

fn speculative_preflight(
    plan: &ProductionExecutionPlan,
    repository_fingerprint: &str,
    root: &Path,
    assumptions: &SpeculationAssumptions,
) -> Result<CoordinatorEvidence, String> {
    let contract = implementation_contract(plan)?;
    if contract.dependencies.len() < 2 {
        return Err("speculative implementation requires at least two promotion dependencies".to_owned());
    }
    let mut workers = Vec::with_capacity(contract.dependencies.len());
    for dependency in &contract.dependencies {
        let role = plan
            .contracts
            .iter()
            .find(|candidate| candidate.task_id == *dependency)
            .map(|candidate| candidate.role)
            .ok_or_else(|| format!("speculative dependency {dependency} has no contract"))?;
        if !matches!(role, AgentRole::Planner | AgentRole::Researcher) {
            return Err(format!(
                "speculative dependency {dependency} is not a read-only preliminary task"
            ));
        }
        workers.push(WorkerEvidence {
            task_id: dependency.clone(),
            worker_id: format!("speculative-assumption-{dependency}"),
            role,
            context_fingerprint: assumptions.fingerprint.clone(),
            lease_epoch: 1,
            session_id: format!("speculative-assumption-{dependency}"),
            turns: 0,
            summary: format!(
                "Provisional assumption for `{dependency}` only. This is not authoritative dependency evidence and grants no integration authority. Full preflight must confirm this assumption before promotion."
            ),
        });
    }
    Ok(CoordinatorEvidence {
        plan_fingerprint: plan.fingerprint.clone(),
        repository_fingerprint: repository_fingerprint.to_owned(),
        workers,
        state_path: root.join("preflight-evidence.json"),
    })
}

fn preflight_promotion_conflict(preflight: &CoordinatorEvidence) -> Option<String> {
    const CONFLICT_MARKERS: &[&str] = &[
        "scope must broaden",
        "scope expansion required",
        "public api change required",
        "security-sensitive change required",
        "dependency change required",
        "conflicting evidence",
        "stale repository graph",
        "capability unavailable",
        "cannot confirm resolved scope",
    ];
    preflight.workers.iter().find_map(|worker| {
        let summary = worker.summary.to_ascii_lowercase();
        CONFLICT_MARKERS
            .iter()
            .find(|marker| summary.contains(**marker))
            .map(|marker| format!("{} reported promotion conflict marker `{marker}`", worker.task_id))
    })
}

#[allow(clippy::too_many_arguments)]
fn promote_speculative_state(
    repo: &Path,
    plan: &ProductionExecutionPlan,
    preflight: &CoordinatorEvidence,
    contract: &AgentContract,
    packet: &ContextPacket,
    state_path: &Path,
    state: &mut DurableImplementationState,
    events: &Sender<RuntimeEvent>,
) -> Result<(), String> {
    if state.speculation_ledger_path.as_os_str().is_empty()
        || state.speculation_assumptions_fingerprint.trim().is_empty()
        || state.speculation_branch.trim().is_empty()
    {
        return Err("prepared speculative state is missing promotion provenance".to_owned());
    }
    let mut ledger = SpeculationLedger::load(&state.speculation_ledger_path)?;
    if ledger.record().assumptions.fingerprint != state.speculation_assumptions_fingerprint {
        let _ = ledger.invalidate(
            InvalidationReason::PromotionMismatch,
            "implementation state assumptions do not match durable speculation ledger",
        );
        update_speculation_history(repo, ledger.record())?;
        return Err("speculation assumptions fingerprint changed".to_owned());
    }
    if let Some(conflict) = preflight_promotion_conflict(preflight) {
        let _ = ledger.invalidate(InvalidationReason::RiskEscalated, conflict.clone());
        update_speculation_history(repo, ledger.record())?;
        return Err(conflict);
    }
    let current_repository_fingerprint =
        crate::multi_agent_coordinator::repository_fingerprint(repo)?;
    if current_repository_fingerprint != preflight.repository_fingerprint {
        let detail = "primary repository changed while speculative work was running".to_owned();
        let _ = ledger.invalidate(InvalidationReason::RepositoryDrift, detail.clone());
        update_speculation_history(repo, ledger.record())?;
        return Err(detail);
    }
    if current_branch(repo)? != state.speculation_branch {
        let detail = "primary repository branch changed while speculative work was running".to_owned();
        let _ = ledger.invalidate(InvalidationReason::RepositoryDrift, detail.clone());
        update_speculation_history(repo, ledger.record())?;
        return Err(detail);
    }
    let planned = plan
        .planning
        .task(medusa_multi_agent_scheduler::TaskKind::Implementation)
        .ok_or_else(|| "promotion plan has no implementation task".to_owned())?;
    let candidate = state
        .worker
        .commit
        .clone()
        .ok_or_else(|| "prepared speculation has no immutable candidate commit".to_owned())?;
    let check = PromotionCheck {
        plan_fingerprint: plan.fingerprint.clone(),
        repository_fingerprint: preflight.repository_fingerprint.clone(),
        repository_scope: contract.allowed_write_paths.clone(),
        dependency_ids: contract.dependencies.clone(),
        task_context_fingerprint: planned.context_fingerprint.clone(),
        candidate_fingerprint: candidate.clone(),
    };
    if let Err(reason) = ledger.promotion_decision(&check) {
        let detail = format!("promotion evidence mismatch: {reason:?}");
        let _ = ledger.invalidate(reason, detail.clone());
        update_speculation_history(repo, ledger.record())?;
        return Err(detail);
    }
    let retained_ms = ledger.record().elapsed_ms;
    ledger.promote(&check, retained_ms)?;
    update_speculation_history(repo, ledger.record())?;
    state.speculative = false;
    state.context_fingerprint = packet.fingerprint.clone();
    write_atomic(state_path, state)?;
    let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
        id: Some(plan.fingerprint.clone()),
        kind: RuntimeActivityKind::Done,
        title: "Speculative candidate promoted".to_owned(),
        details: vec![
            format!("candidate={candidate}"),
            format!("retained_useful_ms={retained_ms}"),
            "full dependency evidence, scope, repository, branch, and task context matched"
                .to_owned(),
        ],
    }));
    Ok(())
}

fn discard_speculative_artifacts(repo: &Path, root: &Path) -> Result<(), String> {
    let state_path = root.join("implementation-state.json");
    if state_path.is_file() {
        if let Ok(state) = load_state(&state_path) {
            let manager = WorkerManager::new(repo, root.join("worktrees"))
                .map_err(|error| error.to_string())?;
            manager
                .cleanup(std::slice::from_ref(&state.worker))
                .map_err(|error| error.to_string())?;
        }
    }
    for path in [
        state_path,
        root.join("implementation-worker-execution.json"),
        root.join("implementation-team.json"),
        root.join("mutation-transaction.json"),
        root.join("prepared.patch"),
    ] {
        if path.is_file() {
            fs::remove_file(path).map_err(|error| error.to_string())?;
        }
    }
    let speculative_evidence = root.join("evidence/worktree");
    if speculative_evidence.is_dir() {
        fs::remove_dir_all(speculative_evidence).map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn coordinate_with_control<F>(
    repo: &Path,
    plan: &ProductionExecutionPlan,
    preflight: &CoordinatorEvidence,
    cancel: &Arc<AtomicBool>,
    control: &TeamControlPlane,
    events: &Sender<RuntimeEvent>,
    speculative: Option<&SpeculativeExecutionContext>,
    executor: F,
) -> Result<ImplementationEvidence, String>
where
    F: Fn(ImplementationRequest) -> Result<WorkerRun, String>,
{
    validate_preflight(plan, preflight)?;
    let contract = implementation_contract(plan)?;
    let task = implementation_task(plan, &contract)?;
    let execution_id = control
        .snapshot()
        .execution_id
        .unwrap_or_else(|| plan.fingerprint.clone());
    let _ = events.send(RuntimeEvent::Team(control.begin(
        execution_id,
        [TeamWorkerRegistration {
            worker_id: IMPLEMENTER_ID.to_owned(),
            role: "implementer".to_owned(),
            task_id: contract.task_id.clone(),
        }],
    )));
    let dependency_outputs = dependency_outputs(&contract, preflight)?;
    let packet = context_for_task(
        plan,
        &contract.task_id,
        dependency_outputs,
        vec![
            "mutations are confined to a dedicated Git worktree".to_owned(),
            "runtime-enforced implementer tool policy".to_owned(),
            "changed paths must remain within the contract write scope".to_owned(),
            "no user interaction".to_owned(),
        ],
        contract.required_evidence.clone(),
    )?;
    let root = preflight
        .state_path
        .parent()
        .ok_or_else(|| "preflight evidence path has no execution root".to_owned())?;
    fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let state_path = root.join("implementation-state.json");
    let controller_path = root.join("implementation-worker-execution.json");
    if cancel.load(Ordering::SeqCst) {
        return Err("mutating worker execution was cancelled before resource creation".to_owned());
    }
    let worktree_root = root.join("worktrees");
    let manager = WorkerManager::new(repo, &worktree_root).map_err(|error| error.to_string())?;
    let team_path = root.join("implementation-team.json");
    let team = if team_path.is_file() {
        TeamRuntime::load(&team_path)?
    } else {
        TeamRuntime::create(
            &team_path,
            format!("{}-implementation", plan.fingerprint),
            vec![
                ("lead".to_owned(), TeamRole::Lead),
                (IMPLEMENTER_ID.to_owned(), TeamRole::Implementer),
            ],
        )?
    };

    if state_path.is_file() {
        let mut state = load_state(&state_path)?;
        if speculative.is_none()
            && state.speculative
            && state.status == ImplementationStatus::Prepared
        {
            if let Err(error) = promote_speculative_state(
                repo,
                plan,
                preflight,
                &contract,
                &packet,
                &state_path,
                &mut state,
                events,
            ) {
                discard_speculative_artifacts(repo, root)?;
                return Err(format!("{SPECULATION_INVALIDATED_PREFIX} {error}"));
            }
        }
        validate_state(plan, preflight, &packet, &state)?;
        match state.status {
            ImplementationStatus::Integrated => {
                return Err(
                    "legacy implementation state integrated before parent review and cannot be resumed as a v2 transaction"
                        .to_owned(),
                );
            }
            ImplementationStatus::Prepared => {
                return complete_prepared(
                    &manager,
                    &mut WorkerExecutionController::load(&controller_path)?,
                    &team,
                    &state_path,
                    &contract.task_id,
                    state,
                    (control, events),
                );
            }
            ImplementationStatus::Running | ImplementationStatus::Retrying => {
                let _ = manager.cleanup(std::slice::from_ref(&state.worker));
                let mut controller = WorkerExecutionController::load(&controller_path)?;
                controller.recover_interrupted()?;
                return execute_attempts(
                    repo,
                    plan,
                    preflight,
                    cancel,
                    events,
                    control,
                    &executor,
                    &manager,
                    &team,
                    &state_path,
                    controller,
                    contract,
                    packet,
                    state.base_head,
                    speculative,
                    None,
                );
            }
            ImplementationStatus::Failed => {
                return Err(state
                    .last_error
                    .unwrap_or_else(|| "mutating worker execution previously failed".to_owned()));
            }
        }
    }

    if cancel.load(Ordering::SeqCst) {
        team.request_shutdown_all()?;
        return Err("mutating worker execution was cancelled before resource creation".to_owned());
    }
    manager.require_clean().map_err(|error| error.to_string())?;
    let base_head = manager
        .repository_head()
        .map_err(|error| error.to_string())?;
    let worker_label = implementation_worker_label(plan, &contract);
    let worker = manager
        .open_or_create_worker(&worker_label, IMPLEMENTER_ID)
        .map_err(|error| error.to_string())?;
    let controller = if controller_path.is_file() {
        let mut controller = WorkerExecutionController::load(&controller_path)?;
        controller.recover_interrupted()?;
        controller
    } else {
        WorkerExecutionController::create(
            &controller_path,
            format!("{}-implementation", plan.fingerprint),
            vec![Task {
                dependencies: Vec::new(),
                ..task
            }],
            vec![ScheduledWorker {
                id: IMPLEMENTER_ID.to_owned(),
                capabilities: vec!["coding".to_owned()],
                healthy: true,
                capacity: 1,
            }],
            vec![worker.clone()],
            MAX_ATTEMPTS,
        )?
    };
    execute_attempts(
        repo,
        plan,
        preflight,
        cancel,
        events,
        control,
        &executor,
        &manager,
        &team,
        &state_path,
        controller,
        contract,
        packet,
        base_head,
        speculative,
        Some(worker),
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_attempts<F>(
    _repo: &Path,
    plan: &ProductionExecutionPlan,
    preflight: &CoordinatorEvidence,
    cancel: &Arc<AtomicBool>,
    events: &Sender<RuntimeEvent>,
    control: &TeamControlPlane,
    executor: &F,
    manager: &WorkerManager,
    team: &TeamRuntime,
    state_path: &Path,
    mut controller: WorkerExecutionController,
    contract: AgentContract,
    packet: ContextPacket,
    base_head: String,
    speculative: Option<&SpeculativeExecutionContext>,
    mut initial_worker: Option<Worker>,
) -> Result<ImplementationEvidence, String>
where
    F: Fn(ImplementationRequest) -> Result<WorkerRun, String>,
{
    if manager
        .repository_head()
        .map_err(|error| error.to_string())?
        != base_head
    {
        if let Some(worker) = initial_worker.as_ref() {
            let _ = manager.cleanup(std::slice::from_ref(worker));
        }
        return Err("primary repository HEAD changed after implementation planning".to_owned());
    }
    let mut last_error = None;
    for attempt in 1..=MAX_ATTEMPTS {
        if cancel.load(Ordering::SeqCst) || control.is_cancelled(IMPLEMENTER_ID) {
            if let Some(worker) = initial_worker.as_ref() {
                manager
                    .cleanup(std::slice::from_ref(worker))
                    .map_err(|error| error.to_string())?;
            }
            team.request_shutdown_all()?;
            return Err("mutating worker execution was cancelled before dispatch".to_owned());
        }
        let assignments = controller.dispatch(now_ms()?, LEASE_TIMEOUT_MS)?;
        if assignments.len() != 1 {
            return Err(format!(
                "implementation scheduler expected one assignment, received {}",
                assignments.len()
            ));
        }
        let assignment = assignments[0].clone();
        let worker = match initial_worker.take() {
            Some(worker) => worker,
            None => manager
                .open_or_create_worker(
                    &implementation_worker_label(plan, &contract),
                    IMPLEMENTER_ID,
                )
                .map_err(|error| error.to_string())?,
        };
        let team_context = team.member_context(&assignment.worker_id)?;
        team.start_member(&assignment.worker_id, &assignment.task_id, "starting")?;
        if let Ok(snapshot) = control.start(
            &assignment.worker_id,
            None,
            format!("implementation attempt {attempt} dispatched"),
        ) {
            let _ = events.send(RuntimeEvent::Team(snapshot));
        }
        let running = DurableImplementationState {
            plan_fingerprint: plan.fingerprint.clone(),
            repository_fingerprint: preflight.repository_fingerprint.clone(),
            base_head: base_head.clone(),
            lease_epoch: assignment.lease_epoch,
            status: ImplementationStatus::Running,
            worker: worker.clone(),
            context_fingerprint: packet.fingerprint.clone(),
            session_id: String::new(),
            turns: 0,
            summary: String::new(),
            changed_paths: Vec::new(),
            changed_components: Vec::new(),
            verification_evidence: Vec::new(),
            verification_receipt: None,
            transaction_path: state_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("mutation-transaction.json"),
            last_error: last_error.clone(),
            speculative: speculative.is_some(),
            speculation_ledger_path: speculative
                .map(|context| context.ledger_path.clone())
                .unwrap_or_default(),
            speculation_assumptions_fingerprint: speculative
                .map(|context| context.assumptions_fingerprint.clone())
                .unwrap_or_default(),
            speculation_branch: speculative
                .map(|context| context.branch.clone())
                .unwrap_or_default(),
        };
        write_atomic(state_path, &running)?;
        let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
            id: Some(plan.fingerprint.clone()),
            kind: RuntimeActivityKind::Progress,
            title: "Isolated implementer dispatched".to_owned(),
            details: vec![
                format!("{} -> {}", assignment.task_id, assignment.worker_id),
                format!("worktree={}", worker.worktree.display()),
                format!("lease_epoch={}", assignment.lease_epoch),
            ],
        }));

        let request = ImplementationRequest {
            contract: contract.clone(),
            packet: packet.clone(),
            worker: worker.clone(),
            team_context,
            control: control.clone(),
            events: events.clone(),
            max_model_turns: speculative
                .map_or_else(
                    || u32::from(plan.planning.model_turn_budget.successful_path_total),
                    |context| context.max_model_turns,
                ),
        };
        let run = match executor(request) {
            Ok(run) => run,
            Err(error) => {
                let cancelled = cancel.load(Ordering::SeqCst);
                let retryable = !cancelled && attempt < MAX_ATTEMPTS;
                let recorded = record_attempt_failure(
                    &mut controller,
                    team,
                    events,
                    control,
                    manager,
                    state_path,
                    &assignment,
                    &worker,
                    running,
                    None,
                    Vec::new(),
                    Vec::new(),
                    error,
                    retryable,
                    cancelled,
                )?;
                last_error = Some(recorded.clone());
                if retryable {
                    continue;
                }
                return Err(recorded);
            }
        };

        if let Err(error) = manager.discard_untracked_runtime_state(&worker, &base_head) {
            let retryable = attempt < MAX_ATTEMPTS;
            let recorded = record_attempt_failure(
                &mut controller,
                team,
                events,
                control,
                manager,
                state_path,
                &assignment,
                &worker,
                running,
                Some(&run),
                Vec::new(),
                Vec::new(),
                format!("failed to discard isolated runtime state: {error}"),
                retryable,
                false,
            )?;
            last_error = Some(recorded.clone());
            if retryable {
                continue;
            }
            return Err(recorded);
        }
        let changed_components = match manager.changed_components_since(&worker, &base_head) {
            Ok(components) => components,
            Err(error) => {
                let retryable = attempt < MAX_ATTEMPTS;
                let recorded = record_attempt_failure(
                    &mut controller,
                    team,
                    events,
                    control,
                    manager,
                    state_path,
                    &assignment,
                    &worker,
                    running,
                    Some(&run),
                    Vec::new(),
                    Vec::new(),
                    format!("failed to inspect isolated changes: {error}"),
                    retryable,
                    false,
                )?;
                last_error = Some(recorded.clone());
                if retryable {
                    continue;
                }
                return Err(recorded);
            }
        };
        let changed_paths = component_paths(&changed_components);
        if let Err(error) = validate_changed_paths(&contract, &changed_paths) {
            let retryable = attempt < MAX_ATTEMPTS;
            let recorded = record_attempt_failure(
                &mut controller,
                team,
                events,
                control,
                manager,
                state_path,
                &assignment,
                &worker,
                running,
                Some(&run),
                changed_paths,
                Vec::new(),
                error,
                retryable,
                false,
            )?;
            last_error = Some(recorded.clone());
            if retryable {
                continue;
            }
            return Err(recorded);
        }
        if let Err(error) =
            prepare_components_for_verification(&worker.worktree, &changed_components)
        {
            let retryable = attempt < MAX_ATTEMPTS;
            let recorded = record_attempt_failure(
                &mut controller,
                team,
                events,
                control,
                manager,
                state_path,
                &assignment,
                &worker,
                running,
                Some(&run),
                changed_paths,
                Vec::new(),
                format!("trusted preparation failed: {error}"),
                retryable,
                false,
            )?;
            last_error = Some(recorded.clone());
            if retryable {
                continue;
            }
            return Err(recorded);
        }
        let prepared_components = match manager.changed_components_since(&worker, &base_head) {
            Ok(components) => components,
            Err(error) => {
                let retryable = attempt < MAX_ATTEMPTS;
                let recorded = record_attempt_failure(
                    &mut controller,
                    team,
                    events,
                    control,
                    manager,
                    state_path,
                    &assignment,
                    &worker,
                    running,
                    Some(&run),
                    changed_paths,
                    Vec::new(),
                    format!("failed to inspect prepared changes: {error}"),
                    retryable,
                    false,
                )?;
                last_error = Some(recorded.clone());
                if retryable {
                    continue;
                }
                return Err(recorded);
            }
        };
        if changed_scope_fingerprint(&prepared_components)
            != changed_scope_fingerprint(&changed_components)
        {
            let error = format!(
                "trusted preparation changed repository scope: before={changed_components:?}; after={prepared_components:?}"
            );
            let retryable = attempt < MAX_ATTEMPTS;
            let recorded = record_attempt_failure(
                &mut controller,
                team,
                events,
                control,
                manager,
                state_path,
                &assignment,
                &worker,
                running,
                Some(&run),
                component_paths(&prepared_components),
                Vec::new(),
                error,
                retryable,
                false,
            )?;
            last_error = Some(recorded.clone());
            if retryable {
                continue;
            }
            return Err(recorded);
        }
        let changed_components = prepared_components;
        let changed_paths = component_paths(&changed_components);
        let worktree_identity = format!(
            "worktree:{}:{}",
            base_head,
            changed_scope_fingerprint(&changed_components)
        );
        let evidence_root = state_path
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
            Ok(verification) => verification,
            Err(error) => {
                let retryable = attempt < MAX_ATTEMPTS;
                let recorded = record_attempt_failure(
                    &mut controller,
                    team,
                    events,
                    control,
                    manager,
                    state_path,
                    &assignment,
                    &worker,
                    running,
                    Some(&run),
                    changed_paths,
                    Vec::new(),
                    format!("isolated verification could not run: {error}"),
                    retryable,
                    false,
                )?;
                last_error = Some(recorded.clone());
                if retryable {
                    continue;
                }
                return Err(recorded);
            }
        };
        if !verification.receipt.passed {
            let error = format!(
                "isolated worktree verification failed: {}",
                verification.summary.join(" | ")
            );
            let retryable = attempt < MAX_ATTEMPTS;
            let recorded = record_attempt_failure(
                &mut controller,
                team,
                events,
                control,
                manager,
                state_path,
                &assignment,
                &worker,
                running,
                Some(&run),
                changed_paths,
                verification.summary,
                error,
                retryable,
                false,
            )?;
            last_error = Some(recorded.clone());
            if retryable {
                continue;
            }
            return Err(recorded);
        }
        let verification_summary = verification.summary.clone();
        let verification_receipt = verification.receipt;
        if let Err(error) = manager.discard_untracked_runtime_state(&worker, &base_head) {
            let retryable = attempt < MAX_ATTEMPTS;
            let recorded = record_attempt_failure(
                &mut controller,
                team,
                events,
                control,
                manager,
                state_path,
                &assignment,
                &worker,
                running,
                Some(&run),
                changed_paths,
                verification_summary,
                format!("failed to clean runtime state after verification: {error}"),
                retryable,
                false,
            )?;
            last_error = Some(recorded.clone());
            if retryable {
                continue;
            }
            return Err(recorded);
        }
        let verified_components = match manager.changed_components_since(&worker, &base_head) {
            Ok(components) => components,
            Err(error) => {
                let retryable = attempt < MAX_ATTEMPTS;
                let recorded = record_attempt_failure(
                    &mut controller,
                    team,
                    events,
                    control,
                    manager,
                    state_path,
                    &assignment,
                    &worker,
                    running,
                    Some(&run),
                    changed_paths,
                    verification_summary,
                    format!("failed to inspect changes after verification: {error}"),
                    retryable,
                    false,
                )?;
                last_error = Some(recorded.clone());
                if retryable {
                    continue;
                }
                return Err(recorded);
            }
        };
        if changed_scope_fingerprint(&verified_components)
            != changed_scope_fingerprint(&changed_components)
        {
            let error = format!(
                "verification mutated the isolated worktree scope: before={changed_components:?}; after={verified_components:?}"
            );
            let retryable = attempt < MAX_ATTEMPTS;
            let recorded = record_attempt_failure(
                &mut controller,
                team,
                events,
                control,
                manager,
                state_path,
                &assignment,
                &worker,
                running,
                Some(&run),
                component_paths(&verified_components),
                verification_summary,
                error,
                retryable,
                false,
            )?;
            last_error = Some(recorded.clone());
            if retryable {
                continue;
            }
            return Err(recorded);
        }

        let finalized = match manager.finalize_worker(
            worker.clone(),
            &base_head,
            &format!("Medusa implement {}", contract.task_id),
        ) {
            Ok(finalized) => finalized,
            Err(error) => {
                let retryable = attempt < MAX_ATTEMPTS;
                let recorded = record_attempt_failure(
                    &mut controller,
                    team,
                    events,
                    control,
                    manager,
                    state_path,
                    &assignment,
                    &worker,
                    running,
                    Some(&run),
                    changed_paths,
                    verification_summary.clone(),
                    format!("failed to finalize isolated worker commit: {error}"),
                    retryable,
                    false,
                )?;
                last_error = Some(recorded.clone());
                if retryable {
                    continue;
                }
                return Err(recorded);
            }
        };
        let prepared = DurableImplementationState {
            plan_fingerprint: plan.fingerprint.clone(),
            repository_fingerprint: preflight.repository_fingerprint.clone(),
            base_head: base_head.clone(),
            lease_epoch: assignment.lease_epoch,
            status: ImplementationStatus::Prepared,
            worker: finalized,
            context_fingerprint: packet.fingerprint.clone(),
            session_id: run.session_id,
            turns: run.turns,
            summary: run.summary,
            changed_paths,
            changed_components,
            verification_evidence: verification_summary,
            verification_receipt: Some(verification_receipt),
            transaction_path: state_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("mutation-transaction.json"),
            last_error: None,
            speculative: speculative.is_some(),
            speculation_ledger_path: speculative
                .map(|context| context.ledger_path.clone())
                .unwrap_or_default(),
            speculation_assumptions_fingerprint: speculative
                .map(|context| context.assumptions_fingerprint.clone())
                .unwrap_or_default(),
            speculation_branch: speculative
                .map(|context| context.branch.clone())
                .unwrap_or_default(),
        };
        write_atomic(state_path, &prepared)?;
        return complete_prepared(
            manager,
            &mut controller,
            team,
            state_path,
            &contract.task_id,
            prepared,
            (control, events),
        );
    }
    Err(last_error.unwrap_or_else(|| "mutating worker execution exhausted attempts".to_owned()))
}

#[allow(clippy::too_many_arguments)]
fn complete_prepared(
    manager: &WorkerManager,
    controller: &mut WorkerExecutionController,
    team: &TeamRuntime,
    state_path: &Path,
    task_id: &str,
    mut state: DurableImplementationState,
    reporting: (&TeamControlPlane, &Sender<RuntimeEvent>),
) -> Result<ImplementationEvidence, String> {
    let (control, events) = reporting;
    let commit = state
        .worker
        .commit
        .as_deref()
        .ok_or_else(|| "prepared worker state has no commit".to_owned())?;
    let needs_completion = match controller
        .task_views()
        .into_iter()
        .find(|view| view.task.id == task_id)
        .map(|view| view.state)
    {
        Some(TaskState::Running { worker_id, .. }) if worker_id == state.worker.id => true,
        Some(TaskState::Succeeded) => false,
        Some(other) => {
            return Err(format!(
                "prepared implementation does not match durable task state: {other:?}"
            ));
        }
        None => return Err("prepared implementation task is missing from durable state".to_owned()),
    };
    let root = state_path
        .parent()
        .ok_or_else(|| "implementation state path has no execution root".to_owned())?;
    let transaction = MutationTransaction::open_or_prepare(
        root,
        manager.repository_path(),
        PreparedMutationInput {
            plan_fingerprint: state.plan_fingerprint.clone(),
            repository_fingerprint: state.repository_fingerprint.clone(),
            task_id: task_id.to_owned(),
            base_head: state.base_head.clone(),
            worker: state.worker.clone(),
            changed_paths: state.changed_paths.clone(),
            changed_components: state.changed_components.clone(),
            implementation_summary: state.summary.clone(),
            worktree_verification_evidence: state.verification_evidence.clone(),
            worktree_verification_receipt: state
                .verification_receipt
                .clone()
                .ok_or_else(|| "prepared implementation has no typed verification receipt".to_owned())?,
            speculative: state.speculative,
        },
    )?;
    if transaction.snapshot().prepared_commit != commit {
        return Err("durable transaction commit does not match prepared worker".to_owned());
    }
    if needs_completion {
        controller.accept_persisted_completion(task_id, &state.worker.id, state.lease_epoch)?;
    }
    state.status = ImplementationStatus::Prepared;
    state.transaction_path = transaction.path().to_path_buf();
    write_atomic(state_path, &state)?;
    team.finish_member(&state.worker.id, false)?;
    team.member_context(&state.worker.id)?
        .execute(
            "team_send_message",
            &json!({
                "recipient":"lead",
                "body":format!(
                    "{} prepared immutable commit {} for parent review",
                    task_id, commit
                )
            }),
        )
        .map_err(|error| error.to_string())?;
    if let Ok(snapshot) = control.progress(
        &state.worker.id,
        Some(state.session_id.as_str()),
        state.turns,
        format!("prepared commit {commit}; awaiting parent review"),
    ) {
        let _ = events.send(RuntimeEvent::Team(snapshot));
    }
    transaction.emit(events);
    evidence_from_state(state_path, task_id, &state)
}

fn execute_production_implementer(
    config: &Config,
    session_api_key: Option<String>,
    cancel: &Arc<AtomicBool>,
    request: ImplementationRequest,
) -> Result<WorkerRun, String> {
    let mut worker_config = config.clone();
    worker_config.agent.mode = Mode::Yolo;
    worker_config.agent.max_turns = bounded_implementer_turns(worker_config.agent.max_turns)
        .min(request.max_model_turns.max(1));
    let provider = ConfiguredProvider::manager_from_config(&worker_config, session_api_key)
        .map_err(|error| error.to_string())?;
    let policy = AgentExecutionPolicy::for_team_role(TeamRole::Implementer)
        .with_allowed_write_paths(request.contract.allowed_write_paths.clone());
    let engine =
        AgentEngine::new_with_cancellation(provider, worker_config.clone(), Arc::clone(cancel))
            .with_execution_policy(policy)
            .with_team_context(request.team_context.clone());
    let objective = format!(
        "Implement delegated task `{}` inside this isolated Git worktree. Objective: {}. Stay within allowed write paths {:?}. These paths are exact contract boundaries: do not create sibling files, package metadata, or convenience files outside them; report any genuinely required out-of-scope change instead. Use the hard {}-turn model budget efficiently: batch independent reads, make every required product edit, run focused verification, and then return a concise evidence-backed summary. Do not ask the user questions and do not modify tests or fixtures merely to make failures disappear.",
        request.contract.task_id,
        request.contract.objective,
        request.contract.allowed_write_paths,
        request.max_model_turns,
    );
    let mut session = engine
        .create_session(&request.worker.worktree, objective)
        .map_err(|error| error.to_string())?;
    request.team_context.clone().execute(
        "team_send_message",
        &json!({"recipient":"lead","body":format!("{} implementation started", request.contract.task_id)}),
    )
    .map_err(|error| error.to_string())?;
    let system_context = format!(
        "Authoritative delegation packet fingerprint: {}\n{}",
        request.packet.fingerprint,
        serde_json::to_string_pretty(&request.packet).map_err(|error| error.to_string())?
    );
    let mut summaries = Vec::new();
    let mut completed = false;
    if let Ok(snapshot) = request.control.start(
        &request.worker.id,
        Some(session.id.as_str()),
        "implementer model session started",
    ) {
        let _ = request.events.send(RuntimeEvent::Team(snapshot));
    }
    for _ in 0..worker_config.agent.max_turns {
        if cancel.load(Ordering::SeqCst) || request.control.is_cancelled(&request.worker.id) {
            return Err(format!("worker {} was cancelled", request.worker.id));
        }
        let mut turn_context = system_context.clone();
        if let Some(instruction) = request.control.take_instruction(&request.worker.id)? {
            turn_context.push_str("\n\nLive steering instruction from the lead: ");
            turn_context.push_str(&instruction);
        }
        if let Ok(snapshot) = request.control.progress(
            &request.worker.id,
            Some(session.id.as_str()),
            session.turn,
            "running implementation turn",
        ) {
            let _ = request.events.send(RuntimeEvent::Team(snapshot));
        }
        let outcome = engine
            .step_with_observer_and_context(&mut session, Some(&turn_context), |update| {
                if let AgentUpdate::AssistantText(text) = update
                    && !text.trim().is_empty()
                {
                    summaries.push(text.clone());
                }
            })
            .map_err(|error| error.to_string())?;
        if let Ok(snapshot) = request.control.progress(
            &request.worker.id,
            Some(session.id.as_str()),
            session.turn,
            "implementation turn completed",
        ) {
            let _ = request.events.send(RuntimeEvent::Team(snapshot));
        }
        match outcome {
            StepOutcome::Continue => {}
            StepOutcome::TurnComplete | StepOutcome::Completed => {
                completed = true;
                break;
            }
            StepOutcome::WaitingForUser => {
                return Err(format!(
                    "worker {} attempted to request user input",
                    request.worker.id
                ));
            }
        }
    }
    let mut summary = summaries.join("\n").trim().to_owned();
    if !completed {
        if !has_in_scope_repository_changes(
            &request.worker,
            &request.contract.allowed_write_paths,
        )? {
            return Err(format!(
                "worker {} exceeded its bounded turn budget without producing an in-scope repository change",
                request.worker.id
            ));
        }
        let fallback = "The model exhausted its bounded turn budget after producing an in-scope repository change. Runtime exact-scope validation and independent verification are authoritative; this narrative is advisory.";
        if summary.is_empty() {
            summary = fallback.to_owned();
        } else {
            summary.push_str("\n\n");
            summary.push_str(fallback);
        }
    }
    if summary.is_empty() {
        return Err(format!(
            "worker {} returned no implementation evidence",
            request.worker.id
        ));
    }
    Ok(WorkerRun {
        session_id: session.id.to_string(),
        turns: session.turn,
        summary,
    })
}

#[cfg(test)]
#[path = "mutating_worker_coordinator_tests.rs"]
mod tests;
