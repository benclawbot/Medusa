//! Production coordination for bounded, durable read-only teammate execution.
//!
//! Mutating workers are intentionally not dispatched here. They require the isolated
//! worktree integration path. This coordinator establishes the production team lifecycle,
//! leases, parallel agent sessions, durable evidence handoff, and final verification gate.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use medusa_agent::{
    AgentEngine, AgentExecutionPolicy, AgentUpdate, StepOutcome, TeamMemberContext, TeamRole,
    TeamRuntime, WorkerExecutionController,
};
use medusa_config::{Config, Mode};
use medusa_multi_agent_scheduler::{ExecutionLane, Task, TaskState, Worker as ScheduledWorker};
use medusa_provider::ConfiguredProvider;
use medusa_workers::{Worker, WorkerState};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    RuntimeActivity, RuntimeActivityKind, RuntimeEvent,
    production_orchestrator::{
        AgentContract, AgentRole, ContextPacket, ProductionExecutionPlan, context_for_task,
        validate_subagent_result,
    },
    team_control::{TeamControlPlane, TeamWorkerRegistration},
};

const LEASE_TIMEOUT_MS: u64 = 5 * 60 * 1_000;
const WORKER_TURN_LIMIT: u32 = 12;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkerEvidence {
    pub task_id: String,
    pub worker_id: String,
    pub role: AgentRole,
    pub context_fingerprint: String,
    #[serde(default)]
    pub lease_epoch: u64,
    pub session_id: String,
    pub turns: u32,
    pub summary: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CoordinatorEvidence {
    pub plan_fingerprint: String,
    pub repository_fingerprint: String,
    pub workers: Vec<WorkerEvidence>,
    pub state_path: PathBuf,
}

impl CoordinatorEvidence {
    #[must_use]
    pub fn parent_context(&self) -> String {
        let rendered = self
            .workers
            .iter()
            .map(|worker| {
                format!(
                    "## {} ({:?}, session {})\n{}",
                    worker.task_id, worker.role, worker.session_id, worker.summary
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        format!(
            "Authoritative read-only teammate evidence for plan {} and repository snapshot {}. The parent remains responsible for all mutations, integration, and verification.\n\n{}",
            self.plan_fingerprint, self.repository_fingerprint, rendered
        )
    }
}

#[derive(Clone)]
struct WorkerRequest {
    contract: AgentContract,
    packet: ContextPacket,
    worker_id: String,
    team_context: TeamMemberContext,
    control: TeamControlPlane,
    events: Sender<RuntimeEvent>,
}

pub fn run_preflight(
    repo: &Path,
    config: &Config,
    session_api_key: Option<String>,
    plan: &ProductionExecutionPlan,
    cancel: &Arc<AtomicBool>,
    control: &TeamControlPlane,
    events: &Sender<RuntimeEvent>,
) -> Result<CoordinatorEvidence, String> {
    coordinate_with_control(repo, plan, cancel, control, events, |request| {
        execute_production_worker(repo, config, session_api_key.clone(), cancel, request)
    })
}

pub fn run_deterministic_fast_preflight(
    repo: &Path,
    plan: &ProductionExecutionPlan,
    control: &TeamControlPlane,
    events: &Sender<RuntimeEvent>,
) -> Result<CoordinatorEvidence, String> {
    if plan.planning.lane != ExecutionLane::FastMutation
        || !plan.planning.uses_deterministic_preflight()
    {
        return Err("deterministic preflight requires the fast mutation lane".to_owned());
    }
    let repository_fingerprint = repository_fingerprint(repo)?;
    let root = execution_root(repo, &plan.fingerprint, &repository_fingerprint);
    let evidence_path = root.join("preflight-evidence.json");
    if evidence_path.is_file() {
        let restored: CoordinatorEvidence =
            serde_json::from_slice(&fs::read(&evidence_path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        validate_evidence(plan, &repository_fingerprint, &evidence_path, &restored)?;
        return Ok(restored);
    }

    let contracts = preflight_contracts(plan);
    if contracts.len() < 2 {
        return Err("fast mutation requires deterministic analysis and risk contracts".to_owned());
    }
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let execution_key = execution_id(&plan.fingerprint, &repository_fingerprint);
    let _ = events.send(RuntimeEvent::Team(control.begin(
        execution_key,
        contracts.iter().map(|contract| TeamWorkerRegistration {
            worker_id: worker_id_for(&contract.task_id),
            role: format!("{:?}", contract.role).to_ascii_lowercase(),
            task_id: contract.task_id.clone(),
        }),
    )));

    let mut workers = Vec::with_capacity(contracts.len());
    for contract in contracts {
        let packet = context_for_task(
            plan,
            &contract.task_id,
            BTreeMap::new(),
            vec![
                "read-only repository access".to_owned(),
                "runtime-enforced role policy".to_owned(),
                "no user interaction".to_owned(),
            ],
            contract.required_evidence.clone(),
        )?;
        let summary = match contract.role {
            AgentRole::Planner => format!(
                "Deterministic fast-lane scope accepted: exact write paths {:?}; affected components {:?}; required capabilities {:?}. No planning model request was used.",
                plan.planning.scope.effective,
                plan.planning.affected_components,
                plan.planning.required_capabilities,
            ),
            AgentRole::Researcher => format!(
                "Deterministic fast-lane risk policy accepted at confidence {}/1000. Any unexpected path, public API or dependency change, security-sensitive file, failed verification, or repeated patch attempt invalidates this authorization and requires escalation.",
                plan.planning.confidence_milli,
            ),
            _ => return Err("fast preflight received a non-read-only contract".to_owned()),
        };
        let worker = WorkerEvidence {
            task_id: contract.task_id.clone(),
            worker_id: worker_id_for(&contract.task_id),
            role: contract.role,
            context_fingerprint: packet.fingerprint,
            lease_epoch: 1,
            session_id: format!(
                "deterministic-fast-{}-{}",
                contract.task_id,
                plan.fingerprint.chars().take(12).collect::<String>()
            ),
            turns: 0,
            summary,
        };
        let contracts_by_task = BTreeMap::from([(contract.task_id.clone(), contract.clone())]);
        validate_worker_evidence(plan, &contracts_by_task, &worker)?;
        if let Ok(snapshot) = control.complete(&worker.worker_id, "deterministic fast-lane evidence") {
            let _ = events.send(RuntimeEvent::Team(snapshot));
        }
        workers.push(worker);
    }
    workers.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    let evidence = CoordinatorEvidence {
        plan_fingerprint: plan.fingerprint.clone(),
        repository_fingerprint,
        workers,
        state_path: evidence_path.clone(),
    };
    validate_evidence(
        plan,
        &evidence.repository_fingerprint,
        &evidence_path,
        &evidence,
    )?;
    write_atomic(&evidence_path, &evidence)?;
    let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
        id: Some(plan.fingerprint.clone()),
        kind: RuntimeActivityKind::Done,
        title: "Fast-lane deterministic preflight accepted".to_owned(),
        details: vec![
            "zero planning or risk-review model turns".to_owned(),
            format!("scope={:?}", plan.planning.scope.effective),
            format!("budget={:?}", plan.planning.model_turn_budget),
        ],
    }));
    Ok(evidence)
}

#[cfg(test)]
fn coordinate_with_executor<F>(
    repo: &Path,
    plan: &ProductionExecutionPlan,
    cancel: &Arc<AtomicBool>,
    events: &Sender<RuntimeEvent>,
    executor: F,
) -> Result<CoordinatorEvidence, String>
where
    F: Fn(WorkerRequest) -> Result<WorkerEvidence, String> + Sync,
{
    coordinate_with_control(
        repo,
        plan,
        cancel,
        &TeamControlPlane::default(),
        events,
        executor,
    )
}

fn coordinate_with_control<F>(
    repo: &Path,
    plan: &ProductionExecutionPlan,
    cancel: &Arc<AtomicBool>,
    control: &TeamControlPlane,
    events: &Sender<RuntimeEvent>,
    executor: F,
) -> Result<CoordinatorEvidence, String>
where
    F: Fn(WorkerRequest) -> Result<WorkerEvidence, String> + Sync,
{
    let repository_fingerprint = repository_fingerprint(repo)?;
    let root = execution_root(repo, &plan.fingerprint, &repository_fingerprint);
    let evidence_path = root.join("preflight-evidence.json");
    let execution_key = execution_id(&plan.fingerprint, &repository_fingerprint);
    if evidence_path.is_file() {
        let restored: CoordinatorEvidence =
            serde_json::from_slice(&fs::read(&evidence_path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        validate_evidence(plan, &repository_fingerprint, &evidence_path, &restored)?;
        let _ = events.send(RuntimeEvent::Team(
            control.begin(
                execution_key,
                restored
                    .workers
                    .iter()
                    .map(|worker| TeamWorkerRegistration {
                        worker_id: worker.worker_id.clone(),
                        role: format!("{:?}", worker.role).to_ascii_lowercase(),
                        task_id: worker.task_id.clone(),
                    }),
            ),
        ));
        for worker in &restored.workers {
            if let Ok(snapshot) = control.complete(&worker.worker_id, "durable evidence restored") {
                let _ = events.send(RuntimeEvent::Team(snapshot));
            }
        }
        if !crate::production_orchestrator::requires_mutation(plan) {
            let _ = events.send(RuntimeEvent::Team(control.finish()));
        }
        return Ok(restored);
    }

    let contracts = preflight_contracts(plan);
    if contracts.len() < 2 {
        return Err(
            "coordinated execution requires at least two independent preflight tasks".into(),
        );
    }
    let _ = events.send(RuntimeEvent::Team(control.begin(
        execution_key.clone(),
        contracts.iter().map(|contract| TeamWorkerRegistration {
            worker_id: worker_id_for(&contract.task_id),
            role: format!("{:?}", contract.role).to_ascii_lowercase(),
            task_id: contract.task_id.clone(),
        }),
    )));
    let contract_by_task = contracts
        .iter()
        .map(|contract| (contract.task_id.clone(), contract.clone()))
        .collect::<BTreeMap<_, _>>();
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;

    let team_members = std::iter::once(("lead".to_owned(), TeamRole::Lead))
        .chain(contracts.iter().map(|contract| {
            (
                worker_id_for(&contract.task_id),
                team_role_for(contract.role),
            )
        }))
        .collect::<Vec<_>>();
    let team_path = root.join("team.json");
    let execution_id = execution_key;
    let team = if team_path.is_file() {
        TeamRuntime::load(&team_path)?
    } else {
        TeamRuntime::create(&team_path, &execution_id, team_members)?
    };

    let tasks = contracts
        .iter()
        .map(|contract| {
            plan.tasks
                .iter()
                .find(|task| task.id == contract.task_id)
                .cloned()
                .ok_or_else(|| format!("task {} is missing from the plan", contract.task_id))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let scheduled_workers = contracts
        .iter()
        .map(|contract| ScheduledWorker {
            id: worker_id_for(&contract.task_id),
            capabilities: task_capabilities(&tasks, &contract.task_id),
            healthy: true,
            capacity: 1,
        })
        .collect::<Vec<_>>();
    let worker_records = scheduled_workers
        .iter()
        .map(|worker| Worker {
            id: worker.id.clone(),
            branch: "read-only".to_owned(),
            worktree: repo.to_path_buf(),
            state: WorkerState::Ready,
            commit: None,
            stdout: String::new(),
            stderr: String::new(),
        })
        .collect::<Vec<_>>();
    let controller_path = root.join("worker-execution.json");
    let mut controller = if controller_path.is_file() {
        WorkerExecutionController::load(&controller_path)?
    } else {
        WorkerExecutionController::create(
            &controller_path,
            &execution_id,
            tasks,
            scheduled_workers,
            worker_records,
            2,
        )?
    };

    let evidence_directory = root.join("worker-evidence");
    let mut evidence = load_worker_evidence(&evidence_directory)?;
    for worker in &evidence {
        validate_worker_evidence(plan, &contract_by_task, worker)?;
        let state = controller
            .task_views()
            .into_iter()
            .find(|view| view.task.id == worker.task_id)
            .map(|view| view.state)
            .ok_or_else(|| {
                format!(
                    "persisted evidence references unknown task {}",
                    worker.task_id
                )
            })?;
        match state {
            TaskState::Running { worker_id, .. } if worker_id == worker.worker_id => {
                controller.accept_persisted_completion(
                    &worker.task_id,
                    &worker.worker_id,
                    worker.lease_epoch,
                )?;
            }
            TaskState::Succeeded => {}
            _ => {
                return Err(format!(
                    "persisted evidence for {} does not match durable task state",
                    worker.task_id
                ));
            }
        }
        team.start_member(&worker.worker_id, &worker.task_id, &worker.session_id)?;
        team.finish_member(&worker.worker_id, false)?;
    }
    controller.recover_interrupted()?;

    if cancel.load(Ordering::SeqCst) {
        team.request_shutdown_all()?;
        return Err("coordinated preflight was cancelled".into());
    }
    let now = now_ms()?;
    let assignments = controller.dispatch(now, LEASE_TIMEOUT_MS)?;
    if assignments.len() + evidence.len() != contracts.len() {
        return Err(
            "preflight scheduler and durable evidence do not cover every independent task".into(),
        );
    }
    let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
        id: Some(plan.fingerprint.clone()),
        kind: RuntimeActivityKind::Progress,
        title: "Read-only teammates dispatched".to_owned(),
        details: assignments
            .iter()
            .map(|assignment| format!("{} -> {}", assignment.task_id, assignment.worker_id))
            .collect(),
    }));

    let mut requests = Vec::with_capacity(assignments.len());
    for assignment in &assignments {
        let contract = contract_by_task
            .get(&assignment.task_id)
            .cloned()
            .ok_or_else(|| format!("missing contract for {}", assignment.task_id))?;
        let packet = context_for_task(
            plan,
            &assignment.task_id,
            BTreeMap::new(),
            vec![
                "read-only repository access".to_owned(),
                "runtime-enforced role policy".to_owned(),
                "no user interaction".to_owned(),
            ],
            contract.required_evidence.clone(),
        )?;
        let team_context = team.member_context(&assignment.worker_id)?;
        team.start_member(&assignment.worker_id, &assignment.task_id, "starting")?;
        if let Ok(snapshot) = control.start(&assignment.worker_id, None, "worker dispatched") {
            let _ = events.send(RuntimeEvent::Team(snapshot));
        }
        requests.push(WorkerRequest {
            contract,
            packet,
            worker_id: assignment.worker_id.clone(),
            team_context,
            control: control.clone(),
            events: events.clone(),
        });
    }

    let results = thread::scope(|scope| {
        let handles = requests
            .into_iter()
            .map(|request| {
                let task_id = request.contract.task_id.clone();
                let worker_id = request.worker_id.clone();
                let executor = &executor;
                (task_id, worker_id, scope.spawn(move || executor(request)))
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|(task_id, worker_id, handle)| {
                let result = handle
                    .join()
                    .map_err(|_| "read-only teammate thread panicked".to_owned())?;
                Ok((task_id, worker_id, result))
            })
            .collect::<Result<Vec<_>, String>>()
    })?;

    let mut failures = Vec::new();
    for (task_id, worker_id, result) in results {
        let assignment = assignments
            .iter()
            .find(|assignment| assignment.task_id == task_id)
            .ok_or_else(|| format!("worker result for unknown task {task_id}"))?;
        let mut result = match result {
            Ok(result) => result,
            Err(error) => {
                controller.fail(&task_id, &worker_id, assignment.lease_epoch, &error, true)?;
                team.finish_member(&worker_id, true)?;
                if let Ok(snapshot) = control.fail(&worker_id, error.clone()) {
                    let _ = events.send(RuntimeEvent::Team(snapshot));
                }
                failures.push(format!("{task_id} ({worker_id}): {error}"));
                continue;
            }
        };
        let contract = contract_by_task
            .get(&task_id)
            .ok_or_else(|| format!("missing contract for {task_id}"))?;
        let packet = context_for_task(
            plan,
            &task_id,
            BTreeMap::new(),
            vec![
                "read-only repository access".to_owned(),
                "runtime-enforced role policy".to_owned(),
                "no user interaction".to_owned(),
            ],
            contract.required_evidence.clone(),
        )?;
        let validation = if result.task_id != task_id
            || result.worker_id != worker_id
            || result.role != contract.role
            || result.session_id.trim().is_empty()
        {
            Err("worker evidence identity did not match its leased assignment".to_owned())
        } else {
            validate_subagent_result(
                &packet,
                &result.task_id,
                &result.context_fingerprint,
                std::slice::from_ref(&result.summary),
            )
            .map_err(str::to_owned)
        };
        if let Err(error) = validation {
            controller.fail(&task_id, &worker_id, assignment.lease_epoch, &error, true)?;
            team.finish_member(&worker_id, true)?;
            if let Ok(snapshot) = control.fail(&worker_id, error.clone()) {
                let _ = events.send(RuntimeEvent::Team(snapshot));
            }
            failures.push(format!("{task_id} ({worker_id}): {error}"));
            continue;
        }
        result.lease_epoch = assignment.lease_epoch;
        write_atomic(
            &evidence_directory.join(format!("{}.json", result.task_id)),
            &result,
        )?;
        if let Err(error) = controller.accept_persisted_completion(
            &result.task_id,
            &result.worker_id,
            result.lease_epoch,
        ) {
            controller.fail(&task_id, &worker_id, assignment.lease_epoch, &error, true)?;
            team.finish_member(&worker_id, true)?;
            if let Ok(snapshot) = control.fail(&worker_id, error.clone()) {
                let _ = events.send(RuntimeEvent::Team(snapshot));
            }
            failures.push(format!("{task_id} ({worker_id}): {error}"));
            continue;
        }
        team.start_member(&result.worker_id, &result.task_id, &result.session_id)?;
        team.finish_member(&result.worker_id, false)?;
        if let Ok(snapshot) = control.complete(&result.worker_id, "evidence accepted") {
            let _ = events.send(RuntimeEvent::Team(snapshot));
        }
        team.member_context(&result.worker_id)?
            .execute(
                "team_send_message",
                &json!({"recipient":"lead","body":result.summary.clone()}),
            )
            .map_err(|error| error.to_string())?;
        evidence.push(result);
    }
    if !failures.is_empty() {
        team.request_shutdown_all()?;
        return Err(format!(
            "read-only teammate execution failed: {}",
            failures.join(" | ")
        ));
    }
    evidence.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    if !controller.is_complete() || controller.has_terminal_failure() {
        return Err("preflight worker execution did not reach successful completion".into());
    }
    let persisted = CoordinatorEvidence {
        plan_fingerprint: plan.fingerprint.clone(),
        repository_fingerprint,
        workers: evidence,
        state_path: evidence_path.clone(),
    };
    write_atomic(&evidence_path, &persisted)?;
    if !crate::production_orchestrator::requires_mutation(plan) {
        let _ = events.send(RuntimeEvent::Team(control.finish()));
    }
    let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
        id: Some(plan.fingerprint.clone()),
        kind: RuntimeActivityKind::Done,
        title: "Read-only teammate evidence integrated".to_owned(),
        details: persisted
            .workers
            .iter()
            .map(|worker| format!("{}: {} turns", worker.task_id, worker.turns))
            .collect(),
    }));
    Ok(persisted)
}

fn execute_production_worker(
    repo: &Path,
    config: &Config,
    session_api_key: Option<String>,
    cancel: &Arc<AtomicBool>,
    request: WorkerRequest,
) -> Result<WorkerEvidence, String> {
    let mut worker_config = config.clone();
    worker_config.agent.mode = Mode::ReadOnly;
    worker_config.agent.max_turns = worker_config.agent.max_turns.clamp(1, WORKER_TURN_LIMIT);
    let provider = ConfiguredProvider::manager_from_config(&worker_config, session_api_key)
        .map_err(|error| error.to_string())?;
    let role = team_role_for(request.contract.role);
    let engine =
        AgentEngine::new_with_cancellation(provider, worker_config.clone(), Arc::clone(cancel))
            .with_execution_policy(AgentExecutionPolicy::for_team_role(role))
            .with_team_context(request.team_context.clone());
    let objective = format!(
        "Complete delegated read-only task `{}`. Objective: {}. Return a concise evidence-backed report; do not ask the user questions and do not modify repository state.",
        request.contract.task_id, request.contract.objective
    );
    let mut session = engine
        .create_session(repo, objective)
        .map_err(|error| error.to_string())?;
    request
        .team_context
        .clone()
        .execute(
            "team_send_message",
            &json!({"recipient":"lead","body":format!("{} started", request.contract.task_id)}),
        )
        .map_err(|error| error.to_string())?;
    let system_context = format!(
        "Delegation context fingerprint: {}\nContract: {}",
        request.packet.fingerprint,
        serde_json::to_string_pretty(&request.packet).map_err(|error| error.to_string())?
    );
    let mut summaries = Vec::new();
    let mut completed = false;
    if let Ok(snapshot) = request.control.start(
        &request.worker_id,
        Some(session.id.as_str()),
        "model session started",
    ) {
        let _ = request.events.send(RuntimeEvent::Team(snapshot));
    }
    for _ in 0..worker_config.agent.max_turns {
        if cancel.load(Ordering::SeqCst) || request.control.is_cancelled(&request.worker_id) {
            return Err(format!("worker {} was cancelled", request.worker_id));
        }
        let mut turn_context = system_context.clone();
        if let Some(instruction) = request.control.take_instruction(&request.worker_id)? {
            turn_context.push_str("\n\nLive steering instruction from the lead: ");
            turn_context.push_str(&instruction);
        }
        if let Ok(snapshot) = request.control.progress(
            &request.worker_id,
            Some(session.id.as_str()),
            session.turn,
            "running model turn",
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
            &request.worker_id,
            Some(session.id.as_str()),
            session.turn,
            "model turn completed",
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
                    request.worker_id
                ));
            }
        }
    }
    if !completed {
        return Err(format!(
            "worker {} exceeded its bounded turn budget",
            request.worker_id
        ));
    }
    let summary = summaries.join("\n").trim().to_owned();
    if summary.is_empty() {
        return Err(format!("worker {} returned no evidence", request.worker_id));
    }
    Ok(WorkerEvidence {
        task_id: request.contract.task_id,
        worker_id: request.worker_id,
        role: request.contract.role,
        context_fingerprint: request.packet.fingerprint,
        lease_epoch: 0,
        session_id: session.id.to_string(),
        turns: session.turn,
        summary,
    })
}

