use std::{
    collections::VecDeque,
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
use medusa_protocol::frontend::{
    FrontendEvent, FrontendEventEnvelope, FrontendKind, PresentationActivity,
    PresentationActivityKind, PresentationLifecycle,
};
use medusa_runtime::frontend::CanonicalFrontendEventStream;

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
    canonical: Mutex<CanonicalPresentation>,
    active_session_id: Mutex<Option<String>>,
    deferred_events: Receiver<RuntimeEvent>,
    deferred_event_sender: Sender<RuntimeEvent>,
    submission_in_flight: Arc<AtomicBool>,
}

struct CanonicalPresentation {
    repo: PathBuf,
    stream: CanonicalFrontendEventStream,
    session_id: Option<String>,
    pending: VecDeque<RuntimeEvent>,
    run_active: bool,
}

impl CanonicalPresentation {
    fn new(repo: PathBuf) -> Self {
        Self {
            stream: CanonicalFrontendEventStream::new(repo.clone(), FrontendKind::Tui),
            repo,
            session_id: None,
            pending: VecDeque::new(),
            run_active: false,
        }
    }

    fn reset(&mut self) {
        self.stream = CanonicalFrontendEventStream::new(self.repo.clone(), FrontendKind::Tui);
        self.session_id = None;
        self.pending.clear();
        self.run_active = false;
    }

    fn try_event(&mut self, session_id: &str) -> Result<Option<RuntimeEvent>, RuntimeError> {
        if self.session_id.as_deref() != Some(session_id) {
            self.session_id = Some(session_id.to_owned());
            self.pending.clear();
            self.run_active = false;
        }
        if let Some(event) = self.pending.pop_front() {
            return Ok(Some(event));
        }
        while let Some(envelope) = self.stream.try_event(session_id)? {
            let mut events = map_frontend_event(envelope, &mut self.run_active);
            if let Some(event) = events.pop_front() {
                self.pending.extend(events);
                return Ok(Some(event));
            }
        }
        Ok(None)
    }
}

impl RuntimeController {
    pub fn start(repo: PathBuf) -> Self {
        let inner = medusa_runtime::RuntimeController::start(repo.clone());
        Self::from_inner(repo, inner)
    }

    pub fn start_resumed(repo: PathBuf, session_id: &str) -> Result<Self, RuntimeError> {
        let inner = medusa_runtime::RuntimeController::start_resumed(repo.clone(), session_id)?;
        Ok(Self::from_inner(repo, inner))
    }

    pub fn start_continue_latest(repo: PathBuf) -> Result<Self, RuntimeError> {
        let inner = medusa_runtime::RuntimeController::start_continue_latest(repo.clone())?;
        Ok(Self::from_inner(repo, inner))
    }

