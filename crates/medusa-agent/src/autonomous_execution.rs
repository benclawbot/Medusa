//! Durable autonomous execution state connected to the user-visible agent plan.

use std::{collections::BTreeMap, fs, path::PathBuf};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_multi_agent_scheduler::{Assignment, DynamicSchedule, Task, TaskState, Worker};
use serde::{Deserialize, Serialize};

use crate::session::{AgentPlanStepStatus, AgentSession};

const DEFAULT_MAX_ATTEMPTS: u32 = 3;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRole {
    Planner,
    Researcher,
    Coder,
    Reviewer,
    Tester,
    Documentation,
    Security,
}

impl WorkerRole {
    #[must_use]
    pub fn capability(&self) -> &'static str {
        match self {
            Self::Planner => "planning",
            Self::Researcher => "research",
            Self::Coder => "coding",
            Self::Reviewer => "review",
            Self::Tester => "testing",
            Self::Documentation => "documentation",
            Self::Security => "security",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AutonomousWorker {
    pub id: String,
    pub role: WorkerRole,
    pub capacity: u16,
}

impl AutonomousWorker {
    fn scheduler_worker(&self) -> Worker {
        Worker {
            id: self.id.clone(),
            capabilities: vec![self.role.capability().to_owned()],
            healthy: true,
            capacity: self.capacity,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PendingReview {
    pub task_id: String,
    pub worker_id: String,
    pub summary: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReviewOutcome {
    pub task_id: String,
    pub reviewer_id: String,
    pub approved: bool,
    pub feedback: String,
}

/// Durable execution controller for one agent session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AutonomousExecution {
    pub session_id: String,
    pub scheduler: DynamicSchedule,
    #[serde(default)]
    pub worker_roles: BTreeMap<String, WorkerRole>,
    #[serde(default)]
    pub pending_reviews: BTreeMap<String, PendingReview>,
    #[serde(default)]
    pub review_history: Vec<ReviewOutcome>,
}

impl AutonomousExecution {
    /// Build and persist an execution graph from the current visible plan.
    pub fn start(session: &mut AgentSession, workers: Vec<Worker>) -> MedusaResult<Self> {
        let autonomous = workers
            .into_iter()
            .map(|worker| AutonomousWorker {
                id: worker.id,
                role: WorkerRole::Coder,
                capacity: worker.capacity,
            })
            .collect();
        Self::start_with_roles(session, autonomous, DEFAULT_MAX_ATTEMPTS)
    }

    pub fn start_with_attempts(
        session: &mut AgentSession,
        workers: Vec<Worker>,
        max_attempts: u32,
    ) -> MedusaResult<Self> {
        let autonomous = workers
            .into_iter()
            .map(|worker| AutonomousWorker {
                id: worker.id,
                role: WorkerRole::Coder,
                capacity: worker.capacity,
            })
            .collect();
        Self::start_with_roles(session, autonomous, max_attempts)
    }

    pub fn start_with_roles(
        session: &mut AgentSession,
        workers: Vec<AutonomousWorker>,
        max_attempts: u32,
    ) -> MedusaResult<Self> {
        if session.plan.is_empty() {
            return Err(validation_error(
                "autonomous execution requires a non-empty visible plan",
            ));
        }
        validate_workers(&workers)?;
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
                capabilities: vec![step_capability(&step.title).to_owned()],
                write_paths: Vec::new(),
                speculative: false,
            })
            .collect::<Vec<_>>();
        let scheduler_workers = workers
            .iter()
            .filter(|worker| worker.role != WorkerRole::Reviewer)
            .map(AutonomousWorker::scheduler_worker)
            .collect::<Vec<_>>();
        if scheduler_workers.is_empty() {
            return Err(validation_error(
                "autonomous execution requires at least one non-review worker",
            ));
        }
        let scheduler = DynamicSchedule::new(tasks, scheduler_workers, max_attempts)
            .map_err(validation_error)?;
        for step in &mut session.plan {
            if step.status != AgentPlanStepStatus::Completed {
                step.status = AgentPlanStepStatus::Pending;
            }
        }
        let execution = Self {
            session_id: session.id.to_string(),
            scheduler,
            worker_roles: workers
                .into_iter()
                .map(|worker| (worker.id, worker.role))
                .collect(),
            pending_reviews: BTreeMap::new(),
            review_history: Vec::new(),
        };
        execution.persist(session)?;
        Ok(execution)
    }

    /// Load a run after process restart and reject cross-session state reuse.
    pub fn load(session: &AgentSession) -> MedusaResult<Self> {
        let bytes = fs::read(execution_path(session))
            .map_err(|error| io_error("read autonomous execution", error))?;
        let execution: Self = serde_json::from_slice(&bytes).map_err(json_error)?;
        execution.ensure_session(session)?;
        execution.scheduler.validate().map_err(validation_error)?;
        Ok(execution)
    }

    /// Dispatch all currently ready tasks and synchronize them into the visible plan.
    pub fn dispatch_ready(&mut self, session: &mut AgentSession) -> MedusaResult<Vec<Assignment>> {
        self.ensure_session(session)?;
        let assignments = self.scheduler.dispatch_ready().map_err(validation_error)?;
        self.sync_and_persist(session)?;
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
        self.pending_reviews.remove(task_id);
        self.sync_and_persist(session)
    }

    pub fn submit_for_review(
        &mut self,
        session: &mut AgentSession,
        task_id: &str,
        worker_id: &str,
        summary: String,
    ) -> MedusaResult<()> {
        self.ensure_session(session)?;
        if self.worker_roles.get(worker_id) == Some(&WorkerRole::Reviewer) {
            return Err(validation_error(
                "reviewers cannot submit implementation work",
            ));
        }
        match self.scheduler.state(task_id) {
            Some(TaskState::Running {
                worker_id: assigned,
                ..
            }) if assigned == worker_id => {}
            _ => {
                return Err(validation_error(
                    "only the assigned running worker can submit work",
                ));
            }
        }
        if summary.trim().is_empty() {
            return Err(validation_error(
                "review submission summary cannot be empty",
            ));
        }
        self.pending_reviews.insert(
            task_id.to_owned(),
            PendingReview {
                task_id: task_id.to_owned(),
                worker_id: worker_id.to_owned(),
                summary,
            },
        );
        self.persist(session)
    }

    pub fn review(
        &mut self,
        session: &mut AgentSession,
        task_id: &str,
        reviewer_id: &str,
        approved: bool,
        feedback: String,
    ) -> MedusaResult<()> {
        self.ensure_session(session)?;
        if self.worker_roles.get(reviewer_id) != Some(&WorkerRole::Reviewer) {
            return Err(validation_error(
                "review decision requires a reviewer worker",
            ));
        }
        let pending = self
            .pending_reviews
            .remove(task_id)
            .ok_or_else(|| validation_error("task has no pending review"))?;
        if feedback.trim().is_empty() {
            return Err(validation_error("review feedback cannot be empty"));
        }
        if approved {
            self.scheduler
                .complete(task_id, &pending.worker_id)
                .map_err(validation_error)?;
        } else {
            self.scheduler
                .fail(task_id, &pending.worker_id, feedback.clone(), true)
                .map_err(validation_error)?;
        }
        self.review_history.push(ReviewOutcome {
            task_id: task_id.to_owned(),
            reviewer_id: reviewer_id.to_owned(),
            approved,
            feedback,
        });
        self.sync_and_persist(session)
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
        self.pending_reviews.remove(task_id);
        self.scheduler
            .fail(task_id, worker_id, reason, retryable)
            .map_err(validation_error)?;
        self.sync_and_persist(session)
    }

    pub fn set_worker_health(
        &mut self,
        session: &mut AgentSession,
        worker_id: &str,
        healthy: bool,
    ) -> MedusaResult<()> {
        self.ensure_session(session)?;
        if self.worker_roles.get(worker_id) == Some(&WorkerRole::Reviewer) {
            return Ok(());
        }
        self.scheduler
            .set_worker_health(worker_id, healthy)
            .map_err(validation_error)?;
        self.sync_and_persist(session)
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.scheduler.is_complete() && self.pending_reviews.is_empty()
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

    fn sync_and_persist(&self, session: &mut AgentSession) -> MedusaResult<()> {
        for (index, step) in session.plan.iter_mut().enumerate() {
            let id = task_id(index);
            let state = self
                .scheduler
                .state(&id)
                .ok_or_else(|| validation_error("execution task is missing from the scheduler"))?;
            step.status = if self.pending_reviews.contains_key(&id) {
                AgentPlanStepStatus::InProgress
            } else {
                match state {
                    TaskState::Pending { .. } => AgentPlanStepStatus::Pending,
                    TaskState::Running { .. } => AgentPlanStepStatus::InProgress,
                    TaskState::Succeeded => AgentPlanStepStatus::Completed,
                    TaskState::Failed { .. } => AgentPlanStepStatus::Failed,
                }
            };
        }
        self.persist(session)
    }

    fn persist(&self, session: &AgentSession) -> MedusaResult<()> {
        self.scheduler.validate().map_err(validation_error)?;
        let path = execution_path(session);
        let parent = path
            .parent()
            .ok_or_else(|| validation_error("autonomous execution path has no parent directory"))?;
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create autonomous execution directory", error))?;
        let temporary = path.with_extension("json.tmp");
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(self).map_err(json_error)?,
        )
        .map_err(|error| io_error("write autonomous execution", error))?;
        fs::rename(&temporary, &path)
            .map_err(|error| io_error("commit autonomous execution", error))?;
        Ok(())
    }
}

fn validate_workers(workers: &[AutonomousWorker]) -> MedusaResult<()> {
    if workers.is_empty() {
        return Err(validation_error("autonomous execution requires workers"));
    }
    let reviewer_count = workers
        .iter()
        .filter(|worker| worker.role == WorkerRole::Reviewer)
        .count();
    if reviewer_count == 0 {
        return Err(validation_error(
            "role-aware autonomous execution requires an independent reviewer",
        ));
    }
    for worker in workers {
        if worker.id.trim().is_empty() || worker.capacity == 0 {
            return Err(validation_error(
                "worker identifiers and capacity must be non-empty",
            ));
        }
    }
    Ok(())
}

fn step_capability(title: &str) -> &'static str {
    let title = title.to_ascii_lowercase();
    if title.contains("plan") || title.contains("design") || title.contains("architect") {
        "planning"
    } else if title.contains("inspect")
        || title.contains("research")
        || title.contains("investigate")
    {
        "research"
    } else if title.contains("test") || title.contains("verify") || title.contains("validate") {
        "testing"
    } else if title.contains("document") || title.contains("readme") {
        "documentation"
    } else if title.contains("security") || title.contains("audit") {
        "security"
    } else {
        "coding"
    }
}

fn task_id(index: usize) -> String {
    format!("plan-{index:04}")
}

fn execution_path(session: &AgentSession) -> PathBuf {
    session
        .repo
        .join(".medusa/executions")
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

    fn worker(id: &str, role: WorkerRole) -> AutonomousWorker {
        AutonomousWorker {
            id: id.to_owned(),
            role,
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
            completed: false,
            turn: 0,
            plan: vec![
                AgentPlanStep {
                    title: "Implement".to_owned(),
                    status: AgentPlanStepStatus::Pending,
                },
                AgentPlanStep {
                    title: "Test".to_owned(),
                    status: AgentPlanStepStatus::Pending,
                },
            ],
            pending_question: None,
            messages: Vec::new(),
            events: Vec::new(),
            evidence: Vec::new(),
            tool_artifacts: Vec::new(),
            world_model: None,
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
        }
    }

    #[test]
    fn reviewer_approval_releases_the_next_role_task() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = session(directory.path());
        let mut execution = AutonomousExecution::start_with_roles(
            &mut session,
            vec![
                worker("coder", WorkerRole::Coder),
                worker("tester", WorkerRole::Tester),
                worker("reviewer", WorkerRole::Reviewer),
            ],
            3,
        )
        .unwrap();

        let first = execution.dispatch_ready(&mut session).unwrap();
        assert_eq!(first[0].worker_id, "coder");
        execution
            .submit_for_review(&mut session, "plan-0000", "coder", "implemented".to_owned())
            .unwrap();
        assert!(execution.dispatch_ready(&mut session).unwrap().is_empty());
        execution
            .review(
                &mut session,
                "plan-0000",
                "reviewer",
                true,
                "looks correct".to_owned(),
            )
            .unwrap();
        let second = execution.dispatch_ready(&mut session).unwrap();
        assert_eq!(second[0].worker_id, "tester");
    }

    #[test]
    fn reviewer_rejection_requeues_work_with_feedback() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = session(directory.path());
        let mut execution = AutonomousExecution::start_with_roles(
            &mut session,
            vec![
                worker("coder", WorkerRole::Coder),
                worker("tester", WorkerRole::Tester),
                worker("reviewer", WorkerRole::Reviewer),
            ],
            3,
        )
        .unwrap();
        execution.dispatch_ready(&mut session).unwrap();
        execution
            .submit_for_review(&mut session, "plan-0000", "coder", "candidate".to_owned())
            .unwrap();
        execution
            .review(
                &mut session,
                "plan-0000",
                "reviewer",
                false,
                "missing error handling".to_owned(),
            )
            .unwrap();
        let retry = execution.dispatch_ready(&mut session).unwrap();
        assert_eq!(retry[0].task_id, "plan-0000");
        assert!(!execution.review_history[0].approved);
    }
}