fn preflight_contracts(plan: &ProductionExecutionPlan) -> Vec<AgentContract> {
    let mut contracts = plan
        .contracts
        .iter()
        .filter(|contract| {
            contract.dependencies.is_empty()
                && !matches!(contract.role, AgentRole::Implementer | AgentRole::Verifier)
        })
        .cloned()
        .collect::<Vec<_>>();
    contracts.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    contracts
}

fn task_capabilities(tasks: &[Task], task_id: &str) -> Vec<String> {
    tasks
        .iter()
        .find(|task| task.id == task_id)
        .map_or_else(Vec::new, |task| task.capabilities.clone())
}

fn worker_id_for(task_id: &str) -> String {
    format!("worker-{task_id}")
}

fn team_role_for(role: AgentRole) -> TeamRole {
    match role {
        AgentRole::Planner => TeamRole::Planner,
        AgentRole::Researcher => TeamRole::Researcher,
        AgentRole::Implementer => TeamRole::Implementer,
        AgentRole::Reviewer => TeamRole::Reviewer,
        AgentRole::Verifier => TeamRole::Verifier,
    }
}

fn load_worker_evidence(directory: &Path) -> Result<Vec<WorkerEvidence>, String> {
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths = fs::read_dir(directory)
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())
        })
        .collect()
}

