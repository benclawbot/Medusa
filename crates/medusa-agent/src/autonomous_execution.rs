//! Durable autonomous execution state connected to the user-visible agent plan.

use std::{fs, path::PathBuf};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_multi_agent_scheduler::{Assignment, DynamicSchedule, Task, TaskState, Worker};
use serde::{Deserialize, Serialize};

use crate::session::{AgentPlanStepStatus, AgentSession};

const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// Durable execution controller for one agent session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AutonomousExecution {
    pub session_id: String,
    pub scheduler: DynamicSchedule,
}

impl AutonomousExecution {
    /// Build and persist an execution graph from the current visible plan.
    ///
    /// Plan steps are ordered dependencies by default. A later planner can provide
    /// a richer graph without changing the durable execution contract.
    pub fn start(session: &mut AgentSession, workers: Vec<Worker>) -> MedusaResult<Self> {
        Self::start_with_attempts(session, workers, DEFAULT_MAX_ATTEMPTS)
    }

    pub fn start_with_attempts(
        session: &mut AgentSession,
        workers: Vec<Worker>,
        max_attempts: u32,
    ) -> MedusaResult<Self> {
        if session.plan.is_empty() {
            return Err(validation_error(
                "autonomous execution requires a non-empty visible plan",
            ));
        }
        let tasks = session
            .plan
            .iter()
            .enumerate()
            .map(|(index, step)| Task {
                id: task_id(index),
                dependencies: index
                    .checked_sub(1)
                    .map(|previous| vec![task_id(previous)])
                    .unwrap_or_default(),
                capabilities: vec!["coding".to_owned()],
                write_paths: Vec::new(),
                speculative: false,
            })
            .collect::<Vec<_>>();
        let scheduler = DynamicSchedule::new(tasks, workers, max_attempts)
            .map_err(|message| validation_error(message))?;
        for step in &mut session.plan {
            if step.status != AgentPlanStepStatus::Completed {
                step.status = AgentPlanStepStatus::Pending;
            }
        }
        let execution = Self {
            session_id: session.id.to_string(),
            scheduler,
        };
        execution.persist(session)?;
        Ok(execution)
    }

    /// Load a run after process restart and reject cross-session state reuse.
    pub fn load(session: &AgentSession) -> MedusaResult<Self> {
        let path = execution_path(session);
        let bytes = fs::read(&path).map_err(|error| io_error("read autonomous execution", error))?;
        let execution: Self = serde_json::from_slice(&bytes).map_err(json_error)?;
        if execution.session_id != session.id.to_string() {
            return Err(validation_error(
                "autonomous execution belongs to a different session",
            ));
        }
        execution
            .scheduler
            .validate()
            .map_err(validation_error)?;
        Ok(execution)
    }

    /// Dispatch all currently ready tasks and synchronize them into the visible plan.
    pub fn dispatch_ready(&mut self, session: &mut AgentSession) -> MedusaResult<Vec<Assignment>> {
        self.ensure_session(session)?;
        let assignments = self
            .scheduler
            .dispatch_ready()
            .map_err(validation_error)?;
        self.sync_plan(session)?;
        self.persist(session)?;
        Ok(assignments)
    }

    pub fn complete(
        &mut self,
        session: &mut AgentSession,
        task_id: &str,
        worker_id: &str,
    ) -> MedusaResult<()> {
        self.ensure_session(session)?;
        self.scheduler
            .complete(task_id, worker_id)
            .map_err(validation_error)?;
        self.sync_plan(session)?;
        self.persist(session)
    }

    pub fn fail(
        &mut self,
        session: &mut AgentSession,
        task_id: &str,
        worker_id: &str,
        reason: impl Into<String>,
        retryable: bool,
    ) -> MedusaResult<()> {
        self.ensure_session(session)?;
        self.scheduler
            .fail(task_id, worker_id, reason, retryable)
            .map_err(validation_error)?;
        self.sync_plan(session)?;
        self.persist(session)
    }

    pub fn set_worker_health(
        &mut self,
        session: &mut AgentSession,
        worker_id: &str,
        healthy: bool,
    ) -> MedusaResult<()> {
        self.ensure_session(session)?;
        self.scheduler
            .set_worker_health(worker_id, healthy)
            .map_err(validation_error)?;
        self.sync_plan(session)?;
        self.persist(session)
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.scheduler.is_complete()
    }

    #[must_use]
    pub fn blocked_tasks(&self) -> Vec<String> {
        self.scheduler.blocked_tasks()
    }

