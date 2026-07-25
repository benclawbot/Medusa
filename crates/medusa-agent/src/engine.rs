#[allow(dead_code)]
mod autonomous_execution {
// Durable autonomous execution state connected to the user-visible agent plan.

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
            return Err(validation_error("reviewers cannot submit implementation work"));
        }
        match self.scheduler.state(task_id) {
            Some(TaskState::Running { worker_id: assigned, .. }) if assigned == worker_id => {}
            _ => return Err(validation_error("only the assigned running worker can submit work")),
        }
        if summary.trim().is_empty() {
            return Err(validation_error("review submission summary cannot be empty"));
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
            return Err(validation_error("review decision requires a reviewer worker"));
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
    } else if title.contains("inspect") || title.contains("research") || title.contains("investigate") {
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
}
mod context_budget { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/context_budget.rs")); }
mod coding_policy { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/coding_policy.rs")); }
mod repository_index { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/repository_index.rs")); }
mod world_model_observation { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/world_model_observation.rs")); }
use std::{collections::VecDeque, path::Path, sync::Mutex, thread};

use medusa_config::{Config, Mode};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use medusa_extensions::{DesktopCommanderClient, DesktopCommanderSettings};
use medusa_protocol::{Actor, EventPayload};
use medusa_provider::{Message, MessageBlock, ModelProvider, ModelRequest, ResponseBlock, Role};
use medusa_world_model::{WorkspaceModel, create_for_session, load as load_world_model};
use time::OffsetDateTime;

use crate::{
    approval::{ApprovalDecision, ApprovalGrant, ApprovalReceipt},
    engine_support::*,
    evidence::append_event,
    identity_guard::validate_provider_text,
    output_envelope::{OutputFormat, wrap as wrap_envelope},
    policy::validate_shell_command_hard_denials,
    session::{
        AgentPlanStep, AgentQuestion, AgentQuestionItem, AgentQuestionOption, AgentSession,
        PendingToolApproval, bootstrap, load, persist,
    },
    tools::{execute_approved_tool, execute_tool, input_string},
    verification::targeted_verification_for_paths,
};

pub(crate) const SYSTEM_PROMPT: &str = "You are Medusa, an independent autonomous coding agent. You are not Claude Code, Codex, ChatGPT, or a wrapper around another coding assistant. Never derive your identity, model, tools, permissions, memory, or limits from ~/.claude, CLAUDE.md, settings.json, or another product's configuration. Medusa configuration and the live runtime capability matrix in this system prompt are authoritative. Never claim a capability is absent when its runtime entry is available. Inspect the repository, make the smallest correct change, and verify it. Use tools rather than inventing repository contents. Use `fs_read` with path `.` to list repository files before reading a specific file, and use `fs_create_dir` to create directories. Call `shell_run` with an approved executable and argument array directly; never repeat the executable in the argument array, and never wrap commands in bash, sh, cmd, PowerShell, or shell operators. You have `web_search` for current public information and `web_fetch` for public pages; use them when the user requests current, external, or source-linked information. Issue independent read-only tool calls together in one response so they can run concurrently. Reuse tool results, avoid near-duplicate searches, and fetch only sources that materially support the answer. Use `update_plan` only for genuinely multi-step, risky, or long-running work; a simple single-file or static HTML task does not need a plan, design document, brainstorming skill, or specification unless the user explicitly requests one or repository instructions require it. When a tool fails, do not repeat the same unsupported command; use a direct filesystem tool or an approved executable that is available in the environment. When information from the user is needed to proceed, call `ask_user_question` with one to four concise multiple-choice questions in a single call, each with a short header and two to four options. Never put blocking questions in assistant text, and do not mark the plan or task complete while waiting. Never modify tests, verification scripts, snapshots, fixtures, or expected outputs unless the user explicitly asks for that exact change; fix the product code instead. Do not expose private chain-of-thought. Default to caveman chat: terse, direct, concrete, usually one to three short sentences. Avoid preambles, repetition, and broad explanations unless the user asks for detail. Report only the decision, action, result, and essential evidence.";
pub(crate) const PLAN_SYSTEM_PROMPT: &str = "You are Medusa, an independent coding agent, in read-only planning mode. You are not Claude Code or a wrapper around another assistant. Never derive identity, model, configuration, tools, permissions, memory, or limits from ~/.claude, CLAUDE.md, settings.json, or another product. Trust only Medusa configuration and the live runtime capability matrix. Inspect the repository and produce a concise, ordered implementation plan grounded in the files you examined. Use `update_plan` to maintain the visible plan as your understanding changes. When clarification is necessary, call `ask_user_question` with one to four concise multiple-choice questions in a single call, each with a short header and two to four options, then wait for its answer before producing a final plan. You can use `web_search` and `web_fetch` for current public information. Do not modify files, create commits, or claim that implementation work has been completed. Only read-only repository and web tools are available. Do not expose private chain-of-thought. Use terse, direct language and an ordered plan without commentary or repetition.";

/// Result of one durable model/tool step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepOutcome {
    Continue,
    TurnComplete,
    WaitingForUser,
    Completed,
}

const MAX_PARALLEL_TOOL_CALLS: usize = 8;

fn parallel_safe_tool(name: &str) -> bool {
    matches!(
        name,
        "fs_read" | "search_text" | "skill_read" | "web_search" | "web_fetch"
    )
}

pub(crate) fn map_parallel_ordered<T, U, F>(items: Vec<T>, operation: F) -> MedusaResult<Vec<U>>
where
    T: Send,
    U: Send,
    F: Fn(T) -> U + Sync,
{
    thread::scope(|scope| {
        let handles = items
            .into_iter()
            .map(|item| scope.spawn(|| operation(item)))
            .collect::<Vec<_>>();
        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            results.push(handle.join().map_err(|_| {
                MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Execution,
                    "parallel tool worker panicked",
                )
            })?);
        }
        Ok(results)
    })
}

/// A live update emitted while the engine executes one step.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentUpdate {
    Event(EventPayload),
    AssistantText(String),
    Plan(Vec<AgentPlanStep>),
    Question(AgentQuestion),
    ToolOutput {
        tool: String,
        output: String,
        is_error: bool,
    },
}