fn validate_worker_evidence(
    plan: &ProductionExecutionPlan,
    contracts: &BTreeMap<String, AgentContract>,
    evidence: &WorkerEvidence,
) -> Result<(), String> {
    let contract = contracts
        .get(&evidence.task_id)
        .ok_or_else(|| format!("evidence references unknown task {}", evidence.task_id))?;
    let packet = context_for_task(
        plan,
        &evidence.task_id,
        BTreeMap::new(),
        vec![
            "read-only repository access".to_owned(),
            "runtime-enforced role policy".to_owned(),
            "no user interaction".to_owned(),
        ],
        contract.required_evidence.clone(),
    )?;
    if evidence.worker_id != worker_id_for(&evidence.task_id)
        || evidence.role != contract.role
        || evidence.lease_epoch == 0
        || evidence.session_id.trim().is_empty()
    {
        return Err(format!(
            "evidence identity is invalid for {}",
            evidence.task_id
        ));
    }
    validate_subagent_result(
        &packet,
        &evidence.task_id,
        &evidence.context_fingerprint,
        std::slice::from_ref(&evidence.summary),
    )
    .map_err(str::to_owned)
}

fn execution_root(repo: &Path, plan_fingerprint: &str, repository_fingerprint: &str) -> PathBuf {
    repo.join(".medusa")
        .join("executions")
        .join(execution_id(plan_fingerprint, repository_fingerprint))
}

