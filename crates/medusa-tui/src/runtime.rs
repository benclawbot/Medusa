use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, TryLockError,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
};

use crate::app::{
    QuestionOption, QuestionPrompt, TranscriptPlan, TranscriptPlanStep, TranscriptPlanStepState,
};
use crate::clipboard::PromptDraft;
use crate::commands::{ModelConfiguration, SlashCommand};

pub use medusa_runtime::{
    RecoveryActionRequest, RecoveryOperation, RecoveryPreflightEvidence, RecoveryView,
    RuntimeActivity, RuntimeActivityKind, RuntimeError, SubmitDisposition, TeamSnapshot,
};

#[derive(Debug)]
pub enum RuntimeEvent {
    Started,
    AssistantText(String),
    Activity(RuntimeActivity),
    Team(TeamSnapshot),
    Plan(TranscriptPlan),
    Question(RuntimeQuestion),
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cache_read_input_tokens: u64,
        cache_creation_input_tokens: u64,
        total_tokens: u64,
        duration_ms: u64,
        tokens_per_second_milli: u64,
        estimated_cost_microusd: u64,
        provenance: String,
    },
    Progress {
        turn: u32,
    },
    Settings {
        model: String,
        effort: String,
        plan_mode: bool,
        credential_configured: bool,
        context_window_tokens: u64,
        auto_compact_percent: u8,
    },
    Notice {
        title: String,
        details: Vec<String>,
    },
    NewSession,
    Compacted {
        message: String,
    },
    Completed {
        session_id: String,
    },
    TurnFinished,
    Cancelled,
    Failed(String),
}

#[derive(Debug)]
pub struct RuntimeQuestion {
    pub questions: Vec<QuestionPrompt>,
}

pub struct RuntimeController {
    inner: Arc<Mutex<medusa_runtime::RuntimeController>>,
    deferred_events: Receiver<RuntimeEvent>,
    deferred_event_sender: Sender<RuntimeEvent>,
    submission_in_flight: Arc<AtomicBool>,
}

impl RuntimeController {
    pub fn start(repo: PathBuf) -> Self {
        Self::from_inner(medusa_runtime::RuntimeController::start(repo))
    }

    pub fn start_resumed(repo: PathBuf, session_id: &str) -> Result<Self, RuntimeError> {
        medusa_runtime::RuntimeController::start_resumed(repo, session_id).map(Self::from_inner)
    }

    pub fn start_continue_latest(repo: PathBuf) -> Result<Self, RuntimeError> {
        medusa_runtime::RuntimeController::start_continue_latest(repo).map(Self::from_inner)
    }