/// Persistent single-agent engine.
pub struct AgentEngine<P> {
    provider: P,
    config: Config,
    desktop_commander_settings: DesktopCommanderSettings,
    desktop_commander: Mutex<Option<DesktopCommanderClient>>,
}

fn audited_tool_name(name: &str, input: &serde_json::Value) -> String {
    if name == "desktop_commander" {
        if let Some(tool) = input.get("tool").and_then(serde_json::Value::as_str) {
            return format!("desktop_commander:{tool}");
        }
    }
    name.to_owned()
}

impl<P: ModelProvider> AgentEngine<P> {
    #[must_use]
    pub fn new(provider: P, config: Config) -> Self {
        Self {
            provider,
            config,
            desktop_commander_settings: DesktopCommanderSettings::from_env(),
            desktop_commander: Mutex::new(None),
        }
    }

    fn execute_desktop_commander(
        &self,
        repo: &Path,
        input: &serde_json::Value,
    ) -> MedusaResult<String> {
        let tool = input_string(input, "tool")?;
        let arguments = input.get("arguments").ok_or_else(|| {
            MedusaError::new(
                ErrorCode::InvalidConfiguration,
                ErrorCategory::Validation,
                "desktop_commander.arguments must be an object",
            )
        })?;
        let mut client = self.desktop_commander.lock().map_err(|_| {
            MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                "Desktop Commander client lock was poisoned",
            )
        })?;
        if client.is_none() {
            *client = Some(DesktopCommanderClient::connect(
                repo,
                self.desktop_commander_settings.clone(),
            )?);
        }
        let initialized = client.as_mut().ok_or_else(|| {
            MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                "Desktop Commander client was not initialized after a successful connection",
            )
        })?;
        let result = initialized.call_tool(
            repo,
            tool,
            arguments,
            self.config.agent.mode == Mode::ReadOnly,
        );
        if result.is_err() {
            client.take();
        }
        serde_json::to_string_pretty(&result?).map_err(Into::into)
    }

    pub fn create_session(&self, repo: &Path, objective: String) -> MedusaResult<AgentSession> {
        self.create_session_with_content(
            repo,
            objective.clone(),
            vec![MessageBlock::Text { text: objective }],
        )
    }

    pub fn create_session_with_content(
        &self,
        repo: &Path,
        objective: String,
        content: Vec<MessageBlock>,
    ) -> MedusaResult<AgentSession> {
        let content = content_with_session_goal(content, &objective);
        validate_user_content(&content, &self.provider.capabilities())?;
        bootstrap(repo)?;
        medusa_intelligence::recover_patch_transactions(repo)?;
        let now = OffsetDateTime::now_utc();
        let id = SessionId::new();
        let world_model = create_for_session(repo, id.as_str(), objective.clone()).ok();
        let mut session = AgentSession {
            id: id.clone(),
            objective: objective.clone(),
            repo: repo.to_path_buf(),
            created_at: now,
            updated_at: now,
            completed: false,
            turn: 0,
            plan: Vec::new(),
            pending_question: None,
            messages: vec![Message {
                role: Role::User,
                content,
            }],
            events: Vec::new(),
            evidence: Vec::new(),
            tool_artifacts: Vec::new(),
            world_model,
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
        };
        append_event(
            &mut session,
            Actor::User,
            EventPayload::SessionCreated { objective },
        )?;
        persist(&session)?;
        Ok(session)
    }

    pub fn load_session(&self, repo: &Path, session: &str) -> MedusaResult<AgentSession> {
        medusa_intelligence::recover_patch_transactions(repo)?;
        load(repo, session)
    }

    /// Loads the durable evidence model associated with a session, when enabled.
    pub fn load_session_world_model(
        &self,
        session: &AgentSession,
    ) -> MedusaResult<Option<WorkspaceModel>> {
        let Some(reference) = &session.world_model else {
            return Ok(None);
        };
        load_world_model(&session.repo, reference).map(Some).map_err(|error| {
            MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                format!("failed to load session world model: {error}"),
            )
        })
    }

    /// Adds a follow-up prompt to an existing session so later turns retain context.
    pub fn append_user_message(
        &self,
        session: &mut AgentSession,
        mut content: Vec<MessageBlock>,
    ) -> MedusaResult<()> {
        content.insert(
            0,
            MessageBlock::Text {
                text: format!("Current session goal: {}", session.objective),
            },
        );
        validate_user_content(&content, &self.provider.capabilities())?;
        let text = compact_message_text(&content);
        session.completed = false;
        session.turn = 0;
        session.messages.push(Message {
            role: Role::User,
            content,
        });
        append_event(
            session,
            Actor::User,
            EventPayload::UserPromptReceived { text },
        )?;
        session.updated_at = OffsetDateTime::now_utc();
        persist(session)
    }

    /// Resolves a blocking question with a single user response and resumes the same session.
    pub fn answer_pending_question(
        &self,
        session: &mut AgentSession,
        content: Vec<MessageBlock>,
    ) -> MedusaResult<()> {
        let question = session.pending_question.take().ok_or_else(|| {
            MedusaError::new(
                ErrorCode::InvalidConfiguration,
                ErrorCategory::Validation,
                "there is no pending question to answer",
            )
        })?;
        validate_user_content(&content, &self.provider.capabilities())?;
        let answer = compact_message_text(&content);
        if answer.trim().is_empty() {
            session.pending_question = Some(question);
            return Err(MedusaError::new(
                ErrorCode::InvalidConfiguration,
                ErrorCategory::Validation,
                "a question response cannot be empty",
            ));
        }
        session.completed = false;
        session.turn = 0;
        let content = if let Some(approval) = question.approval {
            let approved = answer.trim().eq_ignore_ascii_case("approve")
                || answer.trim().to_ascii_lowercase().starts_with("approve ");
            let now = OffsetDateTime::now_utc();
            let decision = if approved {
                approval
                    .grant
                    .authorizes(&approval.tool, &approval.input, &session.plan, now)
            } else {
                ApprovalDecision::Denied
            };
            session.approval_receipts.push(ApprovalReceipt {
                decision: decision.clone(),
                scope: approval.grant.scope.clone(),
                recorded_at: now,
                reason: if approved {
                    "user approved exact action".to_owned()
                } else {
                    format!("user denied action: {answer}")
                },
            });
            let (content, is_error) = if decision == ApprovalDecision::Approved {
                session.approval_grants.push(approval.grant);
                match execute_approved_tool(&session.repo, &approval.tool, &approval.input) {
                    Ok(output) => (format!("User approved this exact action.\n{output}"), false),
                    Err(error) => (format!("Approved action failed: {error}"), true),
                }
            } else {
                (
                    format!("Action was not authorized ({decision:?}). Feedback: {answer}"),
                    true,
                )
            };
            vec![MessageBlock::ToolResult {
                tool_use_id: approval.tool_use_id,
                content,
                is_error,
            }]
        } else {
            match question.tool_use_id {
                Some(tool_use_id) => vec![MessageBlock::ToolResult {
                    tool_use_id,
                    content: format!("User response: {answer}"),
                    is_error: false,
                }],
                None => vec![MessageBlock::Text {
                    text: format!("User response to the clarification question: {answer}"),
                }],
            }
        };
        session.messages.push(Message {
            role: Role::User,
            content,
        });
        append_event(
            session,
            Actor::User,
            EventPayload::UserPromptReceived { text: answer },
        )?;
        append_event(session, Actor::Coordinator, EventPayload::SessionResumed)?;
        session.updated_at = OffsetDateTime::now_utc();
        persist(session)
    }

    /// Updates the durable session objective without creating a new conversation.
    pub fn update_objective(
        &self,
        session: &mut AgentSession,
        objective: String,
    ) -> MedusaResult<()> {
        update_session_objective(session, objective)
    }

    /// Replaces prior message history with a bounded durable summary for the next model request.
    pub fn compact_session(
        &self,
        session: &mut AgentSession,
        focus: Option<&str>,
    ) -> MedusaResult<()> {
        compact_session(session, focus)
    }

    pub fn run_to_completion(&self, session: &mut AgentSession) -> MedusaResult<()> {
        while !session.completed && session.turn < self.config.agent.max_turns {
            match self.step(session)? {
                StepOutcome::WaitingForUser => {
                    return Err(MedusaError::new(
                        ErrorCode::DependencyUnavailable,
                        ErrorCategory::Execution,
                        "agent is waiting for a user response",
                    ));
                }
                StepOutcome::TurnComplete => return Ok(()),
                StepOutcome::Continue | StepOutcome::Completed => {}
            }
        }
        if session.completed {
            Ok(())
        } else {
            Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Execution,
                "agent exhausted max_turns before verification passed",
            ))
        }
    }

    pub fn step(&self, session: &mut AgentSession) -> MedusaResult<StepOutcome> {
        self.step_with_observer(session, |_| {})
    }

    pub fn step_with_observer<F>(
        &self,
        session: &mut AgentSession,
        observer: F,
    ) -> MedusaResult<StepOutcome>
    where
        F: FnMut(&AgentUpdate),
    {
        self.step_with_observer_and_context(session, None, observer)
    }

    pub fn step_with_observer_and_context<F>(
        &self,
        session: &mut AgentSession,
        additional_system_context: Option<&str>,
        mut observer: F,
    ) -> MedusaResult<StepOutcome>
    where
        F: FnMut(&AgentUpdate),
    {
        if session.completed {
            return Ok(StepOutcome::Completed);
        }
        if session.pending_question.is_some() {
            return Ok(StepOutcome::WaitingForUser);
        }
        validate_messages(&session.messages, &self.provider.capabilities())?;
        session.turn = session.turn.saturating_add(1);
        append_observed(
            session,
            EventPayload::ModelRequestStarted {
                provider: self.config.model.provider.clone(),
                model: self.config.model.name.clone(),
            },
            &mut observer,
        )?;
        if let Some(refresh) = repository_index::refresh(&session.repo)? {
            observer(&AgentUpdate::ToolOutput {
                tool: "code_index".to_owned(),
                output: repository_index::summary(&refresh),
                is_error: false,
            });
        }
        let mut system = coding_policy::apply(
            system_prompt_with_context(
                self.config.agent.mode,
                &session.repo,
                additional_system_context,
            ),
            self.config.agent.mode,
        );
        let tools = available_tools(self.config.agent.mode, &self.desktop_commander_settings);
        let mut budget = context_budget::PromptBudget::for_request(
            &system,
            &session.messages,
            &tools,
            self.config.model.max_output_tokens,
            context_budget::configured_context_window_tokens(),
        );
        let repository_capacity = budget
            .compaction_threshold_tokens
            .saturating_sub(budget.estimated_total_tokens);
        if let Some(retrieval) = repository_index::retrieve_context(
            &session.repo,
            &session.objective,
            repository_capacity,
        )? {
            system.push_str("\n\n");
            system.push_str(&retrieval.system_fragment);
            observer(&AgentUpdate::ToolOutput {
                tool: "repository_context".to_owned(),
                output: retrieval.status,
                is_error: false,
            });
            budget = context_budget::PromptBudget::for_request(
                &system,
                &session.messages,
                &tools,
                self.config.model.max_output_tokens,
                context_budget::configured_context_window_tokens(),
            );
        }
        let _remaining_context_tokens = budget.remaining_tokens();
        let _request_exceeds_context_window = budget.exceeds_context_window();
        let mut compacted = false;
        if matches!(
            budget.decision(),
            context_budget::PromptBudgetDecision::Compact
        ) {
            compact_session(
                session,
                Some("preserve the current objective, decisions, tool results, and pending work"),
            )?;
            validate_messages(&session.messages, &self.provider.capabilities())?;
            compacted = true;
        }
        let mut request = ModelRequest {
            system,
            messages: session.messages.clone(),
            tools,
            max_tokens: self.config.model.max_output_tokens,
            temperature_milli: self.config.model.temperature_milli,
        };
        let request_started = std::time::Instant::now();
        let response = match self.provider.complete(&request) {
            Ok(response) => response,
            Err(error) if context_budget::is_context_limit_rejection(&error.to_string()) => {
                if !compacted {
                    compact_session(
                        session,
                        Some(
                            "recover from the provider context limit while preserving the current objective, decisions, tool results, and pending work",
                        ),
                    )?;
                    validate_messages(&session.messages, &self.provider.capabilities())?;
                    request.messages = session.messages.clone();
                }
                self.provider.complete(&request)?
            }
            Err(error) => return Err(error),
        };
        let turn_usage = crate::session::record_turn_usage(
            session.turn,
            &request,
            &response,
            request_started.elapsed(),
        );
        append_observed(
            session,
            EventPayload::ModelResponseReceived {
                response_id: response.response_id.clone(),
                usage: serde_json::to_value(turn_usage).map_err(json_error)?,
            },
            &mut observer,
        )?;
        if let Some(status) = self.provider.execution_status() {
            append_observed(
                session,
                EventPayload::ProviderExecutionRecorded { status },
                &mut observer,
            )?;
        }

        let mut assistant_blocks = Vec::new();
        let mut assistant_text = Vec::new();
        let mut calls = VecDeque::new();
        for block in response.blocks {
            match block {
                ResponseBlock::Text { text } => {
                    let text = if validate_provider_text(&text).is_ok() {
                        text
                    } else {
                        "[provider output rejected: identity or policy contamination]".to_owned()
                    };
                    assistant_text.push(text.clone());
                    assistant_blocks.push(MessageBlock::Text { text });
                }
                ResponseBlock::ToolUse { id, name, input } => {
                    assistant_blocks.push(MessageBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    });
                    calls.push_back((id, name, input));
                }
            }
        }
        if !assistant_blocks.is_empty() {
            session.messages.push(Message {
                role: Role::Assistant,
                content: assistant_blocks,
            });
        }
        let fallback_question = calls
            .is_empty()
            .then(|| question_from_assistant_text(&assistant_text.join("\n")))
            .flatten();
        if fallback_question.is_none() && !assistant_text.is_empty() {
            observer(&AgentUpdate::AssistantText(assistant_text.join("\n")));
        }

        if let Some(question) = fallback_question {
            pause_for_question(session, question, &mut observer)?;
            return Ok(StepOutcome::WaitingForUser);
        }

        while !calls.is_empty() {
            let parallel_count = calls
                .iter()
                .take(MAX_PARALLEL_TOOL_CALLS)
                .take_while(|(_, name, _)| {
                    parallel_safe_tool(name) && tool_allowed(self.config.agent.mode, name)
                })
                .count();
            let batch_len = parallel_count.max(1);
            let batch = calls.drain(..batch_len).collect::<Vec<_>>();
            for (_, name, input) in &batch {
                append_observed(
                    session,
                    EventPayload::ToolCallRequested {
                        tool: audited_tool_name(name, input),
                        arguments: input.clone(),
                    },
                    &mut observer,
                )?;
            }

            let executed = if parallel_count > 1 {
                let repo = session.repo.as_path();
                map_parallel_ordered(batch, |(id, name, input)| {
                    let result = execute_tool(repo, &name, &input);
                    (id, name, input, result)
                })?
            } else {
                let (id, name, input) = batch.into_iter().next().ok_or_else(|| {
                    MedusaError::new(
                        ErrorCode::InternalInvariant,
                        ErrorCategory::Execution,
                        "tool batch was unexpectedly empty",
                    )
                })?;
                let result = if name == "update_plan" {
                    let plan = plan_from_input(&input);
                    if plan.is_empty() {
                        Ok("Visible task plan update ignored because it was empty.".to_owned())
                    } else {
                        if session.plan != plan {
                            let recorded_at = OffsetDateTime::now_utc();
                            for grant in session.approval_grants.drain(..) {
                                session.approval_receipts.push(ApprovalReceipt {
                                    decision: ApprovalDecision::Invalidated,
                                    scope: grant.scope,
                                    recorded_at,
                                    reason: "visible plan changed".to_owned(),
                                });
                            }
                        }
                        session.plan = plan.clone();
                        observer(&AgentUpdate::Plan(plan));
                        Ok("Visible task plan updated.".to_owned())
                    }
                } else if name == "ask_user_question" {
                    match question_from_input(id.clone(), &input) {
                        Ok(question) => {
                            pause_for_question(session, question, &mut observer)?;
                            return Ok(StepOutcome::WaitingForUser);
                        }
                        Err(error) => Err(error),
                    }
                } else if name == "desktop_commander" && tool_allowed(self.config.agent.mode, &name)
                {
                    self.execute_desktop_commander(&session.repo, &input)
                } else if tool_allowed(self.config.agent.mode, &name) {
                    execute_tool(&session.repo, &name, &input)
                } else {
                    let reason = "tool is unavailable in read-only planning mode".to_owned();
                    append_observed(
                        session,
                        EventPayload::ToolCallDenied {
                            tool: audited_tool_name(&name, &input),
                            reason: reason.clone(),
                        },
                        &mut observer,
                    )?;
                    Err(MedusaError::new(
                        ErrorCode::PolicyDenied,
                        ErrorCategory::Policy,
                        reason,
                    ))
                };
                vec![(id, name, input, result)]
            };

            for (id, name, input, result) in executed {
                if let Err(error) = &result
                    && error.code == ErrorCode::PolicyDenied
                    && self.config.agent.mode != Mode::ReadOnly
                    && interactively_approvable(&name, &input)
                {
                    let action = approval_action_label(&name, &input);
                    pause_for_question(
                        session,
                        AgentQuestion {
                            tool_use_id: Some(id.clone()),
                            questions: vec![AgentQuestionItem {
                                header: "Permission".to_owned(),
                                question: format!("Allow Medusa to {action}?"),
                                options: vec![
                                    AgentQuestionOption {
                                        label: "Approve".to_owned(),
                                        description: "Allow this exact action once".to_owned(),
                                    },
                                    AgentQuestionOption {
                                        label: "Deny".to_owned(),
                                        description: "Do not run this action".to_owned(),
                                    },
                                    AgentQuestionOption {
                                        label: "Provide feedback".to_owned(),
                                        description: "Type a different instruction below"
                                            .to_owned(),
                                    },
                                ],
                                multi_select: false,
                            }],
                            legacy_question: None,
                            legacy_options: Vec::new(),
                            approval: Some(PendingToolApproval {
                                grant: ApprovalGrant::exact_action(
                                    &name,
                                    &input,
                                    &session.plan,
                                    OffsetDateTime::now_utc(),
                                ),
                                tool_use_id: id,
                                tool: name,
                                input,
                            }),
                        },
                        &mut observer,
                    )?;
                    return Ok(StepOutcome::WaitingForUser);
                }
                let event_tool = audited_tool_name(&name, &input);
                let (raw_content, is_error, exit_code) = match result {
                    Ok(output) => (output, false, Some(0)),
                    Err(error) => (error.to_string(), true, Some(1)),
                };
                world_model_observation::record_tool_observation(
                    session,
                    &name,
                    &input,
                    &raw_content,
                    if is_error { 1 } else { 0 },
                );
                append_observed(
                    session,
                    EventPayload::ToolExecutionCompleted {
                        tool: event_tool,
                        exit_code,
                    },
                    &mut observer,
                )?;
                // The TUI sees the full body verbatim; the model sees the compact
                // head/tail envelope with a pointer to the on-disk artifact.
                observer(&AgentUpdate::ToolOutput {
                    tool: name.clone(),
                    output: raw_content.clone(),
                    is_error,
                });
                let envelope_cfg = default_envelope_config(&session.repo);
                let model_content = match wrap_envelope(
                    &name,
                    raw_content.as_bytes(),
                    OutputFormat::Plain,
                    &envelope_cfg,
                ) {
                    Ok(env) => {
                        let compact = compact_envelope_for_model(&env);
                        // Persist the artifact path on the session for later
                        // reference (cleanup, replay). Currently unused by
                        // downstream consumers — Task 7 wires SessionBrowser on top.
                        session.tool_artifacts.push(env.path.clone());
                        if is_error {
                            format!("[error]\n{compact}")
                        } else {
                            compact
                        }
                    }
                    Err(_) => {
                        // Envelope wrap failed (rare — disk full, perms). Fall back
                        // to the raw body so the model still sees output.
                        raw_content.clone()
                    }
                };
                session.messages.push(Message {
                    role: Role::User,
                    content: vec![MessageBlock::ToolResult {
                        tool_use_id: id,
                        content: model_content,
                        is_error,
                    }],
                });
                persist(session)?;
            }
        }

        if response.stop_reason.as_deref() == Some("end_turn")
            && !session.messages.last().is_some_and(|message| {
                matches!(
                    message.content.first(),
                    Some(MessageBlock::ToolResult { .. })
                )
            })
        {
            if self.config.agent.mode == Mode::ReadOnly || !has_mutating_tool_result(session) {
                session.updated_at = OffsetDateTime::now_utc();
                persist(session)?;
                return Ok(StepOutcome::TurnComplete);
            }
            append_observed(
                session,
                EventPayload::VerificationStarted {
                    commands: Vec::new(),
                },
                &mut observer,
            )?;
            let mut verification = targeted_verification_for_paths(
                &session.repo,
                &successful_mutation_paths(session),
            )?;
            let transaction_ids = medusa_intelligence::finalize_patch_transactions(
                &session.repo,
                verification.passed,
            )?;
            verification.evidence.extend(transaction_ids.into_iter().map(|transaction_id| {
                if verification.passed {
                    format!("patch_transaction_committed={transaction_id}")
                } else {
                    format!("patch_transaction_rolled_back={transaction_id}")
                }
            }));
            append_observed(
                session,
                EventPayload::VerificationCompleted {
                    passed: verification.passed,
                    evidence: verification.evidence.clone(),
                },
                &mut observer,
            )?;
            session.evidence.extend(verification.evidence.clone());
            if verification.passed && plan_is_complete(session) {
                session.completed = true;
                append_observed(
                    session,
                    EventPayload::SessionCompleted {
                        report_ref: format!("session:{}.json", session.id),
                    },
                    &mut observer,
                )?;
            } else if !verification.passed {
                session.messages.push(Message {
                    role: Role::User,
                    content: vec![MessageBlock::Text {
                        text: format!(
                            "Verification failed. Fix the remaining issue. Evidence:\n{}",
                            verification.evidence.join("\n")
                        ),
                    }],
                });
            }
        }
        session.updated_at = OffsetDateTime::now_utc();
        persist(session)?;
        Ok(if session.completed {
            StepOutcome::Completed
        } else if response.stop_reason.as_deref() == Some("end_turn") {
            StepOutcome::TurnComplete
        } else {
            StepOutcome::Continue
        })
    }
}