    fn ensure_session(&self, session: &AgentSession) -> MedusaResult<()> {
        if self.session_id == session.id.to_string() {
            Ok(())
        } else {
            Err(validation_error(
                "autonomous execution belongs to a different session",
            ))
        }
    }

    fn sync_plan(&self, session: &mut AgentSession) -> MedusaResult<()> {
        for (index, step) in session.plan.iter_mut().enumerate() {
            let state = self
                .scheduler
                .state(&task_id(index))
                .ok_or_else(|| validation_error("execution task is missing from the scheduler"))?;
            step.status = match state {
                TaskState::Pending { .. } => AgentPlanStepStatus::Pending,
                TaskState::Running { .. } => AgentPlanStepStatus::InProgress,
                TaskState::Succeeded => AgentPlanStepStatus::Completed,
                TaskState::Failed { .. } => AgentPlanStepStatus::Failed,
            };
        }
        Ok(())
    }

    fn persist(&self, session: &AgentSession) -> MedusaResult<()> {
        self.scheduler.validate().map_err(validation_error)?;
        let path = execution_path(session);
        let parent = path.parent().ok_or_else(|| {
            validation_error("autonomous execution path has no parent directory")
        })?;
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create autonomous execution directory", error))?;
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self).map_err(json_error)?;
        fs::write(&temporary, bytes)
            .map_err(|error| io_error("write autonomous execution", error))?;
        fs::rename(&temporary, &path)
            .map_err(|error| io_error("commit autonomous execution", error))?;
        Ok(())
    }
}

fn task_id(index: usize) -> String {
    format!("plan-{index:04}")
}

fn execution_path(session: &AgentSession) -> PathBuf {
    session
        .repo
        .join(".medusa")
        .join("executions")
        .join(format!("{}.json", session.id))
}

fn validation_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn io_error(operation: &str, error: std::io::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Execution,
        format!("failed to {operation}: {error}"),
    )
}

fn json_error(error: serde_json::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        format!("autonomous execution serialization failed: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use medusa_core::SessionId;
    use time::OffsetDateTime;

    use super::*;
    use crate::session::AgentPlanStep;

    fn worker(id: &str) -> Worker {
        Worker {
            id: id.to_owned(),
            capabilities: vec!["coding".to_owned()],
            healthy: true,
            capacity: 1,
        }
    }

    fn session(repo: &std::path::Path) -> AgentSession {
        AgentSession {
            id: SessionId::new(),
            objective: "ship the change".to_owned(),
            repo: repo.to_path_buf(),
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
            turn: 0,
            messages: Vec::new(),
            events: Vec::new(),
            plan: vec![
                AgentPlanStep {
                    title: "Inspect".to_owned(),
                    status: AgentPlanStepStatus::Pending,
                },
                AgentPlanStep {
                    title: "Implement".to_owned(),
                    status: AgentPlanStepStatus::Pending,
                },
            ],
            questions: Vec::new(),
            evidence: Vec::new(),
            completed: false,
            pending_tool_approval: None,
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
            world_model: None,
            escalations: Vec::new(),
        }
    }

    #[test]
    fn execution_is_durable_and_releases_the_next_plan_step() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = session(directory.path());
        let mut execution = AutonomousExecution::start(&mut session, vec![worker("one")]).unwrap();

        let first = execution.dispatch_ready(&mut session).unwrap();
        assert_eq!(first[0].task_id, "plan-0000");
        assert_eq!(session.plan[0].status, AgentPlanStepStatus::InProgress);
        execution.complete(&mut session, "plan-0000", "one").unwrap();

        let mut restored = AutonomousExecution::load(&session).unwrap();
        let second = restored.dispatch_ready(&mut session).unwrap();
        assert_eq!(second[0].task_id, "plan-0001");
        assert_eq!(session.plan[1].status, AgentPlanStepStatus::InProgress);
    }

    #[test]
    fn unhealthy_worker_requeues_running_work() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = session(directory.path());
        let mut execution = AutonomousExecution::start(
            &mut session,
            vec![worker("one"), worker("two")],
        )
        .unwrap();

        assert_eq!(execution.dispatch_ready(&mut session).unwrap()[0].worker_id, "one");
        execution.set_worker_health(&mut session, "one", false).unwrap();
        assert_eq!(execution.dispatch_ready(&mut session).unwrap()[0].worker_id, "two");
    }
}
