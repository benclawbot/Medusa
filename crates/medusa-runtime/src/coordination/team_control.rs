//! Shared, typed control plane for production multi-agent observability and steering.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, MutexGuard},
};

use serde::{Deserialize, Serialize};

const MAX_INSTRUCTION_BYTES: usize = 4 * 1024;

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
    /// Compatibility projection for callers that still display the retired process-local queue.
    /// Steering is persisted as durable session actions, so this value is always zero.
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
        let session_id = session_id.map(str::to_owned);
        let snapshot = self.update(worker_id, |worker| {
            if let Some(session_id) = &session_id {
                worker.session_id = Some(session_id.clone());
            }
            if worker.lifecycle != TeamWorkerLifecycle::CancellationRequested {
                worker.lifecycle = TeamWorkerLifecycle::Running;
                worker.last_update = message.into();
            }
        })?;
        self.bind_published_session(worker_id, session_id.as_deref(), &snapshot)?;
        Ok(snapshot)
    }

    pub fn progress(
        &self,
        worker_id: &str,
        session_id: Option<&str>,
        turn: u32,
        message: impl Into<String>,
    ) -> Result<TeamSnapshot, String> {
        let session_id = session_id.map(str::to_owned);
        let snapshot = self.update(worker_id, |worker| {
            if let Some(session_id) = &session_id {
                worker.session_id = Some(session_id.clone());
            }
            worker.turn = turn;
            if worker.lifecycle != TeamWorkerLifecycle::CancellationRequested {
                worker.lifecycle = TeamWorkerLifecycle::Running;
                worker.last_update = message.into();
            }
        })?;
        self.bind_published_session(worker_id, session_id.as_deref(), &snapshot)?;
        Ok(snapshot)
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

    /// Persists steering into the canonical worker session before reporting it accepted. There is
    /// intentionally no independent team queue: the next worker request obtains the instruction
    /// from its durable session context and #890 records the exact action id that became visible.
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

        let (execution_id, session_id, idempotency_key) = {
            let mut state = self.lock();
            let execution_id = state
                .execution_id
                .clone()
                .ok_or_else(|| "no coordinated team is active".to_owned())?;
            let next_sequence = state.sequence.saturating_add(1);
            let worker = state
                .workers
                .get_mut(worker_id)
                .ok_or_else(|| format!("unknown team worker `{worker_id}`"))?;
            if worker.lifecycle.is_terminal() {
                return Err(format!("worker `{worker_id}` is already terminal"));
            }
            let session_id = worker.session_id.clone().ok_or_else(|| {
                format!(
                    "worker `{worker_id}` has no durable session yet; steering was not accepted"
                )
            })?;
            worker.last_update = "persisting steering instruction".to_owned();
            state.sequence = next_sequence;
            (
                execution_id.clone(),
                session_id,
                format!("team-control:{execution_id}:{worker_id}:{next_sequence}"),
            )
        };

        let action_id = medusa_agent::team::admit_control_instruction(
            &execution_id,
            &session_id,
            worker_id,
            instruction,
            &idempotency_key,
        )?;

        let mut state = self.lock();
        if state.execution_id.as_deref() == Some(execution_id.as_str())
            && let Some(worker) = state.workers.get_mut(worker_id)
        {
            worker.last_update = format!("steering action {action_id} accepted durably");
        }
        Ok(snapshot(&state))
    }

    /// Temporary compatibility hook for worker loops that still poll the retired process queue.
    /// Durable session actions are authoritative, so there is never a process-local instruction.
    pub fn take_instruction(&self, _worker_id: &str) -> Result<Option<String>, String> {
        Ok(None)
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
                    "{} [{} / {}] {:?}; turn {}; session {}; {}",
                    worker.worker_id,
                    worker.role,
                    worker.task_id,
                    worker.lifecycle,
                    worker.turn,
                    worker.session_id.as_deref().unwrap_or("pending"),
                    worker.last_update,
                )
            })
            .collect()
    }

    fn bind_published_session(
        &self,
        worker_id: &str,
        session_id: Option<&str>,
        snapshot: &TeamSnapshot,
    ) -> Result<(), String> {
        let Some(session_id) = session_id else {
            return Ok(());
        };
        let execution_id = snapshot
            .execution_id
            .as_deref()
            .ok_or_else(|| "no coordinated team is active".to_owned())?;
        match medusa_agent::team::bind_control_session(execution_id, worker_id, session_id) {
            Ok(()) => Ok(()),
            Err(error) if error.contains("has no durable repository binding") => Ok(()),
            Err(error) => Err(error),
        }
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
                queued_instructions: 0,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use medusa_agent::{AgentEngine, TeamRole, TeamRuntime};
    use medusa_config::Config;
    use medusa_core::MedusaResult;
    use medusa_protocol::EventPayload;
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse, Usage};

    use super::*;

    struct NoopProvider;

    impl ModelProvider for NoopProvider {
        fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
            Ok(ModelResponse {
                response_id: Some("noop".to_owned()),
                stop_reason: Some("stop".to_owned()),
                blocks: Vec::new(),
                usage: Usage::default(),
            })
        }
    }

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
    fn steering_is_persisted_and_not_process_queued() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let engine = AgentEngine::new(NoopProvider, Config::default());
        let session = engine
            .create_session(directory.path(), "analyze".to_owned())
            .expect("session");
        let team = TeamRuntime::create(
            directory.path().join(".medusa/executions/test/team.json"),
            "execution-1",
            vec![
                ("lead".to_owned(), TeamRole::Lead),
                ("worker-a".to_owned(), TeamRole::Planner),
            ],
        )
        .expect("team runtime");
        let worker_context = team.member_context("worker-a").expect("worker context");
        let control = control();
        control
            .start("worker-a", Some(session.id.as_str()), "running")
            .unwrap();
        control
            .steer("worker-a", "inspect the failing test")
            .unwrap();
        assert!(
            worker_context
                .prompt_context()
                .expect("prompt context")
                .contains("inspect the failing test")
        );

        let restored = engine
            .load_session(directory.path(), session.id.as_str())
            .expect("restore session");
        assert!(restored.events.iter().any(|event| matches!(
            &event.payload,
            EventPayload::SessionActionAccepted { action }
                if action.source == "team:lead:worker-a"
                    && action.payload["text"] == serde_json::json!("inspect the failing test")
        )));
    }

    #[test]
    fn steering_rejects_terminal_or_unpublished_workers() {
        let pending = control();
        assert!(pending.steer("worker-a", "not admitted yet").is_err());

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
        let observational = control();
        observational
            .start("worker-a", Some("session-a"), "running")
            .unwrap();
        assert_eq!(
            observational.snapshot().workers[0].session_id.as_deref(),
            Some("session-a")
        );

        let control = control();
        control.stop_worker("worker-a").unwrap();
        control
            .progress("worker-a", None, 2, "late progress")
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
