//! Shared, typed control plane for production multi-agent observability and steering.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex, MutexGuard},
};

use serde::{Deserialize, Serialize};

const MAX_INSTRUCTION_BYTES: usize = 4 * 1024;
const MAX_QUEUED_INSTRUCTIONS: usize = 8;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamWorkerLifecycle {
    Pending,
    Running,
    Retrying,
    CancellationRequested,
    Completed,
    Failed,
    Integrated,
}

impl TeamWorkerLifecycle {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Integrated)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TeamWorkerRegistration {
    pub worker_id: String,
    pub role: String,
    pub task_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamWorkerSnapshot {
    pub worker_id: String,
    pub role: String,
    pub task_id: String,
    pub lifecycle: TeamWorkerLifecycle,
    pub session_id: Option<String>,
    pub turn: u32,
    pub last_update: String,
    pub queued_instructions: usize,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamSnapshot {
    pub execution_id: Option<String>,
    pub active: bool,
    pub shutdown_requested: bool,
    pub sequence: u64,
    pub workers: Vec<TeamWorkerSnapshot>,
}

#[derive(Clone, Debug)]
struct WorkerState {
    role: String,
    task_id: String,
    lifecycle: TeamWorkerLifecycle,
    session_id: Option<String>,
    turn: u32,
    last_update: String,
    instructions: VecDeque<String>,
}

#[derive(Default)]
struct ControlState {
    execution_id: Option<String>,
    active: bool,
    shutdown_requested: bool,
    sequence: u64,
    workers: BTreeMap<String, WorkerState>,
    cancelled_workers: BTreeSet<String>,
}

#[derive(Clone, Default)]
pub struct TeamControlPlane {
    inner: Arc<Mutex<ControlState>>,
}

impl TeamControlPlane {
    pub fn begin(
        &self,
        execution_id: impl Into<String>,
        registrations: impl IntoIterator<Item = TeamWorkerRegistration>,
    ) -> TeamSnapshot {
        let execution_id = execution_id.into();
        let mut state = self.lock();
        if state.execution_id.as_deref() != Some(execution_id.as_str()) {
            *state = ControlState {
                execution_id: Some(execution_id),
                active: true,
                ..ControlState::default()
            };
        } else {
            state.active = true;
        }
        for registration in registrations {
            state
                .workers
                .entry(registration.worker_id)
                .or_insert(WorkerState {
                    role: registration.role,
                    task_id: registration.task_id,
                    lifecycle: TeamWorkerLifecycle::Pending,
                    session_id: None,
                    turn: 0,
                    last_update: "waiting for dispatch".to_owned(),
                    instructions: VecDeque::new(),
                });
        }
        bump(&mut state);
        snapshot(&state)
    }

    pub fn clear(&self) -> TeamSnapshot {
        let mut state = self.lock();
        *state = ControlState::default();
        snapshot(&state)
    }

    #[must_use]
    pub fn snapshot(&self) -> TeamSnapshot {
        snapshot(&self.lock())
    }

    pub fn start(
        &self,
        worker_id: &str,
        session_id: Option<&str>,
        message: impl Into<String>,
    ) -> Result<TeamSnapshot, String> {
        self.update(worker_id, |worker| {
            if let Some(session_id) = session_id {
                worker.session_id = Some(session_id.to_owned());
            }
            if worker.lifecycle != TeamWorkerLifecycle::CancellationRequested {
                worker.lifecycle = TeamWorkerLifecycle::Running;
                worker.last_update = message.into();
            }
        })
    }

    pub fn progress(
        &self,
        worker_id: &str,
        session_id: Option<&str>,
        turn: u32,
        message: impl Into<String>,
    ) -> Result<TeamSnapshot, String> {
        self.update(worker_id, |worker| {
            if let Some(session_id) = session_id {
                worker.session_id = Some(session_id.to_owned());
            }
            worker.turn = turn;
            if worker.lifecycle != TeamWorkerLifecycle::CancellationRequested {
                worker.lifecycle = TeamWorkerLifecycle::Running;
                worker.last_update = message.into();
            }
        })
    }

    pub fn retrying(
        &self,
        worker_id: &str,
        message: impl Into<String>,
    ) -> Result<TeamSnapshot, String> {
        self.update(worker_id, |worker| {
            if worker.lifecycle != TeamWorkerLifecycle::CancellationRequested {
                worker.lifecycle = TeamWorkerLifecycle::Retrying;
                worker.last_update = message.into();
            }
        })
    }

    pub fn complete(
        &self,
        worker_id: &str,
        message: impl Into<String>,
    ) -> Result<TeamSnapshot, String> {
        self.update(worker_id, |worker| {
            worker.lifecycle = TeamWorkerLifecycle::Completed;
            worker.last_update = message.into();
        })
    }

    pub fn integrated(
        &self,
        worker_id: &str,
        message: impl Into<String>,
    ) -> Result<TeamSnapshot, String> {
        self.update(worker_id, |worker| {
            worker.lifecycle = TeamWorkerLifecycle::Integrated;
            worker.last_update = message.into();
        })
    }

    pub fn fail(
        &self,
        worker_id: &str,
        message: impl Into<String>,
    ) -> Result<TeamSnapshot, String> {
        self.update(worker_id, |worker| {
            worker.lifecycle = TeamWorkerLifecycle::Failed;
            worker.last_update = message.into();
        })
    }

    pub fn finish(&self) -> TeamSnapshot {
        let mut state = self.lock();
        state.active = false;
        bump(&mut state);
        snapshot(&state)
    }

    pub fn steer(&self, worker_id: &str, instruction: &str) -> Result<TeamSnapshot, String> {
        let instruction = instruction.trim();
        if instruction.is_empty() {
            return Err("steering instruction cannot be empty".to_owned());
        }
        if instruction.len() > MAX_INSTRUCTION_BYTES {
            return Err(format!(
                "steering instruction exceeds the {MAX_INSTRUCTION_BYTES}-byte limit"
            ));
        }
        let mut state = self.lock();
        let worker = state
            .workers
            .get_mut(worker_id)
            .ok_or_else(|| format!("unknown team worker `{worker_id}`"))?;
        if worker.lifecycle.is_terminal() {
            return Err(format!("worker `{worker_id}` is already terminal"));
        }
        if worker.instructions.len() >= MAX_QUEUED_INSTRUCTIONS {
            return Err(format!(
                "worker `{worker_id}` already has {MAX_QUEUED_INSTRUCTIONS} queued instructions"
            ));
        }
        worker.instructions.push_back(instruction.to_owned());
        worker.last_update = "steering instruction queued".to_owned();
        bump(&mut state);
        Ok(snapshot(&state))
    }

    pub fn stop_worker(&self, worker_id: &str) -> Result<TeamSnapshot, String> {
        let mut state = self.lock();
        let worker = state
            .workers
            .get_mut(worker_id)
            .ok_or_else(|| format!("unknown team worker `{worker_id}`"))?;
        if worker.lifecycle.is_terminal() {
            return Err(format!("worker `{worker_id}` is already terminal"));
        }
        worker.lifecycle = TeamWorkerLifecycle::CancellationRequested;
        worker.last_update = "worker cancellation requested".to_owned();
        state.cancelled_workers.insert(worker_id.to_owned());
        bump(&mut state);
        Ok(snapshot(&state))
    }

    pub fn stop_team(&self) -> Result<TeamSnapshot, String> {
        let mut state = self.lock();
        if state.execution_id.is_none() {
            return Err("no coordinated team is active".to_owned());
        }
        state.shutdown_requested = true;
        let mut cancelled = Vec::new();
        for (worker_id, worker) in &mut state.workers {
            if !worker.lifecycle.is_terminal() {
                worker.lifecycle = TeamWorkerLifecycle::CancellationRequested;
                worker.last_update = "team shutdown requested".to_owned();
                cancelled.push(worker_id.clone());
            }
        }
        state.cancelled_workers.extend(cancelled);
        bump(&mut state);
        Ok(snapshot(&state))
    }

    #[must_use]
    pub fn is_cancelled(&self, worker_id: &str) -> bool {
        let state = self.lock();
        state.shutdown_requested || state.cancelled_workers.contains(worker_id)
    }

    pub fn take_instruction(&self, worker_id: &str) -> Result<Option<String>, String> {
        let mut state = self.lock();
        let worker = state
            .workers
            .get_mut(worker_id)
            .ok_or_else(|| format!("unknown team worker `{worker_id}`"))?;
        let instruction = worker.instructions.pop_front();
        if instruction.is_some() {
            worker.last_update = "steering instruction accepted".to_owned();
            bump(&mut state);
        }
        Ok(instruction)
    }

    #[must_use]
    pub fn render_lines(&self) -> Vec<String> {
        let snapshot = self.snapshot();
        if snapshot.workers.is_empty() {
            return vec!["No coordinated workers are active or retained.".to_owned()];
        }
        snapshot
            .workers
            .into_iter()
            .map(|worker| {
                format!(
                    "{} [{} / {}] {:?}; turn {}; session {}; {}; queued steering {}",
                    worker.worker_id,
                    worker.role,
                    worker.task_id,
                    worker.lifecycle,
                    worker.turn,
                    worker.session_id.as_deref().unwrap_or("pending"),
                    worker.last_update,
                    worker.queued_instructions,
                )
            })
            .collect()
    }

    fn update(
        &self,
        worker_id: &str,
        action: impl FnOnce(&mut WorkerState),
    ) -> Result<TeamSnapshot, String> {
        let mut state = self.lock();
        let worker = state
            .workers
            .get_mut(worker_id)
            .ok_or_else(|| format!("unknown team worker `{worker_id}`"))?;
        action(worker);
        bump(&mut state);
        Ok(snapshot(&state))
    }

    fn lock(&self) -> MutexGuard<'_, ControlState> {
        match self.inner.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

fn bump(state: &mut ControlState) {
    state.sequence = state.sequence.saturating_add(1);
}

fn snapshot(state: &ControlState) -> TeamSnapshot {
    TeamSnapshot {
        execution_id: state.execution_id.clone(),
        active: state.active,
        shutdown_requested: state.shutdown_requested,
        sequence: state.sequence,
        workers: state
            .workers
            .iter()
            .map(|(worker_id, worker)| TeamWorkerSnapshot {
                worker_id: worker_id.clone(),
                role: worker.role.clone(),
                task_id: worker.task_id.clone(),
                lifecycle: worker.lifecycle,
                session_id: worker.session_id.clone(),
                turn: worker.turn,
                last_update: worker.last_update.clone(),
                queued_instructions: worker.instructions.len(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control() -> TeamControlPlane {
        let control = TeamControlPlane::default();
        control.begin(
            "execution-1",
            [TeamWorkerRegistration {
                worker_id: "worker-a".to_owned(),
                role: "planner".to_owned(),
                task_id: "analyze".to_owned(),
            }],
        );
        control
    }

    #[test]
    fn steering_is_bounded_and_consumed_once() {
        let control = control();
        control
            .start("worker-a", Some("session-a"), "running")
            .unwrap();
        control
            .steer("worker-a", "inspect the failing test")
            .unwrap();
        assert_eq!(
            control.take_instruction("worker-a").unwrap().as_deref(),
            Some("inspect the failing test")
        );
        assert_eq!(control.take_instruction("worker-a").unwrap(), None);
    }

    #[test]
    fn steering_rejects_terminal_workers_and_full_queues() {
        let active = control();
        for index in 0..MAX_QUEUED_INSTRUCTIONS {
            active
                .steer("worker-a", &format!("instruction {index}"))
                .unwrap();
        }
        assert!(active.steer("worker-a", "one too many").is_err());

        let terminal = control();
        terminal.complete("worker-a", "done").unwrap();
        assert!(terminal.steer("worker-a", "too late").is_err());
    }

    #[test]
    fn worker_and_team_cancellation_are_distinct() {
        let control = control();
        control.stop_worker("worker-a").unwrap();
        assert!(control.is_cancelled("worker-a"));
        assert!(!control.snapshot().shutdown_requested);

        let other = TeamControlPlane::default();
        other.begin(
            "execution-2",
            [TeamWorkerRegistration {
                worker_id: "worker-b".to_owned(),
                role: "implementer".to_owned(),
                task_id: "implement".to_owned(),
            }],
        );
        other.stop_team().unwrap();
        assert!(other.is_cancelled("worker-b"));
        assert!(other.snapshot().shutdown_requested);
    }

    #[test]
    fn progress_cannot_overwrite_a_cancellation_request() {
        let control = control();
        control
            .start("worker-a", Some("session-a"), "running")
            .unwrap();
        control.stop_worker("worker-a").unwrap();
        control
            .progress("worker-a", Some("session-a"), 2, "late progress")
            .unwrap();
        control.retrying("worker-a", "late retry").unwrap();

        let worker = &control.snapshot().workers[0];
        assert_eq!(worker.lifecycle, TeamWorkerLifecycle::CancellationRequested);
        assert_eq!(worker.last_update, "worker cancellation requested");
        assert_eq!(worker.turn, 2);
    }

    #[test]
    fn a_new_execution_replaces_stale_workers() {
        let control = control();
        control.begin(
            "execution-2",
            [TeamWorkerRegistration {
                worker_id: "worker-b".to_owned(),
                role: "reviewer".to_owned(),
                task_id: "review".to_owned(),
            }],
        );
        let snapshot = control.snapshot();
        assert_eq!(snapshot.execution_id.as_deref(), Some("execution-2"));
        assert_eq!(snapshot.workers.len(), 1);
        assert_eq!(snapshot.workers[0].worker_id, "worker-b");
    }
}
