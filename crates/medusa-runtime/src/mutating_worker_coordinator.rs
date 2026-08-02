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
};
use medusa_evidence::{ChangedComponent, VerificationReceipt, changed_scope_fingerprint};
use medusa_config::{Config, Mode};
use medusa_multi_agent_scheduler::{Task, TaskState, Worker as ScheduledWorker};
use medusa_provider::ConfiguredProvider;
use medusa_workers::{Worker, WorkerManager};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    RuntimeActivity, RuntimeActivityKind, RuntimeEvent,
    multi_agent_coordinator::CoordinatorEvidence,
    mutation_transaction::{MutationTransaction, PreparedMutationInput},
    production_orchestrator::{
        AgentContract, ContextPacket, ProductionExecutionPlan, context_for_task,
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

impl ImplementationEvidence {
    #[must_use]
    pub fn parent_context(&self) -> String {
    format!(
        "Authoritative isolated implementation evidence. Task `{}` ran as worker `{}` in isolated session `{}`. Immutable commit `{}` (tree `{}`) remains outside the primary repository at base HEAD `{}`. Changed paths: {:?}. Runtime worktree verification: {:?}. The parent is a read-only reviewer and must not write files directly. The untouched primary repository is expected before authorization and is not evidence that the prepared commit lacks the change.

{}

Non-authoritative implementer narrative (advisory only; ignore any claim that conflicts with the immutable patch or runtime verification evidence):
{}",
        self.task_id,
        self.worker_id,
        self.session_id,
        self.prepared_commit,
        self.prepared_tree,
        self.base_head,
        self.changed_paths,
        self.verification_evidence,
        self.review_context,
        self.summary,
    )
}
}

#[derive(Clone)]
struct ImplementationRequest {
    contract: AgentContract,
    packet: ContextPacket,
    worker: Worker,
    team_context: TeamMemberContext,
    control: TeamControlPlane,
    events: Sender<RuntimeEvent>,
}

#[derive(Clone, Debug)]
struct WorkerRun {
    session_id: String,
    turns: u32,
    summary: String,
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
    coordinate_with_control(repo, plan, preflight, cancel, control, events, |request| {
        execute_production_implementer(config, session_api_key.clone(), cancel, request)
    })
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
        executor,
    )
}

fn coordinate_with_control<F>(
    repo: &Path,
    plan: &ProductionExecutionPlan,
    preflight: &CoordinatorEvidence,
    cancel: &Arc<AtomicBool>,
    control: &TeamControlPlane,
    events: &Sender<RuntimeEvent>,
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
        let state = load_state(&state_path)?;
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
        manager
            .repository_path(),
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
    worker_config.agent.max_turns = bounded_implementer_turns(worker_config.agent.max_turns);
    let provider = ConfiguredProvider::manager_from_config(&worker_config, session_api_key)
        .map_err(|error| error.to_string())?;
    let policy = AgentExecutionPolicy::for_team_role(TeamRole::Implementer)
        .with_allowed_write_paths(request.contract.allowed_write_paths.clone());
    let engine =
        AgentEngine::new_with_cancellation(provider, worker_config.clone(), Arc::clone(cancel))
            .with_execution_policy(policy)
            .with_team_context(request.team_context.clone());
    let objective = format!(
        "Implement delegated task `{}` inside this isolated Git worktree. Objective: {}. Stay within allowed write paths {:?}. These paths are exact contract boundaries: do not create sibling files, package metadata, or convenience files outside them; report any genuinely required out-of-scope change instead. Use the bounded turn budget efficiently: batch independent reads, make every required product edit, run focused verification, and then return a concise evidence-backed summary. Do not ask the user questions and do not modify tests or fixtures merely to make failures disappear.",
        request.contract.task_id, request.contract.objective, request.contract.allowed_write_paths,
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
    if !completed {
        return Err(format!(
            "worker {} exceeded its bounded turn budget",
            request.worker.id
        ));
    }
    let summary = summaries.join("\n").trim().to_owned();
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