    fn from_inner(repo: PathBuf, inner: medusa_runtime::RuntimeController) -> Self {
        let (deferred_event_sender, deferred_events) = mpsc::channel();
        Self {
            inner: Arc::new(Mutex::new(inner)),
            canonical: Mutex::new(CanonicalPresentation::new(repo)),
            active_session_id: Mutex::new(None),
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

        let (transient, observed_session_id) = match self.inner.try_lock() {
            Ok(inner) => (inner.try_event()?, inner.active_session_id()),
            Err(TryLockError::Poisoned(poisoned)) => {
                let inner = poisoned.into_inner();
                (inner.try_event()?, inner.active_session_id())
            }
            Err(TryLockError::WouldBlock) => (None, None),
        };

        let reset = matches!(
            transient.as_ref(),
            Some(medusa_runtime::RuntimeEvent::NewSession)
        );
        let session_id = if reset {
            *lock(&self.active_session_id) = None;
            lock(&self.canonical).reset();
            None
        } else if let Some(session_id) = observed_session_id {
            *lock(&self.active_session_id) = Some(session_id.clone());
            Some(session_id)
        } else {
            lock(&self.active_session_id).clone()
        };

        if let Some(event) =
            transient.and_then(|event| map_process_event(event, session_id.is_some()))
        {
            return Ok(Some(event));
        }
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        lock(&self.canonical).try_event(&session_id)
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
                Ok(SubmitDisposition::Queued) => {}
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

fn map_process_event(
    event: medusa_runtime::RuntimeEvent,
    session_bound: bool,
) -> Option<RuntimeEvent> {
    match event {
        medusa_runtime::RuntimeEvent::RecoveryAvailable(view) => Some(RuntimeEvent::Notice {
            title: "Recovery available".to_owned(),
            details: recovery_details(&view),
        }),
        medusa_runtime::RuntimeEvent::Settings {
            model,
            effort,
            plan_mode,
            credential_configured,
            context_window_tokens,
            auto_compact_percent,
        } => Some(RuntimeEvent::Settings {
            model,
            effort,
            plan_mode,
            credential_configured,
            context_window_tokens,
            auto_compact_percent,
        }),
        medusa_runtime::RuntimeEvent::ConfigurationChanged(change) => Some(RuntimeEvent::Notice {
            title: format!("Configuration revision {} applied", change.revision),
            details: vec![
                format!("Profile: {}", change.active_profile),
                format!("Changed: {}", change.changed_keys.join(", ")),
                format!("Origin: {:?}", change.origin),
                format!("Apply timing: {:?}", change.apply_timing),
            ],
        }),
        medusa_runtime::RuntimeEvent::Notice { title, details }
            if title == "Runtime capabilities" =>
        {
            Some(RuntimeEvent::Activity(RuntimeActivity {
                id: Some("runtime-capabilities".to_owned()),
                kind: RuntimeActivityKind::Done,
                title,
                details,
            }))
        }
        medusa_runtime::RuntimeEvent::Notice { title, details } => {
            Some(RuntimeEvent::Notice { title, details })
        }
        medusa_runtime::RuntimeEvent::NewSession => Some(RuntimeEvent::NewSession),
        medusa_runtime::RuntimeEvent::Progress { turn } => Some(RuntimeEvent::Progress { turn }),
        medusa_runtime::RuntimeEvent::Cancelled if !session_bound => Some(RuntimeEvent::Cancelled),
        medusa_runtime::RuntimeEvent::Failed(error) if !session_bound => {
            Some(RuntimeEvent::Failed(error))
        }
        medusa_runtime::RuntimeEvent::RecoveryCompleted(_)
        | medusa_runtime::RuntimeEvent::Started
        | medusa_runtime::RuntimeEvent::AssistantText(_)
        | medusa_runtime::RuntimeEvent::Activity(_)
        | medusa_runtime::RuntimeEvent::Team(_)
        | medusa_runtime::RuntimeEvent::Plan(_)
        | medusa_runtime::RuntimeEvent::Question(_)
        | medusa_runtime::RuntimeEvent::Usage { .. }
        | medusa_runtime::RuntimeEvent::Compacted { .. }
        | medusa_runtime::RuntimeEvent::Completed { .. }
        | medusa_runtime::RuntimeEvent::TurnFinished
        | medusa_runtime::RuntimeEvent::Cancelled
        | medusa_runtime::RuntimeEvent::Failed(_) => None,
    }
}

fn map_frontend_event(
    envelope: FrontendEventEnvelope,
    run_active: &mut bool,
) -> VecDeque<RuntimeEvent> {
    let FrontendEventEnvelope {
        session_id, event, ..
    } = envelope;
    let mut events = VecDeque::new();
    match event {
        FrontendEvent::SubmissionAccepted | FrontendEvent::Started => {
            if let Some(event) = canonical_start_event(run_active) {
                events.push_back(event);
            }
        }
        FrontendEvent::SubmissionQueued { position } => {
            events.push_back(RuntimeEvent::Notice {
                title: "Follow-up queued".to_owned(),
                details: vec![format!("Queue position: {position}")],
            });
        }
        FrontendEvent::AssistantTextDelta { text } | FrontendEvent::AssistantInterim { text } => {
            events.push_back(RuntimeEvent::AssistantText(text));
        }
        FrontendEvent::Activity(activity) => {
            events.push_back(RuntimeEvent::Activity(map_presentation_activity(activity)));
        }
        FrontendEvent::Team {
            workers,
            verification,
        } => {
            for worker in workers {
                let mut details = vec![format!("role {}", worker.role)];
                if let Some(action) = worker.current_action {
                    details.push(action);
                }
                events.push_back(RuntimeEvent::Activity(RuntimeActivity {
                    id: Some(format!("team:{}", worker.worker_id)),
                    kind: runtime_activity_kind(PresentationActivityKind::Worker, worker.lifecycle),
                    title: format!("{} · {}", worker.worker_id, worker.task),
                    details,
                }));
            }
            if let Some(verification) = verification {
                events.push_back(RuntimeEvent::Activity(RuntimeActivity {
                    id: Some("team-verification".to_owned()),
                    kind: RuntimeActivityKind::Verification,
                    title: "Team verification".to_owned(),
                    details: vec![verification],
                }));
            }
        }
        FrontendEvent::Plan { steps, .. } => {
            events.push_back(RuntimeEvent::Plan(TranscriptPlan {
                steps: steps
                    .into_iter()
                    .map(|step| TranscriptPlanStep {
                        title: step.title,
                        state: match step.lifecycle {
                            PresentationLifecycle::Active => TranscriptPlanStepState::Active,
                            PresentationLifecycle::Succeeded => TranscriptPlanStepState::Completed,
                            PresentationLifecycle::Failed | PresentationLifecycle::Cancelled => {
                                TranscriptPlanStepState::Failed
                            }
                            PresentationLifecycle::Waiting
                            | PresentationLifecycle::Informational => {
                                TranscriptPlanStepState::Pending
                            }
                        },
                    })
                    .collect(),
            }));
        }
        FrontendEvent::Question(question) => {
            events.push_back(RuntimeEvent::Question(RuntimeQuestion {
                questions: vec![QuestionPrompt {
                    header: "Question".to_owned(),
                    question: question.prompt,
                    options: question
                        .options
                        .into_iter()
                        .map(|option| QuestionOption {
                            description: if option.value != option.label {
                                option.value
                            } else {
                                String::new()
                            },
                            label: option.label,
                        })
                        .collect(),
                    multi_select: false,
                }],
            }));
        }
        FrontendEvent::ApprovalRequired(approval) => {
            events.push_back(RuntimeEvent::Question(RuntimeQuestion {
                questions: vec![QuestionPrompt {
                    header: "Approval".to_owned(),
                    question: format!(
                        "{} in {}: {} (risk: {})",
                        approval.action, approval.scope, approval.reason, approval.risk
                    ),
                    options: vec![
                        QuestionOption {
                            label: "Approve".to_owned(),
                            description: "Allow this action once".to_owned(),
                        },
                        QuestionOption {
                            label: "Deny".to_owned(),
                            description: "Do not perform this action".to_owned(),
                        },
                    ],
                    multi_select: false,
                }],
            }));
        }
        FrontendEvent::Usage {
            input_tokens,
            output_tokens,
            total_tokens,
            estimated_cost_microusd,
        } => {
            events.push_back(RuntimeEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                total_tokens,
                duration_ms: 0,
                tokens_per_second_milli: 0,
                estimated_cost_microusd,
                provenance: "canonical-journal".to_owned(),
            });
        }
        FrontendEvent::Progress { turn, .. } => {
            events.push_back(RuntimeEvent::Progress { turn });
        }
        FrontendEvent::SettingsChanged { .. } => {}
        FrontendEvent::Notice {
            severity,
            title,
            mut details,
        } => {
            if severity != "info" {
                details.insert(0, format!("Severity: {severity}"));
            }
            if title == "Conversation compacted" {
                events.push_back(RuntimeEvent::Compacted {
                    message: details.join(" · "),
                });
            } else {
                events.push_back(RuntimeEvent::Notice { title, details });
            }
        }
        FrontendEvent::Artifact(artifact) => {
            let mut details = vec![
                format!("Media type: {}", artifact.media_type),
                format!("Evidence: {}", artifact.evidence_ref),
            ];
            if let Some(caption) = artifact.caption {
                details.push(caption);
            }
            events.push_back(RuntimeEvent::Activity(RuntimeActivity {
                id: Some(artifact.artifact_id),
                kind: RuntimeActivityKind::Done,
                title: format!("Artifact available: {}", artifact.name),
                details,
            }));
        }
        FrontendEvent::TurnFinished => {
            *run_active = false;
            events.push_back(RuntimeEvent::TurnFinished);
        }
        FrontendEvent::Completed { summary } => {
            *run_active = false;
            if let Some(summary) = summary {
                events.push_back(RuntimeEvent::Notice {
                    title: "Completion report".to_owned(),
                    details: vec![summary],
                });
            }
            events.push_back(RuntimeEvent::Completed { session_id });
        }
        FrontendEvent::Cancelled { reason } => {
            *run_active = false;
            if let Some(reason) = reason {
                events.push_back(RuntimeEvent::Notice {
                    title: "Cancellation reason".to_owned(),
                    details: vec![reason],
                });
            }
            events.push_back(RuntimeEvent::Cancelled);
        }
        FrontendEvent::Failed { message, recovery } => {
            *run_active = false;
            let message = if recovery.is_empty() {
                message
            } else {
                format!("{message}\nRecovery: {}", recovery.join("; "))
            };
            events.push_back(RuntimeEvent::Failed(message));
        }
    }
    events
}

fn canonical_start_event(run_active: &mut bool) -> Option<RuntimeEvent> {
    if *run_active {
        None
    } else {
        *run_active = true;
        Some(RuntimeEvent::Started)
    }
}

fn map_presentation_activity(activity: PresentationActivity) -> RuntimeActivity {
    let mut details = activity.details;
    if !activity.affected_paths.is_empty() {
        details.push(format!("Paths: {}", activity.affected_paths.join(", ")));
    }
    if let Some(evidence) = activity.evidence_ref {
        details.push(format!("Evidence: {evidence}"));
    }
    RuntimeActivity {
        id: Some(activity.activity_id),
        kind: runtime_activity_kind(activity.kind, activity.lifecycle),
        title: activity.title,
        details,
    }
}

fn runtime_activity_kind(
    kind: PresentationActivityKind,
    lifecycle: PresentationLifecycle,
) -> RuntimeActivityKind {
    match lifecycle {
        PresentationLifecycle::Failed | PresentationLifecycle::Cancelled => {
            RuntimeActivityKind::Error
        }
        PresentationLifecycle::Succeeded => match kind {
            PresentationActivityKind::Assistant => RuntimeActivityKind::Assistant,
            PresentationActivityKind::Verification | PresentationActivityKind::Test => {
                RuntimeActivityKind::Verification
            }
            PresentationActivityKind::Error => RuntimeActivityKind::Error,
            _ => RuntimeActivityKind::Done,
        },
        PresentationLifecycle::Active
        | PresentationLifecycle::Waiting
        | PresentationLifecycle::Informational => match kind {
            PresentationActivityKind::Assistant => RuntimeActivityKind::Assistant,
            PresentationActivityKind::RepositoryRead
            | PresentationActivityKind::Edit
            | PresentationActivityKind::Command => RuntimeActivityKind::Tool,
            PresentationActivityKind::Verification | PresentationActivityKind::Test => {
                RuntimeActivityKind::Verification
            }
            PresentationActivityKind::Done => RuntimeActivityKind::Done,
            PresentationActivityKind::Error => RuntimeActivityKind::Error,
            _ => RuntimeActivityKind::Progress,
        },
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
    fn canonical_start_is_emitted_once_until_terminal_state() {
        let mut run_active = false;
        assert!(matches!(
            canonical_start_event(&mut run_active),
            Some(RuntimeEvent::Started)
        ));
        assert!(canonical_start_event(&mut run_active).is_none());
        run_active = false;
        assert!(matches!(
            canonical_start_event(&mut run_active),
            Some(RuntimeEvent::Started)
        ));
    }

    #[test]
    fn session_bound_process_terminal_state_is_suppressed() {
        assert!(map_process_event(medusa_runtime::RuntimeEvent::TurnFinished, true).is_none());
        assert!(
            map_process_event(
                medusa_runtime::RuntimeEvent::Failed("durable failure".to_owned()),
                true,
            )
            .is_none()
        );
    }

    #[test]
    fn pre_session_failure_remains_visible() {
        assert!(matches!(
            map_process_event(
                medusa_runtime::RuntimeEvent::Failed("startup failed".to_owned()),
                false,
            ),
            Some(RuntimeEvent::Failed(error)) if error == "startup failed"
        ));
    }

    #[test]
    fn process_turn_projection_remains_a_compatibility_hint() {
        assert!(matches!(
            map_process_event(medusa_runtime::RuntimeEvent::Progress { turn: 7 }, true),
            Some(RuntimeEvent::Progress { turn: 7 })
        ));
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