    fn from_inner(inner: medusa_runtime::RuntimeController) -> Self {
        let (deferred_event_sender, deferred_events) = mpsc::channel();
        Self {
            inner: Arc::new(Mutex::new(inner)),
            deferred_events,
            deferred_event_sender,
            submission_in_flight: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn submit(&self, draft: PromptDraft) -> Result<SubmitDisposition, RuntimeError> {
        let inner = Arc::clone(&self.inner);
        dispatch_submission(
            Arc::clone(&self.submission_in_flight),
            self.deferred_event_sender.clone(),
            move || lock(&inner).submit(draft),
        )
    }

    pub fn run_command(&self, command: SlashCommand) -> Result<(), RuntimeError> {
        if self.submission_in_flight.load(Ordering::Acquire) {
            return Err(RuntimeError::Busy);
        }
        try_lock(&self.inner)?.run_command(command)
    }

    pub fn configure_model(&self, configuration: ModelConfiguration) -> Result<(), RuntimeError> {
        if self.submission_in_flight.load(Ordering::Acquire) {
            return Err(RuntimeError::Busy);
        }
        try_lock(&self.inner)?.configure_model(configuration)
    }

    pub fn execute_recovery(
        &self,
        view: RecoveryView,
        request: RecoveryActionRequest,
        preflight: RecoveryPreflightEvidence,
    ) -> Result<(), RuntimeError> {
        if self.submission_in_flight.load(Ordering::Acquire) {
            return Err(RuntimeError::Busy);
        }
        try_lock(&self.inner)?.execute_recovery(view, request, preflight)
    }

    pub fn cancel(&self) -> bool {
        match self.inner.try_lock() {
            Ok(inner) => inner.cancel(),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().cancel(),
            Err(TryLockError::WouldBlock) => {
                let inner = Arc::clone(&self.inner);
                let _ = thread::Builder::new()
                    .name("medusa-tui-deferred-cancel".to_owned())
                    .spawn(move || {
                        lock(&inner).cancel();
                    });
                true
            }
        }
    }

    #[must_use]
    pub fn is_busy(&self) -> bool {
        if self.submission_in_flight.load(Ordering::Acquire) {
            return true;
        }
        match self.inner.try_lock() {
            Ok(inner) => inner.is_busy(),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().is_busy(),
            Err(TryLockError::WouldBlock) => true,
        }
    }

    pub fn try_event(&self) -> Result<Option<RuntimeEvent>, RuntimeError> {
        match self.deferred_events.try_recv() {
            Ok(event) => return Ok(Some(event)),
            Err(TryRecvError::Disconnected | TryRecvError::Empty) => {}
        }
        match self.inner.try_lock() {
            Ok(inner) => inner.try_event().map(|event| event.map(map_event)),
            Err(TryLockError::Poisoned(poisoned)) => poisoned
                .into_inner()
                .try_event()
                .map(|event| event.map(map_event)),
            Err(TryLockError::WouldBlock) => Ok(None),
        }
    }
}

fn dispatch_submission<F>(
    in_flight: Arc<AtomicBool>,
    events: Sender<RuntimeEvent>,
    operation: F,
) -> Result<SubmitDisposition, RuntimeError>
where
    F: FnOnce() -> Result<SubmitDisposition, RuntimeError> + Send + 'static,
{
    if in_flight.swap(true, Ordering::AcqRel) {
        return Err(RuntimeError::Busy);
    }
    let worker_flag = Arc::clone(&in_flight);
    let spawn = thread::Builder::new()
        .name("medusa-tui-submit".to_owned())
        .spawn(move || {
            match operation() {
                Ok(SubmitDisposition::Started) => {}
                Ok(SubmitDisposition::Queued) => {
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Follow-up queued".to_owned(),
                        details: vec![
                            "The prompt will run after the active agent turn.".to_owned(),
                        ],
                    });
                }
                Err(error) => {
                    let _ = events.send(RuntimeEvent::Failed(format!(
                        "submission rejected: {error}"
                    )));
                }
            }
            worker_flag.store(false, Ordering::Release);
        });
    if spawn.is_err() {
        in_flight.store(false, Ordering::Release);
        return Err(RuntimeError::WorkerStopped);
    }
    Ok(SubmitDisposition::Started)
}