fn execution_id(plan_fingerprint: &str, repository_fingerprint: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(plan_fingerprint.as_bytes());
    digest.update([0]);
    digest.update(repository_fingerprint.as_bytes());
    format!("{:x}", digest.finalize())
}

fn validate_evidence(
    plan: &ProductionExecutionPlan,
    repository_fingerprint: &str,
    evidence_path: &Path,
    evidence: &CoordinatorEvidence,
) -> Result<(), String> {
    if evidence.plan_fingerprint != plan.fingerprint
        || evidence.repository_fingerprint != repository_fingerprint
        || evidence.state_path != evidence_path
        || evidence.workers.len() < 2
    {
        return Err("persisted coordinator evidence does not match the execution plan".into());
    }
    if evidence.workers.iter().any(|worker| {
        worker.summary.trim().is_empty()
            || worker.context_fingerprint.trim().is_empty()
            || worker.session_id.trim().is_empty()
    }) {
        return Err("persisted coordinator evidence is incomplete".into());
    }
    Ok(())
}

fn repository_fingerprint(repo: &Path) -> Result<String, String> {
    let paths = git_repository_paths(repo).or_else(|_| walk_repository_paths(repo))?;
    let mut digest = Sha256::new();
    for relative in paths {
        let normalized = relative.to_string_lossy().replace('\\', "/");
        if excluded_path(&normalized) {
            continue;
        }
        let path = repo.join(&relative);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("failed to inspect repository path {normalized}: {error}"))?;
        digest.update(normalized.as_bytes());
        digest.update([0]);
        if metadata.file_type().is_symlink() {
            digest.update(b"symlink");
            digest.update(
                fs::read_link(&path)
                    .map_err(|error| error.to_string())?
                    .to_string_lossy()
                    .as_bytes(),
            );
        } else if metadata.is_file() {
            digest.update(b"file");
            digest.update(fs::read(&path).map_err(|error| {
                format!("failed to read repository path {normalized}: {error}")
            })?);
        }
        digest.update([0xff]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn git_repository_paths(repo: &Path) -> Result<Vec<PathBuf>, String> {
    let output = Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(repo)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let mut paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(String::from_utf8_lossy(path).into_owned()))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn walk_repository_paths(repo: &Path) -> Result<Vec<PathBuf>, String> {
    fn visit(root: &Path, directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), String> {
        for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|error| error.to_string())?
                .to_path_buf();
            let normalized = relative.to_string_lossy().replace('\\', "/");
            if excluded_path(&normalized) {
                continue;
            }
            let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                visit(root, &path, paths)?;
            } else {
                paths.push(relative);
            }
        }
        Ok(())
    }

    let mut paths = Vec::new();
    visit(repo, repo, &mut paths)?;
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn excluded_path(path: &str) -> bool {
    matches!(
        path.split('/').next(),
        Some(".git" | ".medusa" | "target" | "node_modules")
    )
}

