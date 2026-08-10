//! Read-only, event-grounded live-session observation and side questions.
//!
//! This module deliberately consumes only durable/projected user-visible state. It never asks the
//! primary agent for hidden state, never exposes provider reasoning blocks, and never writes to the
//! authoritative session journal.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use medusa_agent::{AgentPlanStepStatus, session_browser::load_session};
use medusa_protocol::{EventPayload, SessionState};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{RuntimeController, RuntimeError, TeamSnapshot, lock_submission};

const MAX_RECENT_ACTIONS: usize = 24;
const MAX_RECENT_MESSAGES: usize = 12;
const MAX_TEXT_CHARS: usize = 800;
const MAX_EVIDENCE_ITEMS: usize = 16;
const MAX_SIDE_QUESTION_CHARS: usize = 1_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationStage {
    Idle,
    Running,
    WaitingForUser,
    Verifying,
    Blocked,
    Failed,
    Completed,
    Cancelling,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObservedPlanStep {
    pub title: String,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObservationVerification {
    pub passed: Option<bool>,
    pub evidence: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObservationMessage {
    pub sequence: u64,
    pub role: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionObservationSnapshot {
    pub session_id: String,
    pub objective: String,
    /// Last committed journal sequence observed by this snapshot.
    pub event_sequence: u64,
    /// Authoritative session revision. Today this is the last committed event sequence.
    pub revision: u64,
    pub turn: u32,
    pub stage: ObservationStage,
    pub active_plan_step: Option<ObservedPlanStep>,
    pub remaining_plan_steps: Vec<ObservedPlanStep>,
    pub active_tools: Vec<String>,
    pub files_read: Vec<String>,
    pub files_changed: Vec<String>,
    pub verification: Option<ObservationVerification>,
    pub blocker: Option<String>,
    pub recent_actions: Vec<String>,
    pub recent_messages: Vec<ObservationMessage>,
    pub team: TeamSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SideQuestionRequest {
    pub target_session_id: String,
    pub question: String,
    #[serde(default)]
    pub expected_revision: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SideQuestionResponse {
    pub session_id: String,
    pub observed_revision: u64,
    pub answer: String,
    pub cancelled: bool,
    pub snapshot: SessionObservationSnapshot,
}

/// Cancellation is deliberately independent of the primary runtime cancellation flag.
#[derive(Clone, Debug, Default)]
pub struct SideQuestionCancelToken {
    cancelled: Arc<AtomicBool>,
}

impl SideQuestionCancelToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Builds a canonical observer snapshot from the durable session journal.
pub fn observe_session(
    repo: &Path,
    session_id: &str,
) -> Result<SessionObservationSnapshot, RuntimeError> {
    build_observation(repo, session_id, TeamSnapshot::default())
}

/// Answers one bounded, deterministic side question without invoking tools or mutating the run.
pub fn answer_side_question(
    repo: &Path,
    request: &SideQuestionRequest,
    cancel: &SideQuestionCancelToken,
) -> Result<SideQuestionResponse, RuntimeError> {
    validate_side_question(request)?;
    let snapshot = observe_session(repo, &request.target_session_id)?;
    answer_from_snapshot(snapshot, request, cancel)
}

impl RuntimeController {
    /// Returns the read-only canonical projection for this controller's active session.
    pub fn observe_active_session(
        &self,
    ) -> Result<Option<SessionObservationSnapshot>, RuntimeError> {
        let session_id = lock_submission(&self.submission).active_session_id.clone();
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        build_observation(&self.repo, &session_id, self.team_control.snapshot()).map(Some)
    }

    /// Answers a side question about the active session without entering the command/action plane.
    /// The independent cancellation token cannot cancel or steer primary work.
    pub fn ask_side_question(
        &self,
        request: &SideQuestionRequest,
        cancel: &SideQuestionCancelToken,
    ) -> Result<SideQuestionResponse, RuntimeError> {
        validate_side_question(request)?;
        let active_session_id = lock_submission(&self.submission).active_session_id.clone();
        if active_session_id.as_deref() != Some(request.target_session_id.as_str()) {
            return Err(RuntimeError::InvalidCommand(
                "side question target is not the controller's active session".to_owned(),
            ));
        }
        let snapshot = build_observation(
            &self.repo,
            &request.target_session_id,
            self.team_control.snapshot(),
        )?;
        answer_from_snapshot(snapshot, request, cancel)
    }
}

fn validate_side_question(request: &SideQuestionRequest) -> Result<(), RuntimeError> {
    if request.target_session_id.trim().is_empty() {
        return Err(RuntimeError::InvalidCommand(
            "side question requires a target session".to_owned(),
        ));
    }
    let question = request.question.trim();
    if question.is_empty() {
        return Err(RuntimeError::InvalidCommand(
            "side question cannot be empty".to_owned(),
        ));
    }
    if question.chars().count() > MAX_SIDE_QUESTION_CHARS {
        return Err(RuntimeError::InvalidCommand(format!(
            "side question exceeds {MAX_SIDE_QUESTION_CHARS} characters"
        )));
    }
    Ok(())
}

fn answer_from_snapshot(
    snapshot: SessionObservationSnapshot,
    request: &SideQuestionRequest,
    cancel: &SideQuestionCancelToken,
) -> Result<SideQuestionResponse, RuntimeError> {
    if let Some(expected) = request.expected_revision
        && expected != snapshot.revision
    {
        return Err(RuntimeError::InvalidCommand(format!(
            "stale observation revision: expected {expected}, authoritative revision is {}",
            snapshot.revision
        )));
    }
    if cancel.is_cancelled() {
        return Ok(SideQuestionResponse {
            session_id: snapshot.session_id.clone(),
            observed_revision: snapshot.revision,
            answer: "Side question cancelled; primary session was not affected.".to_owned(),
            cancelled: true,
            snapshot,
        });
    }

    let answer = render_answer(&snapshot, &request.question);
    Ok(SideQuestionResponse {
        session_id: snapshot.session_id.clone(),
        observed_revision: snapshot.revision,
        answer,
        cancelled: false,
        snapshot,
    })
}

fn build_observation(
    repo: &Path,
    session_id: &str,
    mut team: TeamSnapshot,
) -> Result<SessionObservationSnapshot, RuntimeError> {
    let session = load_session(repo, session_id).map_err(RuntimeError::agent)?;
    let revision = session.events.last().map_or(0, |event| event.sequence);
    let mut stage = if session.completed {
        ObservationStage::Completed
    } else {
        ObservationStage::Idle
    };
    let mut blocker = None;
    let mut active_tools = BTreeMap::<String, usize>::new();
    let mut files_read = BTreeSet::<String>::new();
    let mut files_changed = BTreeSet::<String>::new();
    let mut verification = None;
    let mut recent_actions = VecDeque::<String>::new();
    let mut recent_messages = VecDeque::<ObservationMessage>::new();
    let mut approval_pending = false;

    for event in &session.events {
        if let Some(summary) = event_summary(&event.payload) {
            push_bounded(&mut recent_actions, summary, MAX_RECENT_ACTIONS);
        }
        match &event.payload {
            EventPayload::SessionStateChanged { to, .. } => {
                stage = stage_from_state(*to);
            }
            EventPayload::UserPromptReceived { text } => {
                push_bounded(
                    &mut recent_messages,
                    ObservationMessage {
                        sequence: event.sequence,
                        role: "user".to_owned(),
                        text: redact_text(text),
                    },
                    MAX_RECENT_MESSAGES,
                );
                if !session.completed {
                    stage = ObservationStage::Running;
                }
            }
            EventPayload::AssistantMessageRecorded { message } => {
                if let Some(text) = extract_public_text(message) {
                    push_bounded(
                        &mut recent_messages,
                        ObservationMessage {
                            sequence: event.sequence,
                            role: "assistant".to_owned(),
                            text,
                        },
                        MAX_RECENT_MESSAGES,
                    );
                }
            }
            EventPayload::ToolCallRequested { tool, arguments } => {
                if tool.to_ascii_lowercase().contains("read") {
                    collect_paths(arguments, &mut files_read);
                }
            }
            EventPayload::ToolExecutionStarted { tool } => {
                *active_tools.entry(tool.clone()).or_default() += 1;
                if !matches!(stage, ObservationStage::WaitingForUser | ObservationStage::Verifying)
                {
                    stage = ObservationStage::Running;
                }
            }
            EventPayload::ToolExecutionCompleted { tool, .. }
            | EventPayload::ToolCallDenied { tool, .. } => {
                if let Some(count) = active_tools.get_mut(tool) {
                    *count = count.saturating_sub(1);
                }
            }
            EventPayload::FileTransactionCommitted { paths, .. } => {
                files_changed.extend(paths.iter().cloned());
            }
            EventPayload::VerificationStarted { .. } => {
                stage = ObservationStage::Verifying;
                verification = Some(ObservationVerification {
                    passed: None,
                    evidence: Vec::new(),
                });
            }
            EventPayload::VerificationCompleted { passed, evidence } => {
                verification = Some(ObservationVerification {
                    passed: Some(*passed),
                    evidence: evidence
                        .iter()
                        .take(MAX_EVIDENCE_ITEMS)
                        .map(|item| redact_text(item))
                        .collect(),
                });
                if !session.completed {
                    stage = if *passed {
                        ObservationStage::Running
                    } else {
                        ObservationStage::Blocked
                    };
                }
                if !passed {
                    blocker = Some("latest verification failed".to_owned());
                }
            }
            EventPayload::ApprovalRequested { .. } => {
                approval_pending = true;
                blocker = Some("approval required".to_owned());
                stage = ObservationStage::WaitingForUser;
            }
            EventPayload::ApprovalDecisionRecorded { .. } => {
                approval_pending = false;
                if blocker.as_deref() == Some("approval required") {
                    blocker = None;
                }
            }
            EventPayload::QuestionRequested { .. } => {
                blocker = Some("user response required".to_owned());
                stage = ObservationStage::WaitingForUser;
            }
            EventPayload::CancellationRequested { .. } => {
                stage = ObservationStage::Cancelling;
                blocker = Some("cancellation requested".to_owned());
            }
            EventPayload::CancellationCompleted => {
                stage = ObservationStage::Cancelled;
                blocker = None;
            }
            EventPayload::RuntimeFailed { message } => {
                stage = ObservationStage::Failed;
                blocker = Some(redact_text(message));
            }
            EventPayload::SessionFailed { error } => {
                stage = ObservationStage::Failed;
                blocker = Some(redact_text(&error.to_string()));
            }
            EventPayload::SessionPaused { reason } => {
                stage = ObservationStage::Blocked;
                blocker = Some(redact_text(reason));
            }
            EventPayload::SessionResumed => {
                stage = ObservationStage::Running;
                blocker = None;
            }
            EventPayload::SessionCompleted { .. } => {
                stage = ObservationStage::Completed;
                blocker = None;
            }
            EventPayload::RuntimeTurnFinished if !session.completed => {
                stage = ObservationStage::Idle;
            }
            _ => {}
        }
    }

    if session.pending_question.is_some() || approval_pending {
        stage = ObservationStage::WaitingForUser;
        blocker.get_or_insert_with(|| "user action required".to_owned());
    }

    let active_tools = active_tools
        .into_iter()
        .filter_map(|(tool, count)| (count > 0).then_some(tool))
        .collect();
    let mut active_plan_step = None;
    let mut remaining_plan_steps = Vec::new();
    for step in &session.plan {
        let observed = ObservedPlanStep {
            title: bounded_text(&step.title),
            status: plan_status(step.status),
        };
        if step.status == AgentPlanStepStatus::InProgress && active_plan_step.is_none() {
            active_plan_step = Some(observed.clone());
        }
        if step.status != AgentPlanStepStatus::Completed {
            remaining_plan_steps.push(observed);
        }
    }

    for worker in &mut team.workers {
        worker.last_update = redact_text(&worker.last_update);
    }

    Ok(SessionObservationSnapshot {
        session_id: session.id.to_string(),
        objective: bounded_text(&session.objective),
        event_sequence: revision,
        revision,
        turn: session.turn,
        stage,
        active_plan_step,
        remaining_plan_steps,
        active_tools,
        files_read: files_read.into_iter().collect(),
        files_changed: files_changed.into_iter().collect(),
        verification,
        blocker,
        recent_actions: recent_actions.into_iter().collect(),
        recent_messages: recent_messages.into_iter().collect(),
        team,
    })
}

fn stage_from_state(state: SessionState) -> ObservationStage {
    match state {
        SessionState::Verifying | SessionState::Reviewing => ObservationStage::Verifying,
        SessionState::Blocked | SessionState::Paused | SessionState::BudgetExhausted => {
            ObservationStage::Blocked
        }
        SessionState::Completed => ObservationStage::Completed,
        SessionState::CancelRequested => ObservationStage::Cancelling,
        SessionState::Cancelled => ObservationStage::Cancelled,
        SessionState::Crashed => ObservationStage::Failed,
        SessionState::Created => ObservationStage::Idle,
        SessionState::Bootstrapping
        | SessionState::Understanding
        | SessionState::Planning
        | SessionState::Executing
        | SessionState::Learning
        | SessionState::Recovering => ObservationStage::Running,
    }
}

fn plan_status(status: AgentPlanStepStatus) -> String {
    match status {
        AgentPlanStepStatus::Pending => "pending",
        AgentPlanStepStatus::InProgress => "in_progress",
        AgentPlanStepStatus::Completed => "completed",
    }
    .to_owned()
}

fn event_summary(payload: &EventPayload) -> Option<String> {
    let summary = match payload {
        EventPayload::SessionCreated { .. } => "session created".to_owned(),
        EventPayload::UserPromptReceived { .. } => "user prompt received".to_owned(),
        EventPayload::ToolCallRequested { tool, .. } => format!("tool requested: {tool}"),
        EventPayload::ToolCallDenied { tool, .. } => format!("tool denied: {tool}"),
        EventPayload::ToolExecutionStarted { tool } => format!("tool started: {tool}"),
        EventPayload::ToolExecutionCompleted { tool, exit_code } => {
            format!("tool completed: {tool} ({exit_code:?})")
        }
        EventPayload::FileTransactionCommitted { paths, .. } => {
            format!("repository transaction committed: {} path(s)", paths.len())
        }
        EventPayload::VerificationStarted { .. } => "verification started".to_owned(),
        EventPayload::VerificationCompleted { passed, .. } => {
            format!("verification completed: {}", if *passed { "passed" } else { "failed" })
        }
        EventPayload::ApprovalRequested { .. } => "approval requested".to_owned(),
        EventPayload::ApprovalDecisionRecorded { .. } => "approval decision recorded".to_owned(),
        EventPayload::QuestionRequested { .. } => "user question requested".to_owned(),
        EventPayload::AssistantMessageRecorded { .. } => "assistant message recorded".to_owned(),
        EventPayload::TeamStateChanged { .. } => "team state updated".to_owned(),
        EventPayload::WorkerEvidenceRecorded { .. } => "worker evidence recorded".to_owned(),
        EventPayload::CancellationRequested { .. } => "cancellation requested".to_owned(),
        EventPayload::CancellationCompleted => "cancellation completed".to_owned(),
        EventPayload::RuntimeFailed { .. } | EventPayload::SessionFailed { .. } => {
            "runtime failed".to_owned()
        }
        EventPayload::RuntimeTurnFinished => "runtime turn finished".to_owned(),
        EventPayload::SessionCompleted { .. } => "session completed".to_owned(),
        EventPayload::CheckpointRestoreRequested { .. } => "checkpoint restore requested".to_owned(),
        EventPayload::CheckpointCreated { .. } => "checkpoint created".to_owned(),
        _ => return None,
    };
    Some(summary)
}

fn collect_paths(value: &Value, paths: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if key.eq_ignore_ascii_case("path") {
                    if let Some(path) = value.as_str() {
                        paths.insert(bounded_text(path));
                    }
                } else if key.eq_ignore_ascii_case("paths") {
                    if let Some(values) = value.as_array() {
                        for value in values {
                            if let Some(path) = value.as_str() {
                                paths.insert(bounded_text(path));
                            }
                        }
                    }
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_paths(value, paths);
            }
        }
        _ => {}
    }
}

fn extract_public_text(value: &Value) -> Option<String> {
    let mut fragments = Vec::new();
    collect_public_text(value, None, &mut fragments);
    if fragments.is_empty() {
        None
    } else {
        Some(bounded_text(&redact_text(&fragments.join("\n"))))
    }
}

fn collect_public_text(value: &Value, parent_key: Option<&str>, output: &mut Vec<String>) {
    if parent_key.is_some_and(sensitive_key) {
        return;
    }
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if sensitive_key(key) {
                    continue;
                }
                if matches!(key.as_str(), "text" | "content") {
                    if let Some(text) = value.as_str() {
                        output.push(redact_text(text));
                        continue;
                    }
                }
                collect_public_text(value, Some(key), output);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_public_text(value, parent_key, output);
            }
        }
        _ => {}
    }
}

fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "thinking",
        "reasoning",
        "chain_of_thought",
        "chain-of-thought",
        "system_prompt",
        "system-prompt",
        "credential",
        "password",
        "secret",
        "api_key",
        "api-key",
        "environment",
    ]
    .iter()
    .any(|needle| key.contains(needle))
        || key == "token"
        || key.ends_with("_token")
}