fn try_lock(
    inner: &Arc<Mutex<medusa_runtime::RuntimeController>>,
) -> Result<std::sync::MutexGuard<'_, medusa_runtime::RuntimeController>, RuntimeError> {
    match inner.try_lock() {
        Ok(inner) => Ok(inner),
        Err(TryLockError::Poisoned(poisoned)) => Ok(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => Err(RuntimeError::Busy),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn map_event(event: medusa_runtime::RuntimeEvent) -> RuntimeEvent {
    match event {
        medusa_runtime::RuntimeEvent::RecoveryAvailable(view) => RuntimeEvent::Notice {
            title: "Recovery available".to_owned(),
            details: recovery_details(&view),
        },
        medusa_runtime::RuntimeEvent::RecoveryCompleted(receipt) => RuntimeEvent::Notice {
            title: "Recovery action recorded".to_owned(),
            details: vec![
                format!("Session: {}", receipt.record.session_id),
                format!("Action: {:?}", receipt.record.operation),
                format!("Outcome: {:?}", receipt.record.outcome),
                format!("Audit: {}", receipt.audit_path.display()),
            ],
        },
        medusa_runtime::RuntimeEvent::Started => RuntimeEvent::Started,
        medusa_runtime::RuntimeEvent::AssistantText(text) => RuntimeEvent::AssistantText(text),
        medusa_runtime::RuntimeEvent::Activity(activity) => {
            RuntimeEvent::Activity(presentation_activity(activity))
        }
        medusa_runtime::RuntimeEvent::Team(snapshot) => RuntimeEvent::Team(snapshot),
        medusa_runtime::RuntimeEvent::Plan(steps) => RuntimeEvent::Plan(TranscriptPlan {
            steps: steps
                .into_iter()
                .map(|step| TranscriptPlanStep {
                    title: step.title,
                    state: match step.status {
                        medusa_runtime::AgentPlanStepStatus::Pending => {
                            TranscriptPlanStepState::Pending
                        }
                        medusa_runtime::AgentPlanStepStatus::InProgress => {
                            TranscriptPlanStepState::Active
                        }
                        medusa_runtime::AgentPlanStepStatus::Completed => {
                            TranscriptPlanStepState::Completed
                        }
                        medusa_runtime::AgentPlanStepStatus::Failed => {
                            TranscriptPlanStepState::Failed
                        }
                    },
                })
                .collect(),
        }),
        medusa_runtime::RuntimeEvent::Question(question) => {
            RuntimeEvent::Question(RuntimeQuestion {
                questions: question
                    .prompts()
                    .iter()
                    .map(|item| QuestionPrompt {
                        header: item.header.clone(),
                        question: item.question.clone(),
                        options: item
                            .options
                            .iter()
                            .map(|option| QuestionOption {
                                label: option.label.clone(),
                                description: option.description.clone(),
                            })
                            .collect(),
                        multi_select: item.multi_select,
                    })
                    .collect(),
            })
        }
        medusa_runtime::RuntimeEvent::Usage {
            input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            total_tokens,
            duration_ms,
            tokens_per_second_milli,
            estimated_cost_microusd,
            provenance,
        } => RuntimeEvent::Usage {
            input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            total_tokens,
            duration_ms,
            tokens_per_second_milli,
            estimated_cost_microusd,
            provenance: match provenance {
                medusa_runtime::UsageProvenance::ProviderReported => "provider".to_owned(),
                medusa_runtime::UsageProvenance::Estimated => "estimated".to_owned(),
            },
        },
        medusa_runtime::RuntimeEvent::Progress { turn } => RuntimeEvent::Progress { turn },
        medusa_runtime::RuntimeEvent::Settings {
            model,
            effort,
            plan_mode,
            credential_configured,
            context_window_tokens,
            auto_compact_percent,
        } => RuntimeEvent::Settings {
            model,
            effort,
            plan_mode,
            credential_configured,
            context_window_tokens,
            auto_compact_percent,
        },
        medusa_runtime::RuntimeEvent::ConfigurationChanged(change) => RuntimeEvent::Notice {
            title: format!("Configuration revision {} applied", change.revision),
            details: vec![
                format!("Profile: {}", change.active_profile),
                format!("Changed: {}", change.changed_keys.join(", ")),
                format!("Origin: {:?}", change.origin),
                format!("Apply timing: {:?}", change.apply_timing),
            ],
        },
        medusa_runtime::RuntimeEvent::Notice { title, details }
            if title == "Runtime capabilities" =>
        {
            RuntimeEvent::Activity(RuntimeActivity {
                id: Some("runtime-capabilities".to_owned()),
                kind: RuntimeActivityKind::Done,
                title,
                details,
            })
        }
        medusa_runtime::RuntimeEvent::Notice { title, details } => {
            RuntimeEvent::Notice { title, details }
        }
        medusa_runtime::RuntimeEvent::NewSession => RuntimeEvent::NewSession,
        medusa_runtime::RuntimeEvent::Compacted { message } => RuntimeEvent::Compacted { message },
        medusa_runtime::RuntimeEvent::Completed { session_id } => {
            RuntimeEvent::Completed { session_id }
        }
        medusa_runtime::RuntimeEvent::TurnFinished => RuntimeEvent::TurnFinished,
        medusa_runtime::RuntimeEvent::Cancelled => RuntimeEvent::Cancelled,
        medusa_runtime::RuntimeEvent::Failed(error) => RuntimeEvent::Failed(error),
    }
}

fn recovery_details(view: &medusa_recovery_coordinator::RecoveryView) -> Vec<String> {
    let mut details = vec![
        format!("Session: {}", view.session_id),
        format!("Last durable step: {}", view.last_durable_step),
        format!("Health: {:?}", view.health),
        format!("Checkpoints: {}", view.checkpoints.len()),
    ];
    if let Some(operation) = &view.interrupted_operation {
        details.push(format!("Interrupted: {operation}"));
    }
    details.extend(view.warnings.iter().cloned());
    details
}

fn presentation_activity(mut activity: RuntimeActivity) -> RuntimeActivity {
    activity.details.retain(|detail| !detail.trim().is_empty());
    let (kind, label) = match activity.kind {
        RuntimeActivityKind::Assistant if !activity.details.is_empty() => {
            (RuntimeActivityKind::Progress, Some("Assistant"))
        }
        RuntimeActivityKind::Tool if !activity.details.is_empty() => (
            RuntimeActivityKind::Verification,
            Some(tool_activity_label(&activity.title)),
        ),
        _ => (activity.kind, None),
    };
    activity.kind = kind;
    if let Some(label) = label {
        activity.title = format!("{label} · {}", activity.title);
    }
    activity
}

fn tool_activity_label(title: &str) -> &'static str {
    let title = title.to_ascii_lowercase();
    if ["test", "check", "verify", "lint", "clippy"]
        .iter()
        .any(|keyword| title.contains(keyword))
    {
        "Test"
    } else if ["edit", "write", "patch", "update", "create", "delete"]
        .iter()
        .any(|keyword| title.contains(keyword))
    {
        "Edit"
    } else if ["run", "shell", "command", "build", "cargo", "npm"]
        .iter()
        .any(|keyword| title.contains(keyword))
    {
        "Run"
    } else if ["fetch", "download", "http", "web", "request"]
        .iter()
        .any(|keyword| title.contains(keyword))
    {
        "Fetch"
    } else if ["read", "search", "find", "list", "inspect", "open"]
        .iter()
        .any(|keyword| title.contains(keyword))
    {
        "Read"
    } else {
        "Tool"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn submission_dispatch_returns_before_backend_acceptance() {
        let in_flight = Arc::new(AtomicBool::new(false));
        let (event_tx, _event_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let started = Instant::now();

        let disposition = dispatch_submission(Arc::clone(&in_flight), event_tx, move || {
            release_rx.recv().expect("release backend");
            Ok(SubmitDisposition::Started)
        })
        .expect("dispatch submission");

        assert_eq!(disposition, SubmitDisposition::Started);
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(in_flight.load(Ordering::Acquire));
        release_tx.send(()).expect("release submission");
        for _ in 0..100 {
            if !in_flight.load(Ordering::Acquire) {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("submission worker did not complete");
    }

    #[test]
    fn submission_error_is_forwarded_without_blocking_dispatch() {
        let in_flight = Arc::new(AtomicBool::new(false));
        let (event_tx, event_rx) = mpsc::channel();
        dispatch_submission(Arc::clone(&in_flight), event_tx, || {
            Err(RuntimeError::WorkerStopped)
        })
        .expect("dispatch submission");

        let event = event_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("forwarded failure");
        assert!(matches!(
            event,
            RuntimeEvent::Failed(error) if error.contains("submission rejected")
        ));
    }
}
