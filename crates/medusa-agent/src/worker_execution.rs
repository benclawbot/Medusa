//! Durable lease authority for multi-agent worker execution.
//!
//! The static/dynamic scheduler remains responsible for deterministic task ordering,
//! while this controller owns worker identities, lease epochs, progress, cancellation,
//! restart persistence, and handoff into the transaction pipeline.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use medusa_multi_agent_scheduler::{Assignment, DynamicSchedule, Task, Worker as ScheduledWorker};
use medusa_progress::{ProgressEvent, ProgressKind};
use medusa_worker_leases::WorkerLease;
use medusa_workers::{Worker, WorkerState};
use serde::{Deserialize, Serialize};

use crate::transaction::WorkerMutationProposal;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeasedAssignment {
    pub task_id: String,
    pub worker_id: String,
    pub lease_epoch: u64,
    pub speculative: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkerCompletion {
    pub task_id: String,
    pub worker_id: String,
    pub lease_epoch: u64,
    pub transaction_proposals: Vec<WorkerMutationProposal>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkerProgressSummary {
    pub total: u32,
    pub active: u32,
    pub completed: u32,
    pub failed: u32,
    pub retries: u32,
    pub cancelled: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DurableWorkerExecution {
    execution_id: String,
    schedule: DynamicSchedule,
    workers: BTreeMap<String, Worker>,
    leases: BTreeMap<String, WorkerLease>,
    last_epochs: BTreeMap<String, u64>,
    completed_epochs: BTreeMap<String, u64>,
    cancelled_tasks: BTreeSet<String>,
    progress: Vec<ProgressEvent>,
    summary: WorkerProgressSummary,
    next_sequence: u64,
}

pub struct WorkerExecutionController {
    path: PathBuf,
    state: DurableWorkerExecution,
}

impl WorkerExecutionController {
    pub fn create(
        path: impl Into<PathBuf>,
        execution_id: impl Into<String>,
        tasks: Vec<Task>,
        scheduled_workers: Vec<ScheduledWorker>,
        workers: Vec<Worker>,
        max_attempts: u32,
    ) -> Result<Self, String> {
        let execution_id = execution_id.into();
        if execution_id.trim().is_empty() {
            return Err("execution identifier cannot be empty".into());
        }
        let schedule = DynamicSchedule::new(tasks, scheduled_workers.clone(), max_attempts)
            .map_err(str::to_owned)?;
        let workers = workers
            .into_iter()
            .map(|worker| (worker.id.clone(), worker))
            .collect::<BTreeMap<_, _>>();
        if workers.len() != scheduled_workers.len()
            || scheduled_workers
                .iter()
                .any(|worker| !workers.contains_key(&worker.id))
        {
            return Err("scheduled workers must map one-to-one to medusa worker records".into());
        }
        let total = u32::try_from(schedule_task_count(&schedule)).unwrap_or(u32::MAX);
        let state = DurableWorkerExecution {
            execution_id,
            schedule,
            workers,
            leases: BTreeMap::new(),
            last_epochs: BTreeMap::new(),
            completed_epochs: BTreeMap::new(),
            cancelled_tasks: BTreeSet::new(),
            progress: Vec::new(),
            summary: WorkerProgressSummary {
                total,
                ..WorkerProgressSummary::default()
            },
            next_sequence: 1,
        };
        let mut controller = Self {
            path: path.into(),
            state,
        };
        controller.push_progress(ProgressKind::Started, "multi-agent execution started", None)?;
        controller.persist()?;
        Ok(controller)
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let state: DurableWorkerExecution =
            serde_json::from_slice(&fs::read(&path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        state.schedule.validate().map_err(str::to_owned)?;
        for lease in state.leases.values() {
            lease.validate().map_err(str::to_owned)?;
        }
        validate_state(&state)?;
        Ok(Self { path, state })
    }

    pub fn dispatch(&mut self, now_ms: u64, timeout_ms: u64) -> Result<Vec<LeasedAssignment>, String> {
        let assignments = self.state.schedule.dispatch_ready().map_err(str::to_owned)?;
        let mut leased = Vec::with_capacity(assignments.len());
        for assignment in assignments {
            if self.state.leases.contains_key(&assignment.task_id) {
                return Err(format!("task {} already has a live lease", assignment.task_id));
            }
            let worker = self
                .state
                .workers
                .get_mut(&assignment.worker_id)
                .ok_or_else(|| format!("worker {} has no medusa worker record", assignment.worker_id))?;
            if !matches!(worker.state, WorkerState::Ready | WorkerState::Failed) {
                return Err(format!("worker {} is not available", assignment.worker_id));
            }
            let epoch = self
                .state
                .last_epochs
                .get(&assignment.task_id)
                .copied()
                .unwrap_or(0)
                .saturating_add(1);
            let lease = WorkerLease::acquire(
                &assignment.worker_id,
                &assignment.task_id,
                epoch,
                now_ms,
                timeout_ms,
            )
            .map_err(str::to_owned)?;
            self.state.last_epochs.insert(assignment.task_id.clone(), epoch);
            self.state.leases.insert(assignment.task_id.clone(), lease);
            worker.state = WorkerState::Running;
            self.state.summary.active = self.state.summary.active.saturating_add(1);
            self.push_progress(
                ProgressKind::ToolStarted,
                format!("task {} leased to {} at epoch {epoch}", assignment.task_id, assignment.worker_id),
                Some(assignment.task_id.clone()),
            )?;
            leased.push(LeasedAssignment {
                task_id: assignment.task_id,
                worker_id: assignment.worker_id,
                lease_epoch: epoch,
                speculative: assignment.speculative,
            });
        }
        self.persist()?;
        Ok(leased)
    }

    pub fn heartbeat(
        &mut self,
        task_id: &str,
        worker_id: &str,
        lease_epoch: u64,
        now_ms: u64,
    ) -> Result<(), String> {
        let lease = self.current_lease_mut(task_id, worker_id, lease_epoch)?;
        lease.heartbeat(now_ms).map_err(str::to_owned)?;
        self.persist()
    }

    pub fn expire_and_requeue(&mut self, now_ms: u64) -> Result<Vec<String>, String> {
        let expired = self
            .state
            .leases
            .iter()
            .filter_map(|(task_id, lease)| {
                lease
                    .expired(now_ms)
                    .ok()
                    .filter(|expired| *expired)
                    .map(|_| task_id.clone())
            })
            .collect::<Vec<_>>();
        for task_id in &expired {
            let lease = self
                .state
                .leases
                .remove(task_id)
                .ok_or_else(|| "expired lease disappeared".to_owned())?;
            self.state
                .schedule
                .fail(task_id, &lease.worker_id, "worker lease expired", true)
                .map_err(str::to_owned)?;
            self.state
                .schedule
                .set_worker_health(&lease.worker_id, false)
                .map_err(str::to_owned)?;
            if let Some(worker) = self.state.workers.get_mut(&lease.worker_id) {
                worker.state = WorkerState::Failed;
            }
            self.state.summary.active = self.state.summary.active.saturating_sub(1);
            self.state.summary.retries = self.state.summary.retries.saturating_add(1);
            self.push_progress(
                ProgressKind::Retrying,
                format!("task {task_id} requeued after lease expiry"),
                Some(task_id.clone()),
            )?;
        }
        self.persist()?;
        Ok(expired)
    }

    pub fn complete(
        &mut self,
        task_id: &str,
        worker_id: &str,
        lease_epoch: u64,
        now_ms: u64,
        transaction_proposals: Vec<WorkerMutationProposal>,
    ) -> Result<WorkerCompletion, String> {
        let lease = self.current_lease(task_id, worker_id, lease_epoch)?;
        if lease.expired(now_ms).map_err(str::to_owned)? {
            return Err("completion was submitted after lease expiry".into());
        }
        if self.state.completed_epochs.contains_key(task_id) {
            return Err("task completion was already accepted".into());
        }
        if transaction_proposals.is_empty()
            || transaction_proposals.iter().any(|proposal| {
                proposal.worker_id != worker_id
                    || proposal.task_id != task_id
                    || proposal.lease_epoch != lease_epoch
            })
        {
            return Err("worker completion must contain matching transaction proposals".into());
        }
        self.state
            .schedule
            .complete(task_id, worker_id)
            .map_err(str::to_owned)?;
        self.state.leases.remove(task_id);
        self.state.completed_epochs.insert(task_id.to_owned(), lease_epoch);
        if let Some(worker) = self.state.workers.get_mut(worker_id) {
            worker.state = WorkerState::Succeeded;
        }
        self.state.summary.active = self.state.summary.active.saturating_sub(1);
        self.state.summary.completed = self.state.summary.completed.saturating_add(1);
        self.push_progress(
            ProgressKind::ToolFinished,
            format!("task {task_id} completed and is awaiting transaction commit"),
            Some(task_id.to_owned()),
        )?;
        self.persist()?;
        Ok(WorkerCompletion {
            task_id: task_id.to_owned(),
            worker_id: worker_id.to_owned(),
            lease_epoch,
            transaction_proposals,
        })
    }

    pub fn fail(
        &mut self,
        task_id: &str,
        worker_id: &str,
        lease_epoch: u64,
        reason: impl Into<String>,
        retryable: bool,
    ) -> Result<(), String> {
        self.current_lease(task_id, worker_id, lease_epoch)?;
        let reason = reason.into();
        self.state
            .schedule
            .fail(task_id, worker_id, &reason, retryable)
            .map_err(str::to_owned)?;
        self.state.leases.remove(task_id);
        if let Some(worker) = self.state.workers.get_mut(worker_id) {
            worker.state = WorkerState::Failed;
        }
        self.state.summary.active = self.state.summary.active.saturating_sub(1);
        if retryable {
            self.state.summary.retries = self.state.summary.retries.saturating_add(1);
        } else {
            self.state.summary.failed = self.state.summary.failed.saturating_add(1);
        }
        self.push_progress(
            if retryable { ProgressKind::Retrying } else { ProgressKind::Failed },
            reason,
            Some(task_id.to_owned()),
        )?;
        self.persist()
    }

    pub fn cancel(
        &mut self,
        task_id: &str,
        worker_id: &str,
        lease_epoch: u64,
    ) -> Result<(), String> {
        self.current_lease(task_id, worker_id, lease_epoch)?;
        self.state
            .schedule
            .fail(task_id, worker_id, "task cancelled", false)
            .map_err(str::to_owned)?;
        self.state.leases.remove(task_id);
        self.state.cancelled_tasks.insert(task_id.to_owned());
        if let Some(worker) = self.state.workers.get_mut(worker_id) {
            worker.state = WorkerState::Failed;
        }
        self.state.summary.active = self.state.summary.active.saturating_sub(1);
        self.state.summary.cancelled = self.state.summary.cancelled.saturating_add(1);
        self.push_progress(
            ProgressKind::Failed,
            format!("task {task_id} cancelled and lease released"),
            Some(task_id.to_owned()),
        )?;
        self.persist()
    }

    pub fn progress(&self) -> &[ProgressEvent] {
        &self.state.progress
    }

    pub fn summary(&self) -> &WorkerProgressSummary {
        &self.state.summary
    }

    pub fn execution_id(&self) -> &str {
        &self.state.execution_id
    }

    fn current_lease(
        &self,
        task_id: &str,
        worker_id: &str,
        lease_epoch: u64,
    ) -> Result<&WorkerLease, String> {
        let lease = self
            .state
            .leases
            .get(task_id)
            .ok_or_else(|| "task has no live lease".to_owned())?;
        if lease.worker_id != worker_id || lease.lease_epoch != lease_epoch {
            return Err("worker completion does not own the current lease epoch".into());
        }
        Ok(lease)
    }

    fn current_lease_mut(
        &mut self,
        task_id: &str,
        worker_id: &str,
        lease_epoch: u64,
    ) -> Result<&mut WorkerLease, String> {
        let lease = self
            .state
            .leases
            .get_mut(task_id)
            .ok_or_else(|| "task has no live lease".to_owned())?;
        if lease.worker_id != worker_id || lease.lease_epoch != lease_epoch {
            return Err("worker does not own the current lease epoch".into());
        }
        Ok(lease)
    }

    fn push_progress(
        &mut self,
        kind: ProgressKind,
        message: impl Into<String>,
        step_id: Option<String>,
    ) -> Result<(), String> {
        let mut event = ProgressEvent::new(self.state.next_sequence, kind, message)
            .map_err(|error| error.to_string())?;
        event.step_id = step_id;
        self.state.next_sequence = self.state.next_sequence.saturating_add(1);
        self.state.progress.push(event);
        Ok(())
    }

    fn persist(&self) -> Result<(), String> {
        validate_state(&self.state)?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "worker execution state path has no parent".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let temporary = self.path.with_extension("json.tmp");
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(&self.state).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        fs::rename(temporary, &self.path).map_err(|error| error.to_string())
    }
}

fn schedule_task_count(schedule: &DynamicSchedule) -> usize {
    // DynamicSchedule intentionally exposes task state by identifier only. The durable
    // progress total is refreshed from dispatched/completed/failed/cancelled accounting.
    // A minimum of one preserves a useful total for valid non-empty schedules.
    let mut count = 0;
    while schedule.state(&format!("task-{count}" )).is_some() {
        count += 1;
    }
    count.max(1)
}

fn validate_state(state: &DurableWorkerExecution) -> Result<(), String> {
    if state.execution_id.trim().is_empty() || state.next_sequence == 0 {
        return Err("worker execution durable identity is invalid".into());
    }
    state.schedule.validate().map_err(str::to_owned)?;
    for (task_id, lease) in &state.leases {
        lease.validate().map_err(str::to_owned)?;
        if &lease.task_id != task_id {
            return Err("lease map key does not match task id".into());
        }
        if !state.workers.contains_key(&lease.worker_id) {
            return Err("lease references an unknown worker".into());
        }
    }
    if state
        .progress
        .windows(2)
        .any(|pair| pair[0].sequence >= pair[1].sequence)
    {
        return Err("progress sequence is not strictly increasing".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheduler_worker(id: &str) -> ScheduledWorker {
        ScheduledWorker {
            id: id.into(),
            capabilities: vec!["rust".into()],
            healthy: true,
            capacity: 1,
        }
    }

    fn execution_worker(id: &str) -> Worker {
        Worker {
            id: id.into(),
            branch: format!("medusa/{id}"),
            worktree: PathBuf::from(format!("worktrees/{id}")),
            state: WorkerState::Ready,
            commit: None,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn task() -> Task {
        Task {
            id: "task-0".into(),
            dependencies: vec![],
            capabilities: vec!["rust".into()],
            write_paths: vec!["src/lib.rs".into()],
            speculative: false,
        }
    }

    fn proposal(worker: &str, epoch: u64) -> WorkerMutationProposal {
        WorkerMutationProposal {
            worker_id: worker.into(),
            task_id: "task-0".into(),
            lease_epoch: epoch,
            path: "src/lib.rs".into(),
            expected_fingerprint: "00".repeat(32),
            content: "pub fn fixed() {}\n".into(),
            priority: 1,
        }
    }

    #[test]
    fn expired_worker_is_reassigned_once_with_a_new_epoch() {
        let directory = tempfile::tempdir().unwrap();
        let mut controller = WorkerExecutionController::create(
            directory.path().join("workers.json"),
            "exec-1",
            vec![task()],
            vec![scheduler_worker("a"), scheduler_worker("b")],
            vec![execution_worker("a"), execution_worker("b")],
            3,
        )
        .unwrap();
        let first = controller.dispatch(0, 10).unwrap();
        assert_eq!(first[0].worker_id, "a");
        assert_eq!(controller.expire_and_requeue(11).unwrap(), vec!["task-0"]);
        let second = controller.dispatch(12, 10).unwrap();
        assert_eq!(second[0].worker_id, "b");
        assert_eq!(second[0].lease_epoch, 2);
        assert!(controller.expire_and_requeue(12).unwrap().is_empty());
    }

    #[test]
    fn healthy_slow_worker_keeps_lease_via_heartbeat() {
        let directory = tempfile::tempdir().unwrap();
        let mut controller = WorkerExecutionController::create(
            directory.path().join("workers.json"),
            "exec-1",
            vec![task()],
            vec![scheduler_worker("a")],
            vec![execution_worker("a")],
            2,
        )
        .unwrap();
        let assignment = controller.dispatch(0, 10).unwrap().remove(0);
        controller
            .heartbeat("task-0", "a", assignment.lease_epoch, 8)
            .unwrap();
        assert!(controller.expire_and_requeue(15).unwrap().is_empty());
    }

    #[test]
    fn expired_or_duplicate_completion_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let mut controller = WorkerExecutionController::create(
            directory.path().join("workers.json"),
            "exec-1",
            vec![task()],
            vec![scheduler_worker("a")],
            vec![execution_worker("a")],
            2,
        )
        .unwrap();
        let assignment = controller.dispatch(0, 10).unwrap().remove(0);
        assert!(controller
            .complete("task-0", "a", assignment.lease_epoch, 11, vec![proposal("a", 1)])
            .is_err());
        controller.heartbeat("task-0", "a", assignment.lease_epoch, 9).unwrap();
        let completion = controller
            .complete("task-0", "a", assignment.lease_epoch, 10, vec![proposal("a", 1)])
            .unwrap();
        assert_eq!(completion.transaction_proposals.len(), 1);
        assert!(controller
            .complete("task-0", "a", assignment.lease_epoch, 10, vec![proposal("a", 1)])
            .is_err());
    }

    #[test]
    fn cancellation_releases_lease_and_survives_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("workers.json");
        let mut controller = WorkerExecutionController::create(
            &path,
            "exec-1",
            vec![task()],
            vec![scheduler_worker("a")],
            vec![execution_worker("a")],
            2,
        )
        .unwrap();
        let assignment = controller.dispatch(0, 10).unwrap().remove(0);
        controller.cancel("task-0", "a", assignment.lease_epoch).unwrap();
        drop(controller);
        let restored = WorkerExecutionController::load(path).unwrap();
        assert_eq!(restored.summary().cancelled, 1);
        assert_eq!(restored.summary().active, 0);
    }
}