fn write_atomic(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "coordinator state path has no parent".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    fs::rename(temporary, path).map_err(|error| error.to_string())
}

fn now_ms() -> Result<u64, String> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_millis(),
    )
    .map_err(|_| "system clock does not fit in milliseconds".to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Barrier, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    use super::*;
    use crate::{production_orchestrator, prompt::PromptDraft};

    fn find_state_file(repo: &Path, file_name: &str) -> PathBuf {
        let executions = repo.join(".medusa").join("executions");
        fs::read_dir(executions)
            .expect("execution directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path().join(file_name))
            .find(|path| path.is_file())
            .expect("state file")
    }

    #[test]
    fn independent_workers_run_concurrently_and_evidence_survives_restart() {
        let repo = tempfile::tempdir().expect("repository");
        let plan = production_orchestrator::plan(&PromptDraft {
            text: "Implement a repository-wide refactor with tests".to_owned(),
            ..PromptDraft::default()
        })
        .expect("plan");
        let cancel = Arc::new(AtomicBool::new(false));
        let (events, _) = mpsc::channel();
        let barrier = Arc::new(Barrier::new(2));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let sessions = Arc::new(Mutex::new(0_u32));

        let first = coordinate_with_executor(repo.path(), &plan, &cancel, &events, {
            let barrier = Arc::clone(&barrier);
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            let calls = Arc::clone(&calls);
            let sessions = Arc::clone(&sessions);
            move |request| {
                calls.fetch_add(1, Ordering::SeqCst);
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
                barrier.wait();
                active.fetch_sub(1, Ordering::SeqCst);
                let mut sequence = sessions.lock().map_err(|_| "session lock".to_owned())?;
                *sequence += 1;
                Ok(WorkerEvidence {
                    task_id: request.contract.task_id,
                    worker_id: request.worker_id,
                    role: request.contract.role,
                    context_fingerprint: request.packet.fingerprint,
                    lease_epoch: 0,
                    session_id: format!("session-{sequence}"),
                    turns: 1,
                    summary: "repository evidence collected".to_owned(),
                })
            }
        })
        .expect("first execution");
        assert_eq!(first.workers.len(), 2);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let restored = coordinate_with_executor(repo.path(), &plan, &cancel, &events, |_| {
            Err("cached evidence should avoid worker execution".to_owned())
        })
        .expect("restored evidence");
        assert_eq!(restored, first);
    }

    #[test]
    fn deterministic_fast_preflight_uses_zero_model_turns_and_restores() {
        let repo = tempfile::tempdir().expect("repository");
        fs::create_dir_all(repo.path().join("src")).expect("src");
        fs::write(repo.path().join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n")
            .expect("source");
        let plan = production_orchestrator::plan_for_repository(
            repo.path(),
            &PromptDraft {
                text: "Fix src/lib.rs".to_owned(),
                ..PromptDraft::default()
            },
        )
        .expect("plan");
        assert_eq!(plan.planning.lane, ExecutionLane::FastMutation);
        let (events, _) = mpsc::channel();
        let control = TeamControlPlane::default();
        let first = run_deterministic_fast_preflight(repo.path(), &plan, &control, &events)
            .expect("deterministic preflight");
        assert_eq!(first.workers.len(), 2);
        assert!(first.workers.iter().all(|worker| worker.turns == 0));
        let restored = run_deterministic_fast_preflight(repo.path(), &plan, &control, &events)
            .expect("restored deterministic preflight");
        assert_eq!(restored, first);
    }

    #[test]
    fn repository_change_invalidates_cached_evidence() {
        let repo = tempfile::tempdir().expect("repository");
        let plan = production_orchestrator::plan(&PromptDraft {
            text: "Implement a repository-wide refactor with tests".to_owned(),
            ..PromptDraft::default()
        })
        .expect("plan");
        let cancel = Arc::new(AtomicBool::new(false));
        let (events, _) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let execute = |request: WorkerRequest, calls: &AtomicUsize| {
            let sequence = calls.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(WorkerEvidence {
                task_id: request.contract.task_id,
                worker_id: request.worker_id,
                role: request.contract.role,
                context_fingerprint: request.packet.fingerprint,
                lease_epoch: 0,
                session_id: format!("session-{sequence}"),
                turns: 1,
                summary: "fresh repository evidence".to_owned(),
            })
        };

        coordinate_with_executor(repo.path(), &plan, &cancel, &events, {
            let calls = Arc::clone(&calls);
            move |request| execute(request, &calls)
        })
        .expect("first execution");
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        fs::write(repo.path().join("changed.rs"), "pub fn changed() {}\n")
            .expect("repository change");
        coordinate_with_executor(repo.path(), &plan, &cancel, &events, {
            let calls = Arc::clone(&calls);
            move |request| execute(request, &calls)
        })
        .expect("execution after repository change");
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn worker_failure_is_recorded_and_fails_closed() {
        let repo = tempfile::tempdir().expect("repository");
        let plan = production_orchestrator::plan(&PromptDraft {
            text: "Implement a repository-wide refactor with tests".to_owned(),
            ..PromptDraft::default()
        })
        .expect("plan");
        let cancel = Arc::new(AtomicBool::new(false));
        let (events, _) = mpsc::channel();
        let error = coordinate_with_executor(repo.path(), &plan, &cancel, &events, |request| {
            if request.contract.task_id == "analyze" {
                return Err("planner failed".to_owned());
            }
            Ok(WorkerEvidence {
                task_id: request.contract.task_id,
                worker_id: request.worker_id,
                role: request.contract.role,
                context_fingerprint: request.packet.fingerprint,
                lease_epoch: 0,
                session_id: "session-risk".to_owned(),
                turns: 1,
                summary: "risk evidence".to_owned(),
            })
        })
        .expect_err("worker failure must fail the coordinated turn");
        assert!(error.contains("planner failed"));

        let team_state = find_state_file(repo.path(), "team.json");
        let content = fs::read_to_string(team_state).expect("team state");
        assert!(content.contains("\"failed\""));
    }

    #[test]
    fn cancellation_fails_before_worker_dispatch() {
        let repo = tempfile::tempdir().expect("repository");
        let plan = production_orchestrator::plan(&PromptDraft {
            text: "Implement a repository-wide refactor with tests".to_owned(),
            ..PromptDraft::default()
        })
        .expect("plan");
        let cancel = Arc::new(AtomicBool::new(true));
        let (events, _) = mpsc::channel();
        assert!(
            coordinate_with_executor(repo.path(), &plan, &cancel, &events, |_| {
                Err("worker must not start".to_owned())
            })
            .is_err()
        );
    }
}