fn redact_text(input: &str) -> String {
    let redacted = input
        .split_whitespace()
        .map(|token| {
            let lower = token.to_ascii_lowercase();
            if lower.starts_with("sk-")
                || lower.contains("api_key=")
                || lower.contains("api-key=")
                || lower.contains("token=")
                || lower.contains("password=")
                || lower.contains("secret=")
                || lower.contains("authorization:bearer")
            {
                "[REDACTED]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    bounded_text(&redacted)
}

fn bounded_text(input: &str) -> String {
    let mut chars = input.chars();
    let bounded = chars.by_ref().take(MAX_TEXT_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

fn push_bounded<T>(queue: &mut VecDeque<T>, value: T, max: usize) {
    queue.push_back(value);
    while queue.len() > max {
        queue.pop_front();
    }
}

fn render_answer(snapshot: &SessionObservationSnapshot, question: &str) -> String {
    let question = question.to_ascii_lowercase();
    if question.contains("file") {
        return format!(
            "Observed at revision {}. Files read: {}. Files changed: {}.",
            snapshot.revision,
            display_list(&snapshot.files_read),
            display_list(&snapshot.files_changed)
        );
    }
    if question.contains("verif") || question.contains("test") || question.contains("check") {
        return match &snapshot.verification {
            Some(verification) => format!(
                "Observed at revision {}. Verification: {}. Evidence: {}.",
                snapshot.revision,
                match verification.passed {
                    Some(true) => "passed",
                    Some(false) => "failed",
                    None => "in progress",
                },
                display_list(&verification.evidence)
            ),
            None => format!(
                "Observed at revision {}. No verification result is recorded yet.",
                snapshot.revision
            ),
        };
    }
    if question.contains("tool") || question.contains("process") {
        return format!(
            "Observed at revision {}. Active tools/processes: {}.",
            snapshot.revision,
            display_list(&snapshot.active_tools)
        );
    }
    if question.contains("block")
        || question.contains("wait")
        || question.contains("approval")
        || question.contains("action")
    {
        return format!(
            "Observed at revision {}. Stage: {:?}. Required user action: {}.",
            snapshot.revision,
            snapshot.stage,
            snapshot.blocker.as_deref().unwrap_or("none")
        );
    }
    if question.contains("team") || question.contains("child") || question.contains("worker") {
        let workers = snapshot
            .team
            .workers
            .iter()
            .map(|worker| format!("{}:{:?}", worker.worker_id, worker.lifecycle))
            .collect::<Vec<_>>();
        return format!(
            "Observed at revision {}. Team workers: {}.",
            snapshot.revision,
            display_list(&workers)
        );
    }

    let active_step = snapshot
        .active_plan_step
        .as_ref()
        .map(|step| step.title.as_str())
        .unwrap_or("none");
    format!(
        "Observed at revision {}. Stage: {:?}. Active step: {}. Remaining steps: {}. Blocker: {}.",
        snapshot.revision,
        snapshot.stage,
        active_step,
        snapshot.remaining_plan_steps.len(),
        snapshot.blocker.as_deref().unwrap_or("none")
    )
}

fn display_list(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use medusa_agent::{AgentEngine, AgentPlanStep, record_session_event};
    use medusa_config::Config;
    use medusa_core::MedusaResult;
    use medusa_protocol::Actor;
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};
    use serde_json::json;

    use super::*;

    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("observer tests do not call a model")
        }
    }

    #[test]
    fn snapshot_projects_running_tools_files_verification_and_revision() {
        let repository = tempfile::tempdir().expect("repository");
        fs::write(repository.path().join("lib.rs"), "fn main() {}\n").expect("fixture");
        let engine = AgentEngine::new(UnusedProvider, Config::default());
        let mut session = engine
            .create_session(repository.path(), "inspect live work".to_owned())
            .expect("session");
        session.plan = vec![AgentPlanStep {
            title: "Inspect files".to_owned(),
            status: AgentPlanStepStatus::InProgress,
        }];
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::ToolCallRequested {
                tool: "fs_read".to_owned(),
                arguments: json!({"path":"lib.rs","api_key":"sk-never-expose"}),
            },
        )
        .expect("tool request");
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::ToolExecutionStarted {
                tool: "fs_read".to_owned(),
            },
        )
        .expect("tool start");
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::FileTransactionCommitted {
                paths: vec!["src/lib.rs".to_owned()],
                rollback_ref: "artifact".to_owned(),
            },
        )
        .expect("transaction");
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::VerificationStarted {
                commands: vec!["cargo test".to_owned()],
            },
        )
        .expect("verification");

        let snapshot = observe_session(repository.path(), session.id.as_str()).expect("snapshot");
        assert_eq!(snapshot.event_sequence, snapshot.revision);
        assert_eq!(snapshot.stage, ObservationStage::Verifying);
        assert_eq!(snapshot.active_tools, vec!["fs_read"]);
        assert_eq!(snapshot.files_read, vec!["lib.rs"]);
        assert_eq!(snapshot.files_changed, vec!["src/lib.rs"]);
        assert_eq!(
            snapshot.active_plan_step.as_ref().map(|step| step.title.as_str()),
            Some("Inspect files")
        );
        assert!(!serde_json::to_string(&snapshot).unwrap().contains("sk-never-expose"));
    }

    #[test]
    fn sensitive_reasoning_and_credentials_are_excluded_or_redacted() {
        let value = json!({
            "text": "Visible result token=super-secret",
            "thinking": "private chain of thought",
            "reasoning": {"content":"hidden reasoning"},
            "credential": "credential-value"
        });
        let text = extract_public_text(&value).expect("public text");
        assert!(text.contains("Visible result"));
        assert!(text.contains("[REDACTED]"));
        assert!(!text.contains("private chain"));
        assert!(!text.contains("hidden reasoning"));
        assert!(!text.contains("credential-value"));
    }

    #[test]
    fn side_question_is_read_only_and_stale_revision_is_rejected() {
        let repository = tempfile::tempdir().expect("repository");
        let engine = AgentEngine::new(UnusedProvider, Config::default());
        let session = engine
            .create_session(repository.path(), "stay unchanged".to_owned())
            .expect("session");
        let before = load_session(repository.path(), session.id.as_str()).expect("before");
        let snapshot = observe_session(repository.path(), session.id.as_str()).expect("snapshot");
        let request = SideQuestionRequest {
            target_session_id: session.id.to_string(),
            question: "What is this session doing now?".to_owned(),
            expected_revision: Some(snapshot.revision),
        };
        let response = answer_side_question(
            repository.path(),
            &request,
            &SideQuestionCancelToken::default(),
        )
        .expect("side answer");
        assert!(!response.cancelled);
        let after = load_session(repository.path(), session.id.as_str()).expect("after");
        assert_eq!(before.events.len(), after.events.len());
        assert_eq!(before.messages, after.messages);

        let stale = SideQuestionRequest {
            expected_revision: Some(snapshot.revision + 1),
            ..request
        };
        assert!(answer_side_question(
            repository.path(),
            &stale,
            &SideQuestionCancelToken::default()
        )
        .is_err());
    }

    #[test]
    fn side_question_cancellation_is_independent_and_history_is_bounded() {
        let repository = tempfile::tempdir().expect("repository");
        let engine = AgentEngine::new(UnusedProvider, Config::default());
        let mut session = engine
            .create_session(repository.path(), "bounded observer".to_owned())
            .expect("session");
        for index in 0..40 {
            record_session_event(
                &mut session,
                Actor::User,
                EventPayload::UserPromptReceived {
                    text: format!("message {index}"),
                },
            )
            .expect("message event");
        }
        let token = SideQuestionCancelToken::default();
        token.cancel();
        let response = answer_side_question(
            repository.path(),
            &SideQuestionRequest {
                target_session_id: session.id.to_string(),
                question: "What happened?".to_owned(),
                expected_revision: None,
            },
            &token,
        )
        .expect("cancelled side question");
        assert!(response.cancelled);
        assert!(response.snapshot.recent_messages.len() <= MAX_RECENT_MESSAGES);
        assert!(response.snapshot.recent_actions.len() <= MAX_RECENT_ACTIONS);
        let restored = load_session(repository.path(), session.id.as_str()).expect("restored");
        assert!(!restored.completed);
    }
}