fn approval_action_label(name: &str, input: &serde_json::Value) -> String {
    match name {
        "fs_write" => format!(
            "write {}",
            input
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("the requested file")
        ),
        "fs_create_dir" => format!(
            "create {}",
            input
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("the requested directory")
        ),
        "shell_run" => format!(
            "run {} {}",
            input
                .get("program")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("the requested command"),
            input
                .get("args")
                .and_then(serde_json::Value::as_array)
                .map(|args| args
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "))
                .unwrap_or_default()
        )
        .trim()
        .to_owned(),
        _ => "run the requested action".to_owned(),
    }
}

fn interactively_approvable(name: &str, input: &serde_json::Value) -> bool {
    match name {
        "fs_write" | "fs_create_dir" => input
            .get("path")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|path| Path::new(path).is_absolute()),
        "shell_run" => {
            let Some(program) = input.get("program").and_then(serde_json::Value::as_str) else {
                return false;
            };
            let Some(args) = input.get("args").and_then(serde_json::Value::as_array) else {
                return false;
            };
            let Some(args) = args
                .iter()
                .map(serde_json::Value::as_str)
                .map(|arg| arg.map(str::to_owned))
                .collect::<Option<Vec<_>>>()
            else {
                return false;
            };
            validate_shell_command_hard_denials(program, &args).is_ok()
        }
        _ => false,
    }
}

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/autonomous_engine.rs"));