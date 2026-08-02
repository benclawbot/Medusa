use std::{
    collections::{BTreeMap, VecDeque},
    env,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
};

use medusa_agent::{
    AgentEngine, AgentPlanStep, AgentQuestion, AgentSession, StepOutcome, TurnUsage,
    compact_session, update_session_objective,
};
use medusa_capabilities::CapabilityRegistry;
use medusa_config::{Config, ConfigurationChanged, Mode};
use medusa_protocol::{Actor, EventPayload};
use medusa_provider::{ConfiguredProvider, ModelProvider};

use crate::{
    commands::{
        Effort, LearningCommand, ModelCommand, ModelConfiguration, ReviewCommand, SlashCommand,
    },
    prompt::PromptDraft,
};

pub mod attachment;
pub mod checkpoint_payload;
pub mod checkpoint_store;
pub mod commands;
mod config_command;
mod error;
pub mod execution_history;
mod learning_retrieval;
pub mod learning_review;
mod multi_agent_coordinator;
mod mutating_worker_coordinator;
mod mutation_transaction;
pub mod openai_realtime;
pub mod prompt;
pub mod review;
pub mod skill_dependencies;
pub mod skill_dependency_locks;
mod support;
mod team_control;
#[cfg(test)]
mod tests;
pub mod voice;
pub mod voice_agent_bridge;

pub use checkpoint_payload::{CheckpointFilePayload, RuntimeCheckpointPayload};
pub use checkpoint_store::RuntimeCheckpointRecord;
pub use error::RuntimeError;
pub use execution_history::{
    RuntimeContinuityHealth, RuntimeExecutionHealth, RuntimeHistoricalState,
};
pub use medusa_agent::{
    AgentPlanStep as RuntimePlanStep, AgentPlanStepStatus, AgentQuestionItem, AgentQuestionOption,
    UsageProvenance,
};
pub use team_control::{
    TeamControlPlane, TeamSnapshot, TeamWorkerLifecycle, TeamWorkerRegistration, TeamWorkerSnapshot,
};

use support::{
    SUPPORTED_PROVIDERS, SelectedSkill, UpdateState, configure_model, credential_environment,
    discover_skills, effort_for_turns, forward_update, is_supported_provider, load_selected_skill,
    message_blocks, model_configuration_details, objective_for, protocol_for_provider,
    should_auto_compact, turns_for_effort,
};

#[derive(Debug)]
pub enum RuntimeCommand {
    Submit {
        draft: PromptDraft,
        accepted: Sender<()>,
    },
    Slash(SlashCommand),
    ConfigureModel(ModelConfiguration),
    Recovery {
        view: Box<medusa_recovery_coordinator::RecoveryView>,
        request: medusa_recovery_coordinator::RecoveryActionRequest,
        preflight: medusa_recovery_coordinator::RecoveryPreflightEvidence,
    },
    Shutdown,
}

#[derive(Debug)]
pub enum RuntimeEvent {
    RecoveryAvailable(medusa_recovery_coordinator::RecoveryView),
    RecoveryCompleted(medusa_recovery_coordinator::RecoveryExecutionReceipt),
    Started,
    AssistantText(String),
    Activity(RuntimeActivity),
    Team(TeamSnapshot),
    Plan(Vec<AgentPlanStep>),
    Question(AgentQuestion),
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cache_read_input_tokens: u64,
        cache_creation_input_tokens: u64,
        total_tokens: u64,
        duration_ms: u64,
        tokens_per_second_milli: u64,
        estimated_cost_microusd: u64,
        provenance: UsageProvenance,
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
    ConfigurationChanged(ConfigurationChanged),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeEventDurability {
    CanonicalJournal(&'static str),
    SessionBoundCanonical {
        journal_source: &'static str,
        pre_session_classification: &'static str,
    },
    DurableProjection(&'static str),
    PresentationOnly(&'static str),
}

impl RuntimeEvent {
    #[must_use]
    pub fn durability(&self) -> RuntimeEventDurability {
        match self {
            Self::RecoveryAvailable(_) => {
                RuntimeEventDurability::DurableProjection("recovery coordinator record")
            }
            Self::RecoveryCompleted(_) => {
                RuntimeEventDurability::CanonicalJournal("recovery_action_completed")
            }
            Self::Started => RuntimeEventDurability::PresentationOnly("frontend busy indicator"),
            Self::AssistantText(_) => {
                RuntimeEventDurability::CanonicalJournal("assistant_message_recorded")
            }
            Self::Activity(_) => RuntimeEventDurability::PresentationOnly(
                "projection of model, tool, verification, or worker events",
            ),
            Self::Team(_) => RuntimeEventDurability::CanonicalJournal("team_state_changed"),
            Self::Plan(_) => RuntimeEventDurability::CanonicalJournal("plan_updated"),
            Self::Question(_) => RuntimeEventDurability::CanonicalJournal("question_requested"),
            Self::Usage { .. } => {
                RuntimeEventDurability::CanonicalJournal("model_response_received")
            }
            Self::Progress { .. } => {
                RuntimeEventDurability::DurableProjection("materialized session turn")
            }
            Self::Settings { .. } => {
                RuntimeEventDurability::PresentationOnly("process-local frontend settings")
            }
            Self::ConfigurationChanged(_) => {
                RuntimeEventDurability::DurableProjection("configuration-state.toml")
            }
            Self::Notice { .. } => {
                RuntimeEventDurability::PresentationOnly("human-readable presentation notice")
            }
            Self::NewSession => RuntimeEventDurability::PresentationOnly(
                "frontend transcript reset after controller state mutation",
            ),
            Self::Compacted { .. } => {
                RuntimeEventDurability::CanonicalJournal("conversation_compacted")
            }
            Self::Completed { .. } => RuntimeEventDurability::CanonicalJournal("session_completed"),
            Self::TurnFinished => RuntimeEventDurability::CanonicalJournal("runtime_turn_finished"),
            Self::Cancelled => RuntimeEventDurability::SessionBoundCanonical {
                journal_source: "cancellation_completed",
                pre_session_classification: "cancelled before a session identity existed",
            },
            Self::Failed(_) => RuntimeEventDurability::SessionBoundCanonical {
                journal_source: "runtime_failed",
                pre_session_classification: "startup failure before a session identity existed",
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeActivityKind {
    Assistant,
    Done,
    Error,
    Progress,
    Tool,
    Verification,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeActivity {
    pub id: Option<String>,
    pub kind: RuntimeActivityKind,
    pub title: String,
    pub details: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitDisposition {
    Started,
    Queued,
}

#[derive(Clone, Debug)]
struct QueuedFollowup {
    command_id: String,
    draft: PromptDraft,
    durably_recorded: bool,
}

#[derive(Default)]
struct SubmissionState {
    busy: bool,
    followups: VecDeque<QueuedFollowup>,
    active_session_id: Option<String>,
}

fn restore_queued_followups(
    session: &AgentSession,
) -> Result<VecDeque<QueuedFollowup>, RuntimeError> {
    let mut followups = VecDeque::new();
    for event in &session.events {
        match &event.payload {
            EventPayload::UserFollowupQueued { command_id, prompt } => {
                let draft = serde_json::from_value::<PromptDraft>(prompt.clone())
                    .map_err(RuntimeError::agent)?;
                followups
                    .retain(|queued: &QueuedFollowup| queued.command_id != command_id.as_str());
                followups.push_back(QueuedFollowup {
                    command_id: command_id.clone(),
                    draft,
                    durably_recorded: true,
                });
            }
            EventPayload::UserFollowupDequeued { command_id, .. } => {
                followups.retain(|queued| queued.command_id != command_id.as_str());
            }
            EventPayload::CancellationCompleted
            | EventPayload::RuntimeFailed { .. }
            | EventPayload::SessionReset { .. }
            | EventPayload::SessionCompleted { .. } => followups.clear(),
            _ => {}
        }
    }
    Ok(followups)
}

static FOLLOWUP_COMMAND_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub struct RuntimeController {
    commands: Sender<RuntimeCommand>,
    events: Receiver<RuntimeEvent>,
    cancel: Arc<AtomicBool>,
    submission: Arc<Mutex<SubmissionState>>,
    event_sender: Sender<RuntimeEvent>,
    team_control: TeamControlPlane,
    repo: PathBuf,
}

impl RuntimeController {
    pub fn start(repo: PathBuf) -> Self {
        match RuntimeState::load(repo.clone()) {
            Ok(state) => Self::start_with_state(state),
            Err(error) => Self::failed_start(error),
        }
    }

    pub fn start_with_config(repo: PathBuf, config: Config) -> Self {
        Self::start_with_state(RuntimeState::from_config(repo, config))
    }

    fn start_with_state(state: RuntimeState) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let (runtime_event_tx, runtime_event_rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let submission = Arc::new(Mutex::new(SubmissionState::default()));
        let worker_cancel = Arc::clone(&cancel);
        let worker_submission = Arc::clone(&submission);
        let worker_events = runtime_event_tx.clone();
        let team_control = state.team_control.clone();
        let state_repo = state.repo.clone();
        let dispatch_repo = state_repo.clone();
        let dispatch_submission = Arc::clone(&submission);
        let dispatch_events = event_tx.clone();
        if let Err(error) = thread::Builder::new()
            .name("medusa-runtime-events".to_owned())
            .spawn(move || {
                dispatch_runtime_events(
                    &dispatch_repo,
                    &dispatch_submission,
                    runtime_event_rx,
                    &dispatch_events,
                );
            })
        {
            let _ = event_tx.send(RuntimeEvent::Failed(format!(
                "failed to spawn durable runtime event dispatcher: {error}"
            )));
        }
        if let Err(error) = thread::Builder::new()
            .name("medusa-runtime".to_owned())
            .spawn(move || {
                worker_loop_with_state(
                    state,
                    command_rx,
                    worker_events,
                    worker_cancel,
                    worker_submission,
                );
            })
        {
            let _ = event_tx.send(RuntimeEvent::Failed(format!(
                "failed to spawn agent runtime worker: {error}"
            )));
        }
        Self {
            commands: command_tx,
            events: event_rx,
            cancel,
            submission,
            event_sender: runtime_event_tx,
            team_control,
            repo: state_repo,
        }
    }

    fn failed_start(error: RuntimeError) -> Self {
        let (command_tx, _command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let _ = event_tx.send(RuntimeEvent::Failed(error.to_string()));
        Self {
            commands: command_tx,
            events: event_rx,
            cancel: Arc::new(AtomicBool::new(false)),
            submission: Arc::new(Mutex::new(SubmissionState::default())),
            event_sender: event_tx,
            team_control: TeamControlPlane::default(),
            repo: PathBuf::new(),
        }
    }

    pub fn submit(&self, draft: PromptDraft) -> Result<SubmitDisposition, RuntimeError> {
        let mut submission = lock_submission(&self.submission);
        if submission.busy {
            let session_id = submission
                .active_session_id
                .clone()
                .ok_or(RuntimeError::Busy)?;
            let command_id = next_followup_command_id();
            let queued = QueuedFollowup {
                command_id: command_id.clone(),
                draft,
                durably_recorded: true,
            };
            record_controller_event(
                &self.repo,
                &session_id,
                Actor::User,
                EventPayload::UserFollowupQueued {
                    command_id,
                    prompt: serde_json::to_value(&queued.draft).map_err(RuntimeError::agent)?,
                },
            )?;
            submission.followups.push_back(queued);
            return Ok(SubmitDisposition::Queued);
        }
        submission.busy = true;
        drop(submission);
        self.cancel.store(false, Ordering::SeqCst);
        let (accepted_tx, accepted_rx) = mpsc::channel();
        if self
            .commands
            .send(RuntimeCommand::Submit {
                draft,
                accepted: accepted_tx,
            })
            .is_err()
        {
            mark_idle(&self.submission, true);
            return Err(RuntimeError::WorkerStopped);
        }
        if accepted_rx.recv().is_err() {
            mark_idle(&self.submission, true);
            return Err(RuntimeError::agent(
                "runtime prompt ended before a durable session accepted the submission",
            ));
        }
        Ok(SubmitDisposition::Started)
    }

    pub fn run_command(&self, command: SlashCommand) -> Result<(), RuntimeError> {
        if let SlashCommand::Team(command) = &command {
            let snapshot = match command {
                crate::commands::TeamCommand::Show => self.team_control.snapshot(),
                crate::commands::TeamCommand::Steer {
                    worker_id,
                    instruction,
                } => self
                    .team_control
                    .steer(worker_id, instruction)
                    .map_err(RuntimeError::agent)?,
                crate::commands::TeamCommand::StopWorker { worker_id } => self
                    .team_control
                    .stop_worker(worker_id)
                    .map_err(RuntimeError::agent)?,
                crate::commands::TeamCommand::StopTeam => {
                    self.team_control.stop_team().map_err(RuntimeError::agent)?
                }
            };
            let _ = self.event_sender.send(RuntimeEvent::Team(snapshot));
            return Ok(());
        }
        let mut submission = lock_submission(&self.submission);
        if submission.busy {
            return Err(RuntimeError::Busy);
        }
        if command.runs_agent() {
            submission.busy = true;
            self.cancel.store(false, Ordering::SeqCst);
        }
        if self.commands.send(RuntimeCommand::Slash(command)).is_err() {
            submission.busy = false;
            return Err(RuntimeError::WorkerStopped);
        }
        Ok(())
    }

    pub fn configure_model(&self, configuration: ModelConfiguration) -> Result<(), RuntimeError> {
        if lock_submission(&self.submission).busy {
            return Err(RuntimeError::Busy);
        }
        self.commands
            .send(RuntimeCommand::ConfigureModel(configuration))
            .map_err(|_| RuntimeError::WorkerStopped)
    }

    pub fn execute_recovery(
        &self,
        view: medusa_recovery_coordinator::RecoveryView,
        request: medusa_recovery_coordinator::RecoveryActionRequest,
        preflight: medusa_recovery_coordinator::RecoveryPreflightEvidence,
    ) -> Result<(), RuntimeError> {
        if lock_submission(&self.submission).busy {
            return Err(RuntimeError::Busy);
        }
        let (view, preflight) = if matches!(
            request.operation,
            medusa_recovery_coordinator::RecoveryOperation::RestoreCheckpoint
        ) {
            let checkpoint_id = request.checkpoint_id.as_deref().ok_or_else(|| {
                RuntimeError::InvalidCommand("restore requires a checkpoint id".to_owned())
            })?;
            self.preview_checkpoint_restore(&request.session_id, checkpoint_id)?;
            let (authoritative_view, authoritative_preflight) =
                recovery_action_context(&self.repo, &request).map_err(RuntimeError::agent)?;
            if preflight != authoritative_preflight {
                return Err(RuntimeError::InvalidCommand(
                    "recovery preflight is stale; refresh the checkpoint preview".to_owned(),
                ));
            }
            let source_cursor = self.execution_health(&request.session_id)?.journal_cursor;
            record_controller_event(
                &self.repo,
                &request.session_id,
                Actor::User,
                EventPayload::CheckpointRestoreRequested {
                    checkpoint_id: checkpoint_id.to_owned(),
                    source_cursor,
                },
            )?;
            (authoritative_view, authoritative_preflight)
        } else {
            (view, preflight)
        };
        self.commands
            .send(RuntimeCommand::Recovery {
                view: Box::new(view),
                request,
                preflight,
            })
            .map_err(|_| RuntimeError::WorkerStopped)
    }

    pub fn cancel(&self) -> bool {
        let submission = lock_submission(&self.submission);
        if !submission.busy {
            return false;
        }
        if let Some(session_id) = submission.active_session_id.as_deref() {
            if let Err(error) = record_controller_event(
                &self.repo,
                session_id,
                Actor::User,
                EventPayload::CancellationRequested {
                    source: "frontend".to_owned(),
                },
            ) {
                let _ = self.event_sender.send(RuntimeEvent::Failed(format!(
                    "cancellation was not requested because its durable record failed: {error}"
                )));
                return false;
            }
        }
        self.cancel.store(true, Ordering::SeqCst);
        true
    }

    #[must_use]
    pub fn is_busy(&self) -> bool {
        lock_submission(&self.submission).busy
    }

    /// Returns the durable session identity after a submission has been accepted.
    #[must_use]
    pub fn active_session_id(&self) -> Option<String> {
        lock_submission(&self.submission).active_session_id.clone()
    }

    pub fn try_event(&self) -> Result<Option<RuntimeEvent>, RuntimeError> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(RuntimeError::WorkerStopped),
        }
    }
}

fn dispatch_runtime_events(
    repo: &std::path::Path,
    submission: &Arc<Mutex<SubmissionState>>,
    runtime_events: Receiver<RuntimeEvent>,
    frontend_events: &Sender<RuntimeEvent>,
) {
    while let Ok(event) = runtime_events.recv() {
        let payload = match controller_event_payload(&event) {
            Ok(payload) => payload,
            Err(error) => {
                let _ = frontend_events.send(RuntimeEvent::Failed(format!(
                    "runtime event was not published because durable serialization failed: {error}"
                )));
                continue;
            }
        };
        if let Some(payload) = payload {
            let session_id = match &event {
                RuntimeEvent::RecoveryCompleted(receipt) => Some(receipt.record.session_id.clone()),
                _ => lock_submission(submission).active_session_id.clone(),
            };
            if let Some(session_id) = session_id {
                if let Err(error) =
                    record_controller_event(repo, &session_id, Actor::Coordinator, payload)
                {
                    let _ = frontend_events.send(RuntimeEvent::Failed(format!(
                        "runtime event was not published because its durable record failed: {error}"
                    )));
                    continue;
                }
            } else if !matches!(
                event.durability(),
                RuntimeEventDurability::SessionBoundCanonical { .. }
            ) {
                let _ = frontend_events.send(RuntimeEvent::Failed(
                    "runtime event was not published because no durable session identity was available"
                        .to_owned(),
                ));
                continue;
            }
        }
        let _ = frontend_events.send(event);
    }
}

fn controller_event_payload(event: &RuntimeEvent) -> Result<Option<EventPayload>, RuntimeError> {
    let payload = match event {
        RuntimeEvent::RecoveryCompleted(receipt) => Some(EventPayload::RecoveryActionCompleted {
            receipt: serde_json::json!({
                "record": &receipt.record,
                "audit_path": receipt.audit_path.display().to_string(),
            }),
        }),
        RuntimeEvent::Team(snapshot) => Some(EventPayload::TeamStateChanged {
            snapshot: serde_json::to_value(snapshot).map_err(RuntimeError::agent)?,
        }),
        RuntimeEvent::TurnFinished => Some(EventPayload::RuntimeTurnFinished),
        RuntimeEvent::Cancelled => Some(EventPayload::CancellationCompleted),
        RuntimeEvent::Failed(message) => Some(EventPayload::RuntimeFailed {
            message: message.clone(),
        }),
        RuntimeEvent::RecoveryAvailable(_)
        | RuntimeEvent::Started
        | RuntimeEvent::AssistantText(_)
        | RuntimeEvent::Activity(_)
        | RuntimeEvent::Plan(_)
        | RuntimeEvent::Question(_)
        | RuntimeEvent::Usage { .. }
        | RuntimeEvent::Progress { .. }
        | RuntimeEvent::Settings { .. }
        | RuntimeEvent::ConfigurationChanged(_)
        | RuntimeEvent::Notice { .. }
        | RuntimeEvent::NewSession
        | RuntimeEvent::Compacted { .. }
        | RuntimeEvent::Completed { .. } => None,
    };
    Ok(payload)
}

fn next_followup_command_id() -> String {
    let sequence = FOLLOWUP_COMMAND_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!(
        "followup-{}-{sequence}",
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    )
}

fn record_controller_event(
    repo: &std::path::Path,
    session_id: &str,
    actor: Actor,
    payload: EventPayload,
) -> Result<(), RuntimeError> {
    let checkpoint_boundary = crate::checkpoint_store::is_checkpoint_boundary(&payload);
    let mut session = medusa_agent::session_browser::load_session(repo, session_id)
        .map_err(RuntimeError::agent)?;
    medusa_agent::record_session_event(&mut session, actor, payload)
        .map_err(RuntimeError::agent)?;
    if checkpoint_boundary {
        let checkpoint = crate::checkpoint_store::materialize(repo, session_id)?;
        let checkpoint_id = checkpoint.checkpoint.fingerprint;
        crate::recovery_projection::refresh(repo, session_id)?;
        let mut session = medusa_agent::session_browser::load_session(repo, session_id)
            .map_err(RuntimeError::agent)?;
        let already_recorded = session.events.last().is_some_and(|event| {
            matches!(
                &event.payload,
                EventPayload::CheckpointCreated {
                    checkpoint_id: existing,
                } if existing == &checkpoint_id
            )
        });
        if !already_recorded {
            medusa_agent::record_session_event(
                &mut session,
                Actor::Coordinator,
                EventPayload::CheckpointCreated { checkpoint_id },
            )
            .map_err(RuntimeError::agent)?;
        }
    }
    Ok(())
}

fn lock_submission(
    submission: &Mutex<SubmissionState>,
) -> std::sync::MutexGuard<'_, SubmissionState> {
    match submission.lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    }
}

impl Drop for RuntimeController {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        let _ = self.commands.send(RuntimeCommand::Shutdown);
    }
}

fn worker_loop_with_state(
    state: RuntimeState,
    commands: Receiver<RuntimeCommand>,
    events: Sender<RuntimeEvent>,
    cancel: Arc<AtomicBool>,
    submission: Arc<Mutex<SubmissionState>>,
) {
    worker_loop_with_discovery(
        state,
        commands,
        events,
        cancel,
        submission,
        capability_event,
    );
}

fn worker_loop_with_discovery<F>(
    mut state: RuntimeState,
    commands: Receiver<RuntimeCommand>,
    events: Sender<RuntimeEvent>,
    cancel: Arc<AtomicBool>,
    submission: Arc<Mutex<SubmissionState>>,
    discover: F,
) where
    F: FnOnce(PathBuf) -> RuntimeEvent + Send + 'static,
{
    let _ = events.send(state.settings_event());
    for recovery_event in recovery::startup_events(&state.repo) {
        let _ = events.send(recovery_event);
    }
    let capability_repo = state.repo.clone();
    let capability_events = events.clone();
    if let Err(error) = thread::Builder::new()
        .name("medusa-capability-discovery".to_owned())
        .spawn(move || {
            let _ = capability_events.send(discover(capability_repo));
        })
    {
        let _ = events.send(RuntimeEvent::Notice {
            title: "Runtime capabilities unavailable".to_owned(),
            details: vec![format!("failed to start capability discovery: {error}")],
        });
    }
    while let Ok(command) = commands.recv() {
        match command {
            RuntimeCommand::Submit { draft, accepted } => {
                let _ = events.send(RuntimeEvent::Started);
                let outcome = run_prompt(
                    &mut state,
                    draft,
                    &events,
                    &cancel,
                    &submission,
                    Some(accepted),
                );
                let event = match outcome {
                    Ok(completed) => completed,
                    Err(error) => {
                        mark_idle(&submission, true);
                        RuntimeEvent::Failed(error.to_string())
                    }
                };
                let _ = events.send(event);
            }
            RuntimeCommand::Slash(command) => {
                let runs_agent = command.runs_agent();
                if runs_agent {
                    let _ = events.send(RuntimeEvent::Started);
                }
                match execute_slash_command_with_submission(
                    &mut state,
                    command,
                    &events,
                    &cancel,
                    &submission,
                ) {
                    Ok(Some(event)) => {
                        if !runs_agent {
                            mark_idle(&submission, false);
                        }
                        let _ = events.send(event);
                    }
                    Ok(None) => {
                        if runs_agent {
                            mark_idle(&submission, false);
                        }
                    }
                    Err(error) => {
                        if runs_agent {
                            mark_idle(&submission, true);
                        }
                        let event = if runs_agent {
                            RuntimeEvent::Failed(error.to_string())
                        } else {
                            RuntimeEvent::Notice {
                                title: "Command failed".to_owned(),
                                details: vec![error.to_string()],
                            }
                        };
                        let _ = events.send(event);
                    }
                }
            }
            RuntimeCommand::ConfigureModel(configuration) => {
                if let Err(error) = configure_model(&mut state, configuration, &events) {
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Model configuration failed".to_owned(),
                        details: vec![error.to_string()],
                    });
                }
            }
            RuntimeCommand::Recovery {
                view,
                request,
                preflight,
            } => match recovery_tui::execute_view_action(&state.repo, &view, &request, preflight) {
                Ok(receipt) => {
                    let _ = events.send(RuntimeEvent::RecoveryCompleted(receipt));
                }
                Err(error) => {
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Recovery action failed closed".to_owned(),
                        details: vec![error],
                    });
                }
            },
            RuntimeCommand::Shutdown => break,
        }
    }
    mark_idle(&submission, true);
}

fn capability_event(repo: PathBuf) -> RuntimeEvent {
    match CapabilityRegistry::discover(repo) {
        Ok(registry) => RuntimeEvent::Notice {
            title: "Runtime capabilities".to_owned(),
            details: registry
                .prompt_summary()
                .lines()
                .map(str::to_owned)
                .collect(),
        },
        Err(error) => RuntimeEvent::Notice {
            title: "Runtime capabilities unavailable".to_owned(),
            details: vec![error.to_string()],
        },
    }
}

fn cancel_requested(cancel: &Arc<AtomicBool>, submission: &Arc<Mutex<SubmissionState>>) -> bool {
    if cancel.load(Ordering::SeqCst) {
        mark_idle(submission, true);
        true
    } else {
        false
    }
}

fn mark_idle(submission: &Arc<Mutex<SubmissionState>>, clear_followups: bool) {
    let mut state = lock_submission(submission);
    state.busy = false;
    if clear_followups {
        state.followups.clear();
    }
}

fn take_followups(submission: &Arc<Mutex<SubmissionState>>) -> Vec<QueuedFollowup> {
    lock_submission(submission).followups.drain(..).collect()
}

fn finish_or_take_followups(submission: &Arc<Mutex<SubmissionState>>) -> Vec<QueuedFollowup> {
    let mut state = lock_submission(submission);
    if state.followups.is_empty() {
        state.busy = false;
        Vec::new()
    } else {
        state.followups.drain(..).collect()
    }
}

struct RuntimeState {
    repo: PathBuf,
    base_config: Config,
    config: Config,
    session: Option<AgentSession>,
    pending_goal: Option<String>,
    pending_skill: Option<SelectedSkill>,
    session_api_key: Option<String>,
    effort: Effort,
    plan_mode: bool,
    team_control: TeamControlPlane,
}

impl RuntimeState {
    fn load(repo: PathBuf) -> Result<Self, RuntimeError> {
        let project = repo.join(".medusa/config.toml");
        let project = project.exists().then_some(project);
        let config =
            Config::load_layers(None, project.as_deref(), &BTreeMap::new(), &BTreeMap::new())
                .map_err(RuntimeError::agent)?;
        Ok(Self::from_config(repo, config))
    }

    fn from_config(repo: PathBuf, config: Config) -> Self {
        Self {
            repo,
            base_config: config.clone(),
            effort: effort_for_turns(config.agent.max_turns),
            plan_mode: config.agent.mode == Mode::ReadOnly,
            config,
            session: None,
            pending_goal: None,
            pending_skill: None,
            session_api_key: None,
            team_control: TeamControlPlane::default(),
        }
    }

    fn settings_event(&self) -> RuntimeEvent {
        RuntimeEvent::Settings {
            model: format!(
                "{} / {}",
                self.config.model.provider, self.config.model.name
            ),
            effort: format!("effort:{}", self.effort.label()),
            plan_mode: self.plan_mode,
            credential_configured: self.session_api_key.is_some()
                || credential_environment(&self.config.model.provider)
                    .is_some_and(|name| env::var(name).is_ok()),
            context_window_tokens: self.config.model.context_window_tokens,
            auto_compact_percent: self.config.model.auto_compact_percent,
        }
    }
}

fn run_prompt(
    state: &mut RuntimeState,
    draft: PromptDraft,
    events: &Sender<RuntimeEvent>,
    cancel: &Arc<AtomicBool>,
    submission: &Arc<Mutex<SubmissionState>>,
    accepted: Option<Sender<()>>,
) -> Result<RuntimeEvent, RuntimeError> {
    let config = state.config.clone();
    let max_turns = config.agent.max_turns;
    let provider = ConfiguredProvider::manager_from_config(&config, state.session_api_key.clone())
        .map_err(RuntimeError::agent)?;
    let resuming_pending_question = state
        .session
        .as_ref()
        .is_some_and(|session| session.pending_question.is_some());
    if !resuming_pending_question {
        crate::review::capture_review_baseline(&state.repo)
            .map_err(|error| RuntimeError::agent(error.to_string()))?;
    }
    let selected_skill = state.pending_skill.clone();
    let execution_plan =
        crate::production_orchestrator::plan_for_repository(&state.repo, &draft)
            .map_err(RuntimeError::agent)?;
    let coordinated =
        execution_plan.mode == crate::production_orchestrator::ExecutionMode::Orchestrated;
    let engine = AgentEngine::new_with_cancellation(provider, config.clone(), Arc::clone(cancel));
    let engine = if coordinated {
        engine.with_execution_policy(medusa_agent::AgentExecutionPolicy::for_team_role(
            medusa_agent::TeamRole::Reviewer,
        ))
    } else {
        engine
    };
    let content = message_blocks(&draft)?;
    let session = match state.session.take() {
        Some(mut session) => {
            let update = if session.pending_question.is_some() {
                engine.answer_pending_question(&mut session, content)
            } else {
                engine.append_user_message(&mut session, content)
            };
            if let Err(error) = update {
                state.session = Some(session);
                return Err(RuntimeError::agent(error));
            }
            session
        }
        None => {
            let objective = state
                .pending_goal
                .take()
                .unwrap_or_else(|| objective_for(&draft));
            engine
                .create_session_with_content(&state.repo, objective, content)
                .map_err(RuntimeError::agent)?
        }
    };
    lock_submission(submission).active_session_id = Some(session.id.to_string());
    state.session = Some(session);
    if let Some(accepted) = accepted {
        let _ = accepted.send(());
    }
    let session = state.session.as_mut().ok_or_else(|| {
        RuntimeError::agent("runtime session disappeared before execution plan recording")
    })?;
    medusa_agent::record_session_event(
        session,
        Actor::Coordinator,
        EventPayload::PlanCreated {
            plan: serde_json::to_value(&execution_plan).map_err(RuntimeError::agent)?,
        },
    )
    .map_err(RuntimeError::agent)?;

    let mut execution_ledger = if coordinated {
        let ledger = crate::production_orchestrator::open_ledger(
            &state.repo,
            session.id.as_str(),
            &execution_plan,
        )
        .map_err(RuntimeError::agent)?;
        let projected = crate::production_orchestrator::projection(&ledger);
        session.plan = projected.clone();
        let _ = events.send(RuntimeEvent::Plan(projected));
        Some(ledger)
    } else {
        None
    };
    if execution_plan.mode == crate::production_orchestrator::ExecutionMode::Direct {
        let _ = events.send(RuntimeEvent::Team(state.team_control.clear()));
    } else {
        state.team_control.clear();
    }
    for event in crate::production_orchestrator::events(&execution_plan) {
        let _ = events.send(event);
    }
    let coordinator_evidence = if !resuming_pending_question && coordinated {
        if let Some(ledger) = execution_ledger.as_mut() {
            crate::production_orchestrator::begin_kinds(
                ledger,
                &execution_plan,
                &[
                    medusa_multi_agent_scheduler::TaskKind::Analysis,
                    medusa_multi_agent_scheduler::TaskKind::RiskReview,
                ],
                "preflight",
            )
            .map_err(RuntimeError::agent)?;
            let _ = events.send(RuntimeEvent::Plan(
                crate::production_orchestrator::projection(ledger),
            ));
        }
        match crate::multi_agent_coordinator::run_preflight(
            &state.repo,
            &config,
            state.session_api_key.clone(),
            &execution_plan,
            cancel,
            &state.team_control,
            events,
        ) {
            Ok(evidence) => {
                if let Some(ledger) = execution_ledger.as_mut() {
                    crate::production_orchestrator::succeed_kinds(
                        ledger,
                        &execution_plan,
                        &[
                            medusa_multi_agent_scheduler::TaskKind::Analysis,
                            medusa_multi_agent_scheduler::TaskKind::RiskReview,
                        ],
                        "durable preflight worker evidence recorded",
                    )
                    .map_err(RuntimeError::agent)?;
                    let _ = events.send(RuntimeEvent::Plan(
                        crate::production_orchestrator::projection(ledger),
                    ));
                }
                Some(evidence)
            }
            Err(error) => {
                if let Some(ledger) = execution_ledger.as_mut() {
                    let _ = crate::production_orchestrator::fail_kinds(
                        ledger,
                        &execution_plan,
                        &[
                            medusa_multi_agent_scheduler::TaskKind::Analysis,
                            medusa_multi_agent_scheduler::TaskKind::RiskReview,
                        ],
                        &error,
                    );
                    let _ = events.send(RuntimeEvent::Plan(
                        crate::production_orchestrator::projection(ledger),
                    ));
                }
                return Err(RuntimeError::agent(error));
            }
        }
    } else {
        None
    };
    if let Some(evidence) = coordinator_evidence.as_ref() {
        let session = state.session.as_mut().ok_or_else(|| {
            RuntimeError::agent("runtime session disappeared before worker evidence recording")
        })?;
        medusa_agent::record_session_event(
            session,
            Actor::Coordinator,
            EventPayload::WorkerEvidenceRecorded {
                evidence: serde_json::to_value(evidence).map_err(RuntimeError::agent)?,
            },
        )
        .map_err(RuntimeError::agent)?;
    }
    let implementation_evidence =
        if crate::production_orchestrator::requires_mutation(&execution_plan) {
            let preflight = coordinator_evidence.as_ref().ok_or_else(|| {
                RuntimeError::agent("mutating execution requires coordinator preflight evidence")
            })?;
            if let Some(ledger) = execution_ledger.as_mut() {
                crate::production_orchestrator::begin_kinds(
                    ledger,
                    &execution_plan,
                    &[medusa_multi_agent_scheduler::TaskKind::Implementation],
                    "implementation",
                )
                .map_err(RuntimeError::agent)?;
                let _ = events.send(RuntimeEvent::Plan(
                    crate::production_orchestrator::projection(ledger),
                ));
            }
            match crate::mutating_worker_coordinator::run_implementation(
                &state.repo,
                &config,
                state.session_api_key.clone(),
                &execution_plan,
                preflight,
                cancel,
                (&state.team_control, events),
            ) {
                Ok(evidence) => {
                    if let Some(ledger) = execution_ledger.as_mut() {
                        crate::production_orchestrator::succeed_kinds(
                            ledger,
                            &execution_plan,
                            &[medusa_multi_agent_scheduler::TaskKind::Implementation],
                            "immutable isolated implementation prepared for parent review",
                        )
                        .map_err(RuntimeError::agent)?;
                        let _ = events.send(RuntimeEvent::Plan(
                            crate::production_orchestrator::projection(ledger),
                        ));
                    }
                    Some(evidence)
                }
                Err(error) => {
                    if let Some(ledger) = execution_ledger.as_mut() {
                        let _ = crate::production_orchestrator::fail_kinds(
                            ledger,
                            &execution_plan,
                            &[medusa_multi_agent_scheduler::TaskKind::Implementation],
                            &error,
                        );
                        let _ = events.send(RuntimeEvent::Plan(
                            crate::production_orchestrator::projection(ledger),
                        ));
                    }
                    return Err(RuntimeError::agent(error));
                }
            }
        } else {
            None
        };
    if let Some(evidence) = implementation_evidence.as_ref() {
        let session = state.session.as_mut().ok_or_else(|| {
            RuntimeError::agent("runtime session disappeared before prepared evidence recording")
        })?;
        medusa_agent::record_session_event(
            session,
            Actor::Coordinator,
            EventPayload::WorkerEvidenceRecorded {
                evidence: serde_json::to_value(evidence).map_err(RuntimeError::agent)?,
            },
        )
        .map_err(RuntimeError::agent)?;
    }
    let orchestration_context = crate::production_orchestrator::runtime_context(&execution_plan);
    let tool_policy_context =
        crate::tool_policy::runtime_context(&draft).map_err(RuntimeError::agent)?;
    let verification_plan = medusa_tool_control::verification_plan(&draft.text);
    let verification_context = format!(
        "Progressive verification requirements: {:?}. Rationale: {:?}. Complete the narrowest checks first and escalate only when required by risk or failure.",
        verification_plan.requirements, verification_plan.rationale
    );
    let session_id = state.session.as_ref().map(|session| session.id.as_str());
    let learning_context =
        crate::learning_retrieval::select(&state.repo, &draft, session_id, events);
    let mut task_context = vec![
        orchestration_context,
        tool_policy_context,
        verification_context,
    ];
    if implementation_evidence.is_none() {
    if let Some(evidence) = coordinator_evidence.as_ref() {
        task_context.push(evidence.parent_context());
    }
}
if let Some(evidence) = implementation_evidence.as_ref() {
    task_context.push(evidence.parent_context());
}
    if let Some(learning) = learning_context.prompt_context {
        task_context.push(learning);
    }
    if let Some(skill) = selected_skill.as_ref().map(SelectedSkill::prompt_context) {
        task_context.push(skill);
    }
    let skill_context = task_context.join("\n\n");
    let mut session = state
        .session
        .take()
        .ok_or_else(|| RuntimeError::agent("runtime session disappeared before execution"))?;
    if coordinated {
        if let Some(ledger) = execution_ledger.as_mut() {
            crate::production_orchestrator::begin_kinds(
                ledger,
                &execution_plan,
                &[medusa_multi_agent_scheduler::TaskKind::Review],
                "parent-review",
            )
            .map_err(RuntimeError::agent)?;
            let _ = events.send(RuntimeEvent::Plan(
                crate::production_orchestrator::projection(ledger),
            ));
        }
    }
    let mut updates = UpdateState::new();
    if coordinated {
        updates.suppress_model_plan();
    }
    if !coordinated && !session.plan.is_empty() {
        let _ = events.send(RuntimeEvent::Plan(session.plan.clone()));
    }

    let result = (|| {
        loop {
            if cancel_requested(cancel, submission) {
                if let Some(ledger) = execution_ledger.as_mut() {
                    let _ = ledger.cancel_remaining("runtime cancellation requested");
                    let projected = crate::production_orchestrator::projection(ledger);
                    session.plan = projected.clone();
                    let _ = events.send(RuntimeEvent::Plan(projected));
                }
                return Ok(RuntimeEvent::Cancelled);
            }
            append_followups(&engine, &mut session, take_followups(submission))?;
            if session.turn >= max_turns {
                return Err(RuntimeError::TurnLimit(max_turns));
            }
            let provider_activity_id =
                format!("provider-request-{}", session.turn.saturating_add(1));
            let provider_signature = format!(
                "{}:{}:{}",
                state.config.model.provider, state.config.model.name, provider_activity_id
            );
            let mut retry_guard = medusa_tool_control::RetryGuard::new(2);
            let mut next_attempt = 1_u8;
            let outcome = loop {
                let attempt_signature = format!("{provider_signature}:attempt:{next_attempt}");
                let attempt = retry_guard
                    .begin_attempt(&attempt_signature)
                    .map_err(RuntimeError::agent)?;
                next_attempt = next_attempt.saturating_add(1);
                let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                    id: Some(provider_activity_id.clone()),
                    kind: RuntimeActivityKind::Progress,
                    title: format!(
                        "Waiting for {} / {} response",
                        state.config.model.provider, state.config.model.name
                    ),
                    details: vec![
                        format!("bounded attempt {attempt}/2"),
                        format!(
                            "verification requirements: {:?}",
                            verification_plan.requirements
                        ),
                    ],
                }));
                let provider_started_at = std::time::Instant::now();
                match engine.step_with_observer_and_context(
                    &mut session,
                    Some(skill_context.as_str()),
                    |update| {
                        forward_update(update, events, &mut updates);
                    },
                ) {
                    Ok(outcome) => {
                        let provider_duration_ms =
                            u64::try_from(provider_started_at.elapsed().as_millis())
                                .unwrap_or(u64::MAX);
                        let trace = medusa_tool_control::trace(
                            &attempt_signature,
                            attempt,
                            None,
                            &verification_plan,
                        );
                        let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                            id: Some(provider_activity_id.clone()),
                            kind: RuntimeActivityKind::Done,
                            title: "Model response received".to_owned(),
                            details: vec![
                                format!("completed in {provider_duration_ms} ms"),
                                format!("execution trace {}", trace.fingerprint),
                            ],
                        }));
                        break outcome;
                    }
                    Err(_) if cancel_requested(cancel, submission) => {
                        return Ok(RuntimeEvent::Cancelled);
                    }
                    Err(error) => {
                        let error_text = error.to_string();
                        let decision = retry_guard.decide(&error_text);
                        let trace = medusa_tool_control::trace(
                            &attempt_signature,
                            attempt,
                            Some(&decision),
                            &verification_plan,
                        );
                        let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                            id: Some(provider_activity_id.clone()),
                            kind: RuntimeActivityKind::Progress,
                            title: "Provider attempt classified".to_owned(),
                            details: vec![
                                format!("class={:?}; action={:?}", decision.class, decision.action),
                                decision.rationale.clone(),
                                format!("execution trace {}", trace.fingerprint),
                            ],
                        }));
                        if decision.action == medusa_tool_control::RetryAction::Retry {
                            continue;
                        }
                        return Err(RuntimeError::agent(error));
                    }
                }
            };
            let _ = events.send(RuntimeEvent::Progress { turn: session.turn });

            if matches!(outcome, StepOutcome::Continue | StepOutcome::TurnComplete)
                && should_auto_compact(
                    updates.current_context_tokens,
                    state.config.model.context_window_tokens,
                    state.config.model.auto_compact_percent,
                )
            {
                compact_session(&mut session, None).map_err(RuntimeError::agent)?;
                updates.current_context_tokens = 0;
                let _ = events.send(RuntimeEvent::Compacted {
                    message: format!(
                        "Auto-compacted at {}% of the {}-token context window.",
                        state.config.model.auto_compact_percent,
                        state.config.model.context_window_tokens
                    ),
                });
            }

            if cancel_requested(cancel, submission) {
                if let Some(ledger) = execution_ledger.as_mut() {
                    let _ = ledger.cancel_remaining("runtime cancellation requested");
                    let projected = crate::production_orchestrator::projection(ledger);
                    session.plan = projected.clone();
                    let _ = events.send(RuntimeEvent::Plan(projected));
                }
                return Ok(RuntimeEvent::Cancelled);
            }

            if matches!(outcome, StepOutcome::WaitingForUser) {
                mark_idle(submission, false);
                let question = session.pending_question.as_ref().ok_or_else(|| {
                    RuntimeError::agent("agent paused without a pending question")
                })?;
                return Ok(RuntimeEvent::Question(question.clone()));
            }

            let queued = if matches!(outcome, StepOutcome::Completed | StepOutcome::TurnComplete) {
                finish_or_take_followups(submission)
            } else {
                take_followups(submission)
            };
            if !queued.is_empty() {
                append_followups(&engine, &mut session, queued)?;
                continue;
            }

            match outcome {
                StepOutcome::Completed => {
                    return Ok(RuntimeEvent::Completed {
                        session_id: session.id.to_string(),
                    });
                }
                StepOutcome::TurnComplete => return Ok(RuntimeEvent::TurnFinished),
                StepOutcome::Continue => {}
                StepOutcome::WaitingForUser => {
                    return Err(RuntimeError::agent(
                        "agent remained paused after its pending question was handled",
                    ));
                }
            }
        }
    })();
    let waiting_for_user = matches!(&result, Ok(RuntimeEvent::Question(_)));
    if selected_skill.is_some() && !waiting_for_user {
        state.pending_skill = None;
    }
    let mut result = result;
    let terminal_turn = matches!(
        &result,
        Ok(RuntimeEvent::Completed { .. } | RuntimeEvent::TurnFinished)
    );
    let mut verified = terminal_turn && !crate::production_orchestrator::requires_mutation(&execution_plan);
    if coordinated {
        if let Some(evidence) = implementation_evidence.as_ref() {
            match &result {
                Ok(RuntimeEvent::Completed { .. } | RuntimeEvent::TurnFinished) => {
                    match crate::mutation_transaction::complete_after_parent_review(
                        &evidence.transaction_path,
                        &state.repo,
                        &session,
                        events,
                    ) {
                        Ok(crate::mutation_transaction::TransactionCompletion::Reconciled(receipt)) => {
                            verified = true;
                            if let Some(ledger) = execution_ledger.as_mut() {
                                crate::production_orchestrator::succeed_kinds(
                                    ledger,
                                    &execution_plan,
                                    &[medusa_multi_agent_scheduler::TaskKind::Review],
                                    "parent review accepted the immutable prepared commit",
                                )
                                .map_err(RuntimeError::agent)?;
                                crate::production_orchestrator::begin_kinds(
                                    ledger,
                                    &execution_plan,
                                    &[medusa_multi_agent_scheduler::TaskKind::Verification],
                                    "independent-verification",
                                )
                                .map_err(RuntimeError::agent)?;
                                crate::production_orchestrator::succeed_kinds(
                                    ledger,
                                    &execution_plan,
                                    &[medusa_multi_agent_scheduler::TaskKind::Verification],
                                    "independent verification, authorization, integration, and reconciliation completed",
                                )
                                .map_err(RuntimeError::agent)?;
                            }
                            medusa_agent::record_session_event(
                                &mut session,
                                Actor::Coordinator,
                                EventPayload::IntegrationReceiptRecorded {
                                    receipt: serde_json::to_value(&receipt)
                                        .map_err(RuntimeError::agent)?,
                                },
                            )
                            .map_err(RuntimeError::agent)?;
                        }
                        Ok(crate::mutation_transaction::TransactionCompletion::RevisionRequested(reason)) => {
                            if let Some(ledger) = execution_ledger.as_mut() {
                                let _ = crate::production_orchestrator::fail_kinds(
                                    ledger,
                                    &execution_plan,
                                    &[medusa_multi_agent_scheduler::TaskKind::Review],
                                    &reason,
                                );
                            }
                            result = Err(RuntimeError::agent(format!(
                                "parent review requested a bounded isolated revision: {reason}"
                            )));
                        }
                        Err(error) => {
                            if let Some(ledger) = execution_ledger.as_mut() {
                                let _ = crate::production_orchestrator::fail_kinds(
                                    ledger,
                                    &execution_plan,
                                    &[
                                        medusa_multi_agent_scheduler::TaskKind::Review,
                                        medusa_multi_agent_scheduler::TaskKind::Verification,
                                    ],
                                    &error,
                                );
                            }
                            result = Err(RuntimeError::agent(error));
                        }
                    }
                }
                Ok(RuntimeEvent::Cancelled) => {
                    let _ = crate::mutation_transaction::cancel_transaction(
                        &evidence.transaction_path,
                        "runtime cancellation completed",
                        events,
                    );
                    if let Some(ledger) = execution_ledger.as_mut() {
                        let _ = ledger.cancel_remaining("runtime cancellation completed");
                    }
                }
                Err(error) => {
                    let _ = crate::mutation_transaction::fail_transaction(
                        &evidence.transaction_path,
                        &error.to_string(),
                        events,
                    );
                    if let Some(ledger) = execution_ledger.as_mut() {
                        let _ = crate::production_orchestrator::fail_kinds(
                            ledger,
                            &execution_plan,
                            &[medusa_multi_agent_scheduler::TaskKind::Review],
                            &error.to_string(),
                        );
                    }
                }
                _ => {}
            }
        } else if let Some(ledger) = execution_ledger.as_mut() {
            match &result {
                Ok(RuntimeEvent::Completed { .. } | RuntimeEvent::TurnFinished) => {
                    crate::production_orchestrator::succeed_kinds(
                        ledger,
                        &execution_plan,
                        &[medusa_multi_agent_scheduler::TaskKind::Review],
                        "parent review completed from durable execution evidence",
                    )
                    .map_err(RuntimeError::agent)?;
                }
                Ok(RuntimeEvent::Cancelled) => {
                    let _ = ledger.cancel_remaining("runtime cancellation completed");
                }
                Err(error) => {
                    let _ = crate::production_orchestrator::fail_kinds(
                        ledger,
                        &execution_plan,
                        &[medusa_multi_agent_scheduler::TaskKind::Review],
                        &error.to_string(),
                    );
                }
                _ => {}
            }
        }
        if let Some(ledger) = execution_ledger.as_ref() {
            let projected = crate::production_orchestrator::projection(ledger);
            session.plan = projected.clone();
            let _ = events.send(RuntimeEvent::Plan(projected));
        }
    }
    let failed = result.is_err();
    if let Err(error) = crate::production_orchestrator::persist_outcome(
        &state.repo,
        &draft,
        &execution_plan,
        verified,
        failed,
    ) {
        let _ = events.send(RuntimeEvent::Notice {
            title: "Runtime learning record unavailable".to_owned(),
            details: vec![error.to_string()],
        });
    }
    if coordinated {
        let _ = events.send(RuntimeEvent::Team(state.team_control.finish()));
    }
    state.session = Some(session);
    result
}

fn append_followups<P: ModelProvider>(
    engine: &AgentEngine<P>,
    session: &mut AgentSession,
    followups: Vec<QueuedFollowup>,
) -> Result<(), RuntimeError> {
    for followup in followups {
        if !followup.durably_recorded {
            medusa_agent::record_session_event(
                session,
                Actor::User,
                EventPayload::UserFollowupQueued {
                    command_id: followup.command_id.clone(),
                    prompt: serde_json::to_value(&followup.draft).map_err(RuntimeError::agent)?,
                },
            )
            .map_err(RuntimeError::agent)?;
        }
        engine
            .append_queued_user_message(
                session,
                followup.command_id,
                message_blocks(&followup.draft)?,
            )
            .map_err(RuntimeError::agent)?;
    }
    Ok(())
}

#[cfg(test)]
fn execute_slash_command(
    state: &mut RuntimeState,
    command: SlashCommand,
    events: &Sender<RuntimeEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<Option<RuntimeEvent>, RuntimeError> {
    let submission = Arc::new(Mutex::new(SubmissionState {
        busy: true,
        ..SubmissionState::default()
    }));
    execute_slash_command_with_submission(state, command, events, cancel, &submission)
}

fn execute_slash_command_with_submission(
    state: &mut RuntimeState,
    command: SlashCommand,
    events: &Sender<RuntimeEvent>,
    cancel: &Arc<AtomicBool>,
    submission: &Arc<Mutex<SubmissionState>>,
) -> Result<Option<RuntimeEvent>, RuntimeError> {
    match command {
        SlashCommand::Config(command) => {
            crate::config_command::execute(state, command, events)?;
        }
        SlashCommand::Team(_) => {
            return Err(RuntimeError::InvalidCommand(
                "team commands must execute through the live control plane".to_owned(),
            ));
        }
        SlashCommand::Learning { action } => {
            let snapshot = crate::learning_review::read(&state.repo)
                .map_err(|error| RuntimeError::agent(error.to_string()))?;
            match action {
                LearningCommand::Show { filter } => {
                    let mut details = Vec::new();
                    details.push(format!(
                        "privacy: capture={} user_persistence={} cross_repository={} telemetry={} automatic_proposals={}",
                        snapshot.privacy.capture_enabled,
                        snapshot.privacy.user_persistence_enabled,
                        snapshot.privacy.cross_repository_reuse_enabled,
                        snapshot.privacy.telemetry_enabled,
                        snapshot.privacy.automatic_proposals_enabled,
                    ));
                    for item in &snapshot.items {
                        let searchable = format!(
                            "{} {} {} {:?} {:?}",
                            item.id, item.title, item.scope, item.kind, item.state
                        )
                        .to_ascii_lowercase();
                        if filter
                            .as_ref()
                            .is_some_and(|value| !searchable.contains(&value.to_ascii_lowercase()))
                        {
                            continue;
                        }
                        details.push(format!(
                            "{} | {:?} | {:?} | {} | confidence {}",
                            item.id, item.state, item.kind, item.scope, item.confidence_milli
                        ));
                        details.push(format!("  learned: {}", item.generalized_rule));
                        details.push(format!("  why: {}", item.root_cause));
                        if let Some(replay) = &item.replay {
                            details.push(format!(
                                "  replay: reproduced={} resolved={} regressions={}",
                                replay.reproduced, replay.resolved, replay.regression_count
                            ));
                        }
                        if !item.conflicts_with.is_empty() {
                            details.push(format!(
                                "  conflicts: {}",
                                item.conflicts_with
                                    .iter()
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ));
                        }
                    }
                    if snapshot.items.is_empty() {
                        details.push("No learning proposals are recorded.".to_owned());
                    }
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Learning review".to_owned(),
                        details,
                    });
                }
                LearningCommand::Privacy => {
                    let preview = crate::learning_review::redaction_preview(&state.repo)
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Learning privacy".to_owned(),
                        details: vec![
                            format!("Capture: {}", snapshot.privacy.capture_enabled),
                            format!(
                                "User-level persistence: {}",
                                snapshot.privacy.user_persistence_enabled
                            ),
                            format!(
                                "Cross-repository reuse: {}",
                                snapshot.privacy.cross_repository_reuse_enabled
                            ),
                            format!("Telemetry: {}", snapshot.privacy.telemetry_enabled),
                            format!(
                                "Automatic proposals: {}",
                                snapshot.privacy.automatic_proposals_enabled
                            ),
                            format!("Export redaction safe: {}", preview.safe),
                            preview.warnings.join(" "),
                        ],
                    });
                }
                LearningCommand::Export => {
                    let export = crate::learning_review::export(&state.repo)
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let json = serde_json::to_string_pretty(&export)
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Learning audit export".to_owned(),
                        details: json.lines().map(str::to_owned).collect(),
                    });
                }
                action => {
                    let (id, target) = match action {
                        LearningCommand::Approve { id } => {
                            (id, crate::learning_review::LearningReviewState::Approved)
                        }
                        LearningCommand::Reject { id } => {
                            (id, crate::learning_review::LearningReviewState::Rejected)
                        }
                        LearningCommand::Defer { id } => {
                            (id, crate::learning_review::LearningReviewState::Deferred)
                        }
                        LearningCommand::Validate { id } => {
                            (id, crate::learning_review::LearningReviewState::Validated)
                        }
                        LearningCommand::Activate { id } => {
                            (id, crate::learning_review::LearningReviewState::Active)
                        }
                        LearningCommand::Suspend { id } => {
                            (id, crate::learning_review::LearningReviewState::Suspended)
                        }
                        LearningCommand::Rollback { id } => {
                            (id, crate::learning_review::LearningReviewState::RolledBack)
                        }
                        LearningCommand::Delete { id } => {
                            (id, crate::learning_review::LearningReviewState::Deleted)
                        }
                        LearningCommand::Show { .. }
                        | LearningCommand::Privacy
                        | LearningCommand::Export => unreachable!(),
                    };
                    let updated = crate::learning_review::transition(
                        &state.repo,
                        &id,
                        target,
                        snapshot.revision,
                        "tui",
                    )
                    .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let item = updated
                        .items
                        .iter()
                        .find(|item| item.id == id)
                        .ok_or_else(|| RuntimeError::agent("updated learning item disappeared"))?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Learning lifecycle updated".to_owned(),
                        details: vec![format!(
                            "{} is now {:?} at revision {}",
                            item.id, item.state, updated.revision
                        )],
                    });
                }
            }
        }
        SlashCommand::Review { action } => {
            let workspace = crate::review::read_review_workspace(&state.repo)
                .map_err(|error| RuntimeError::agent(error.to_string()))?;
            match action {
                ReviewCommand::Show { filter } => {
                    let mut details = Vec::new();
                    for file in &workspace.snapshot.files {
                        let label =
                            format!("{:?} {:?} {:?}", file.kind, file.origin, file.verification)
                                .to_ascii_lowercase();
                        if filter.as_ref().is_some_and(|value| {
                            !file.path.contains(value)
                                && !label.contains(&value.to_ascii_lowercase())
                        }) {
                            continue;
                        }
                        details.push(format!(
                            "{} | {:?} | {:?} | {:?}",
                            file.path, file.kind, file.origin, file.review_state
                        ));
                        if let Some(diff) = workspace
                            .files
                            .iter()
                            .find(|candidate| candidate.path == file.path)
                        {
                            for hunk in &diff.hunks {
                                details.push(format!("  hunk {} {}", hunk.id, hunk.header));
                            }
                        }
                    }
                    if details.is_empty() {
                        details.push("No changes match the review filter.".to_owned());
                    }
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Repository review".to_owned(),
                        details,
                    });
                }
                ReviewCommand::AcceptFile { path } => {
                    crate::review::apply_review_action(
                        &state.repo,
                        medusa_review_model::ReviewActionRequest::AcceptFile {
                            path,
                            expected_snapshot_id: workspace.snapshot.id.clone(),
                        },
                        "tui",
                    )
                    .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Review accepted".to_owned(),
                        details: vec![
                            "File marked accepted; no commit, push, or merge was performed."
                                .to_owned(),
                        ],
                    });
                }
                ReviewCommand::AcceptTask => {
                    crate::review::apply_review_action(
                        &state.repo,
                        medusa_review_model::ReviewActionRequest::AcceptTask {
                            expected_snapshot_id: workspace.snapshot.id.clone(),
                        },
                        "tui",
                    )
                    .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice { title: "Task review accepted".to_owned(), details: vec!["All eligible Medusa changes were marked accepted; repository history was not modified.".to_owned()] });
                }
                ReviewCommand::RevertFile { path } => {
                    let file = workspace
                        .snapshot
                        .file(&path)
                        .ok_or_else(|| RuntimeError::agent("review file not found"))?;
                    crate::review::apply_review_action(
                        &state.repo,
                        medusa_review_model::ReviewActionRequest::RevertFile {
                            path,
                            expected_snapshot_id: workspace.snapshot.id.clone(),
                            expected_file_fingerprint: file.current_fingerprint.clone(),
                        },
                        "tui",
                    )
                    .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "File reverted".to_owned(),
                        details: vec![
                            "The selected Medusa file change was reverted safely.".to_owned(),
                        ],
                    });
                }
                ReviewCommand::RevertHunk { path, hunk_id } => {
                    let file = workspace
                        .snapshot
                        .file(&path)
                        .ok_or_else(|| RuntimeError::agent("review file not found"))?;
                    let hunk = file
                        .hunks
                        .iter()
                        .find(|candidate| candidate.id == hunk_id)
                        .ok_or_else(|| RuntimeError::agent("review hunk not found"))?;
                    crate::review::apply_review_action(
                        &state.repo,
                        medusa_review_model::ReviewActionRequest::RevertHunk {
                            path,
                            hunk_id,
                            expected_snapshot_id: workspace.snapshot.id.clone(),
                            expected_file_fingerprint: file.current_fingerprint.clone(),
                            expected_hunk_fingerprint: hunk.current_fingerprint.clone(),
                        },
                        "tui",
                    )
                    .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Hunk reverted".to_owned(),
                        details: vec!["The selected Medusa hunk was reverted safely.".to_owned()],
                    });
                }
                ReviewCommand::Export => {
                    let generated_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as i64;
                    let export = crate::review::export_review_audit(&state.repo, generated_at)
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let json = serde_json::to_string_pretty(&export)
                        .map_err(|error| RuntimeError::agent(error.to_string()))?;
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Review audit export".to_owned(),
                        details: json.lines().map(str::to_owned).collect(),
                    });
                }
            }
        }
        SlashCommand::Help => {
            let _ = events.send(RuntimeEvent::Notice {
                title: "Slash commands".to_owned(),
                details: commands::COMMAND_SPECS
                    .iter()
                    .map(|spec| format!("{} - {}", spec.usage, spec.description))
                    .collect(),
            });
        }
        SlashCommand::New => {
            if let Some(session) = state.session.as_mut() {
                medusa_agent::record_session_event(
                    session,
                    Actor::Coordinator,
                    EventPayload::SessionReset {
                        reason: "user requested a new session".to_owned(),
                    },
                )
                .map_err(RuntimeError::agent)?;
            }
            state.session = None;
            lock_submission(submission).active_session_id = None;
            state.pending_goal = None;
            state.pending_skill = None;
            state.config.agent.mode = state.base_config.agent.mode;
            state.plan_mode = false;
            let _ = events.send(RuntimeEvent::NewSession);
            let _ = events.send(state.settings_event());
        }
        SlashCommand::Compact { focus } => {
            let Some(session) = state.session.as_mut() else {
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Nothing to compact".to_owned(),
                    details: vec!["Start a task before compacting its context.".to_owned()],
                });
                return Ok(None);
            };
            let original_messages = session.messages.len();
            compact_session(session, focus.as_deref()).map_err(RuntimeError::agent)?;
            let _ = events.send(RuntimeEvent::Compacted {
                message: format!(
                    "Compacted session context from {original_messages} messages to a durable summary."
                ),
            });
        }
        SlashCommand::Goal { objective } => match objective {
            Some(objective) => {
                if let Some(session) = state.session.as_mut() {
                    update_session_objective(session, objective.clone())
                        .map_err(RuntimeError::agent)?;
                } else {
                    state.pending_goal = Some(objective.clone());
                }
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Goal updated".to_owned(),
                    details: vec![
                        objective,
                        "The goal will be included in the next agent turn.".to_owned(),
                    ],
                });
            }
            None => {
                let objective = state
                    .session
                    .as_ref()
                    .map(|session| session.objective.as_str())
                    .or(state.pending_goal.as_deref())
                    .unwrap_or("No goal is set; the next prompt becomes the session goal.");
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Current goal".to_owned(),
                    details: vec![objective.to_owned()],
                });
            }
        },
        SlashCommand::Model(model_command) => match model_command {
            ModelCommand::Show => {
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Model configuration".to_owned(),
                    details: model_configuration_details(state),
                });
            }
            ModelCommand::SetModel(model) => {
                state.config.model.name = model;
                let _ = events.send(state.settings_event());
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Model updated".to_owned(),
                    details: model_configuration_details(state),
                });
            }
            ModelCommand::SetProvider(provider) => {
                if !is_supported_provider(&provider) {
                    return Err(RuntimeError::InvalidCommand(format!(
                        "supported providers are {}",
                        SUPPORTED_PROVIDERS.join(", ")
                    )));
                }
                state.config.model.protocol = protocol_for_provider(&provider).to_owned();
                state.config.model.provider = provider;
                let _ = events.send(state.settings_event());
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Provider updated".to_owned(),
                    details: model_configuration_details(state),
                });
            }
            ModelCommand::SetApiKey(key) => {
                state.session_api_key = Some(key);
                let _ = events.send(RuntimeEvent::Notice {
                    title: "API key updated".to_owned(),
                    details: vec![
                        "The key is applied only to this Medusa process and is not shown, logged, or written to disk."
                            .to_owned(),
                    ],
                });
            }
        },
        SlashCommand::Effort { effort } => match effort {
            Some(Effort::Auto) => {
                state.config.agent.max_turns = state.base_config.agent.max_turns;
                state.effort = Effort::Auto;
                let _ = events.send(state.settings_event());
            }
            Some(effort) => {
                state.config.agent.max_turns = turns_for_effort(effort);
                state.effort = effort;
                let _ = events.send(state.settings_event());
            }
            None => {
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Effort".to_owned(),
                    details: vec![format!(
                        "{} ({} turn budget)",
                        state.effort.label(),
                        state.config.agent.max_turns
                    )],
                });
            }
        },
        SlashCommand::Skills => {
            let skills = discover_skills(&state.repo);
            let _ = events.send(RuntimeEvent::Notice {
                title: "Available skills".to_owned(),
                details: if skills.is_empty() {
                    vec![
                        "No skills found in .medusa/skills, .claude/skills, or their user equivalents."
                            .to_owned(),
                    ]
                } else {
                    let mut details = vec![
                        "Run /<name> to load a skill for the next prompt, or /<name> <task> to use it immediately."
                            .to_owned(),
                    ];
                    details.extend(skills);
                    details
                },
            });
        }
        SlashCommand::Skill { selector, task } if selector.eq_ignore_ascii_case("recovery") => {
            match recovery_tui::execute_command(&state.repo, task.as_deref()) {
                Ok(Some(receipt)) => {
                    let _ = events.send(RuntimeEvent::RecoveryCompleted(receipt));
                }
                Ok(None) => {
                    for event in recovery::startup_events(&state.repo) {
                        let _ = events.send(event);
                    }
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Recovery commands".to_owned(),
                        details: vec!["/recovery inspect|resume|verify|abandon or /recovery restore <checkpoint> [--confirm]".to_owned()],
                    });
                }
                Err(error) => {
                    let _ = events.send(RuntimeEvent::Notice {
                        title: "Recovery action failed closed".to_owned(),
                        details: vec![error],
                    });
                }
            }
        }
        SlashCommand::Skill { selector, task } => {
            let skill = load_selected_skill(&state.repo, &selector)?;
            let label = skill.label();
            if let Some(task) = task {
                state.pending_skill = Some(skill);
                let result = run_prompt(
                    state,
                    PromptDraft {
                        text: task,
                        ..PromptDraft::default()
                    },
                    events,
                    cancel,
                    submission,
                    None,
                )
                .map(Some);
                if result.is_err() {
                    state.pending_skill = None;
                }
                return result;
            }
            state.pending_skill = Some(skill);
            let _ = events.send(RuntimeEvent::Notice {
                title: "Skill loaded".to_owned(),
                details: vec![
                    label,
                    "The next prompt will use this skill without persisting its instructions."
                        .to_owned(),
                ],
            });
        }
        SlashCommand::Plan { task } => {
            if task.as_deref().is_some_and(|value| {
                matches!(value.to_ascii_lowercase().as_str(), "off" | "execute")
            }) {
                state.config.agent.mode = state.base_config.agent.mode;
                state.plan_mode = false;
                let _ = events.send(state.settings_event());
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Planning mode off".to_owned(),
                    details: vec!["Subsequent prompts can make changes again.".to_owned()],
                });
            } else {
                state.config.agent.mode = Mode::ReadOnly;
                state.plan_mode = true;
                let _ = events.send(state.settings_event());
                if let Some(task) = task {
                    return run_prompt(
                        state,
                        PromptDraft {
                            text: task,
                            ..PromptDraft::default()
                        },
                        events,
                        cancel,
                        submission,
                        None,
                    )
                    .map(Some);
                }
                let _ = events.send(RuntimeEvent::Notice {
                    title: "Planning mode on".to_owned(),
                    details: vec![
                        "The next prompt will inspect the repository and return a read-only plan. Use /plan off to resume execution."
                            .to_owned(),
                    ],
                });
            }
        }
    }
    Ok(None)
}

mod recovery_projection;
mod recovery_tui;
mod tool_policy;

pub use medusa_recovery_coordinator::{
    RecoveryActionRequest, RecoveryAuditRecord, RecoveryExecutionReceipt, RecoveryOperation,
    RecoveryPreflightEvidence, RecoveryView,
};

pub fn recovery_action_context(
    repo: &std::path::Path,
    request: &RecoveryActionRequest,
) -> Result<(RecoveryView, RecoveryPreflightEvidence), String> {
    recovery::action_context(repo, request)
}

mod recovery {
    use std::{
        convert::Infallible,
        fs,
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    use medusa_recovery_coordinator::{
        AuthorizedRecoveryAction, CheckpointPresentation, RecoveryActionExecutor,
        RecoveryActionRequest, RecoveryActionService, RecoveryExecutionOutcome,
        RecoveryExecutionReceipt, RecoveryOperation, RecoveryPreflightEvidence, RecoveryPreview,
        RecoveryView, RecoveryViewInput, VerificationState,
    };
    use serde::Deserialize;

    use super::RuntimeEvent;

    const RECOVERY_DIRECTORY: &str = ".medusa/recovery";

    #[derive(Debug, Deserialize)]
    struct PersistedRecoveryRecord {
        session_id: String,
        last_durable_step: String,
        interrupted_operation: Option<String>,
        current_repository_fingerprint: String,
        verification: VerificationState,
        approvals_must_be_reestablished: bool,
        containment_must_be_reestablished: bool,
        checkpoints: Vec<CheckpointPresentation>,
        selected_preview: Option<RecoveryPreview>,
    }

    struct RuntimeRecoveryExecutor {
        repository_fingerprint: String,
    }

    impl RecoveryActionExecutor for RuntimeRecoveryExecutor {
        type Error = Infallible;

        fn execute(
            &mut self,
            _repository: &Path,
            action: &AuthorizedRecoveryAction,
        ) -> Result<RecoveryExecutionOutcome, Self::Error> {
            let outcome = match action.operation {
                RecoveryOperation::Inspect => RecoveryExecutionOutcome::succeeded(
                    self.repository_fingerprint.clone(),
                    VerificationState::Unknown,
                ),
                RecoveryOperation::Resume => RecoveryExecutionOutcome::succeeded(
                    self.repository_fingerprint.clone(),
                    VerificationState::Incomplete,
                ),
                RecoveryOperation::RetryVerification => RecoveryExecutionOutcome::succeeded(
                    self.repository_fingerprint.clone(),
                    VerificationState::Incomplete,
                ),
                RecoveryOperation::Abandon => {
                    RecoveryExecutionOutcome::cancelled(VerificationState::Incomplete)
                }
                RecoveryOperation::RestoreCheckpoint => RecoveryExecutionOutcome::failed_closed(
                    "checkpoint payload restoration is not available in the runtime executor",
                    Some(self.repository_fingerprint.clone()),
                    VerificationState::Incomplete,
                ),
            };
            Ok(outcome)
        }
    }

    pub(crate) fn action_context(
        repo: &Path,
        request: &RecoveryActionRequest,
    ) -> Result<(RecoveryView, RecoveryPreflightEvidence), String> {
        let view = discover(repo)
            .into_iter()
            .find(|view| view.session_id == request.session_id)
            .ok_or_else(|| {
                format!(
                    "recovery session {} is no longer available; refresh recovery state",
                    request.session_id
                )
            })?;

        let selected_checkpoint = request.checkpoint_id.as_deref().and_then(|checkpoint_id| {
            view.checkpoints
                .iter()
                .find(|checkpoint| checkpoint.id == checkpoint_id)
        });
        let checkpoint_integrity_verified = match request.operation {
            RecoveryOperation::RestoreCheckpoint => {
                selected_checkpoint.is_some_and(|checkpoint| checkpoint.integrity_verified)
            }
            _ => true,
        };
        let matching_preview = request.checkpoint_id.as_deref().and_then(|checkpoint_id| {
            view.selected_preview
                .as_ref()
                .filter(|preview| preview.checkpoint_id == checkpoint_id)
        });
        let conflicting_uncommitted_paths = matching_preview
            .map(|preview| {
                preview
                    .files
                    .iter()
                    .filter(|file| file.would_overwrite_uncommitted_work)
                    .map(|file| file.path.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut unresolved_risks = matching_preview
            .map(|preview| preview.unresolved_risks.clone())
            .unwrap_or_default();
        if matches!(request.operation, RecoveryOperation::RestoreCheckpoint)
            && matching_preview.is_none()
        {
            unresolved_risks.push(
                "No authoritative preview exists for the selected checkpoint; regenerate it."
                    .to_owned(),
            );
        }
        if matches!(request.operation, RecoveryOperation::RestoreCheckpoint)
            && !checkpoint_integrity_verified
        {
            unresolved_risks.push(
                "The selected checkpoint is missing or failed integrity verification.".to_owned(),
            );
        }
        let repository_preconditions_verified = match request.operation {
            RecoveryOperation::RestoreCheckpoint => matching_preview.is_some_and(|preview| {
                preview.repository_matches_checkpoint_base
                    && checkpoint_integrity_verified
                    && conflicting_uncommitted_paths.is_empty()
                    && unresolved_risks.is_empty()
            }),
            _ => !view.current_repository_fingerprint.is_empty(),
        };
        let evidence = RecoveryPreflightEvidence {
            repository_fingerprint_before: view.current_repository_fingerprint.clone(),
            checkpoint_integrity_verified,
            repository_preconditions_verified,
            conflicting_uncommitted_paths,
            unresolved_risks,
        };
        Ok((view, evidence))
    }

    pub(crate) fn execute_action(
        repo: &Path,
        view: &RecoveryView,
        request: &RecoveryActionRequest,
        preflight: RecoveryPreflightEvidence,
    ) -> Result<RecoveryExecutionReceipt, String> {
        let executor = RuntimeRecoveryExecutor {
            repository_fingerprint: preflight.repository_fingerprint_before.clone(),
        };
        let mut service = RecoveryActionService::new(executor);
        service
            .execute_and_audit(repo, view, request, preflight, now_unix_ms())
            .map_err(|error| error.to_string())
    }

    fn now_unix_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_millis()).ok())
            .unwrap_or_default()
    }

    pub(crate) fn startup_events(repo: &Path) -> Vec<RuntimeEvent> {
        discover(repo)
            .into_iter()
            .map(RuntimeEvent::RecoveryAvailable)
            .collect()
    }

    pub(crate) fn discover(repo: &Path) -> Vec<RecoveryView> {
        let directory = repo.join(RECOVERY_DIRECTORY);
        let Ok(entries) = fs::read_dir(&directory) else {
            return Vec::new();
        };

        let mut paths = entries
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
            .map(|path| match fs::read_to_string(&path) {
                Ok(contents) => match serde_json::from_str::<PersistedRecoveryRecord>(&contents) {
                    Ok(record) => RecoveryView::build(RecoveryViewInput {
                        session_id: record.session_id,
                        last_durable_step: record.last_durable_step,
                        interrupted_operation: record.interrupted_operation,
                        current_repository_fingerprint: record.current_repository_fingerprint,
                        verification: record.verification,
                        approvals_must_be_reestablished: record.approvals_must_be_reestablished,
                        containment_must_be_reestablished: record.containment_must_be_reestablished,
                        checkpoints: record.checkpoints,
                        selected_preview: record.selected_preview,
                        source_corrupt: false,
                    }),
                    Err(_) => corrupt_view(&path),
                },
                Err(_) => corrupt_view(&path),
            })
            .collect()
    }

    fn corrupt_view(path: &Path) -> RecoveryView {
        let session_id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("unknown-recovery-record")
            .to_owned();
        RecoveryView::build(RecoveryViewInput {
            session_id,
            last_durable_step: "Unknown because the recovery record could not be read".to_owned(),
            interrupted_operation: None,
            current_repository_fingerprint: String::new(),
            verification: VerificationState::Unknown,
            approvals_must_be_reestablished: true,
            containment_must_be_reestablished: true,
            checkpoints: Vec::new(),
            selected_preview: None,
            source_corrupt: true,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use medusa_recovery_coordinator::{CheckpointPresentation, RecoveryHealth};
        use tempfile::tempdir;

        fn checkpoint() -> CheckpointPresentation {
            CheckpointPresentation {
                id: "checkpoint-1".to_owned(),
                sequence: 1,
                created_at_unix_ms: 1_700_000_000_000,
                task_step: "implement".to_owned(),
                reason: "durable progress".to_owned(),
                repository_fingerprint: "a".repeat(64),
                verification: VerificationState::Incomplete,
                provenance: "execution-checkpoint/v1".to_owned(),
                integrity_verified: true,
            }
        }

        fn write_record(repo: &Path, preview: Option<RecoveryPreview>) {
            let directory = repo.join(RECOVERY_DIRECTORY);
            fs::create_dir_all(&directory).expect("create recovery directory");
            let record = serde_json::json!({
                "session_id": "session-a",
                "last_durable_step": "implement",
                "interrupted_operation": "cargo test",
                "current_repository_fingerprint": "b".repeat(64),
                "verification": "Incomplete",
                "approvals_must_be_reestablished": true,
                "containment_must_be_reestablished": true,
                "checkpoints": [checkpoint()],
                "selected_preview": preview
            });
            fs::write(
                directory.join("a.json"),
                serde_json::to_vec_pretty(&record).expect("serialize recovery record"),
            )
            .expect("write recovery record");
        }

        #[test]
        fn discovers_recovery_records_in_stable_filename_order() {
            let repo = tempdir().expect("temporary repository");
            let directory = repo.path().join(RECOVERY_DIRECTORY);
            fs::create_dir_all(&directory).expect("create recovery directory");
            for (name, session_id) in [("b.json", "session-b"), ("a.json", "session-a")] {
                let record = serde_json::json!({
                    "session_id": session_id,
                    "last_durable_step": "implement",
                    "interrupted_operation": "cargo test",
                    "current_repository_fingerprint": "b".repeat(64),
                    "verification": "Incomplete",
                    "approvals_must_be_reestablished": true,
                    "containment_must_be_reestablished": true,
                    "checkpoints": [checkpoint()],
                    "selected_preview": null
                });
                fs::write(
                    directory.join(name),
                    serde_json::to_vec_pretty(&record).expect("serialize recovery record"),
                )
                .expect("write recovery record");
            }

            let views = discover(repo.path());
            assert_eq!(views.len(), 2);
            assert_eq!(views[0].session_id, "session-a");
            assert_eq!(views[1].session_id, "session-b");
            assert!(
                views
                    .iter()
                    .all(|view| view.approvals_must_be_reestablished)
            );
        }

        #[test]
        fn action_context_reloads_authoritative_view_and_fails_closed_without_preview() {
            let repo = tempdir().expect("temporary repository");
            write_record(repo.path(), None);
            let request = RecoveryActionRequest {
                session_id: "session-a".to_owned(),
                operation: RecoveryOperation::RestoreCheckpoint,
                checkpoint_id: Some("checkpoint-1".to_owned()),
                confirmed_destructive_effects: true,
            };
            let (view, evidence) = action_context(repo.path(), &request).expect("action context");
            assert_eq!(view.session_id, "session-a");
            assert!(!evidence.repository_preconditions_verified);
            assert!(!evidence.unresolved_risks.is_empty());
        }

        #[test]
        fn missing_or_stale_session_is_rejected() {
            let repo = tempdir().expect("temporary repository");
            let request = RecoveryActionRequest {
                session_id: "missing".to_owned(),
                operation: RecoveryOperation::Inspect,
                checkpoint_id: None,
                confirmed_destructive_effects: false,
            };
            assert!(action_context(repo.path(), &request).is_err());
        }

        #[test]
        fn corrupt_records_are_visible_and_fail_closed() {
            let repo = tempdir().expect("temporary repository");
            let directory = repo.path().join(RECOVERY_DIRECTORY);
            fs::create_dir_all(&directory).expect("create recovery directory");
            fs::write(directory.join("broken.json"), b"{not json")
                .expect("write corrupt recovery record");

            let views = discover(repo.path());
            assert_eq!(views.len(), 1);
            assert_eq!(views[0].session_id, "broken");
            assert_eq!(views[0].health, RecoveryHealth::Corrupt);
            assert!(views[0].containment_must_be_reestablished);
        }

        #[test]
        fn missing_recovery_directory_is_not_an_error() {
            let repo = tempdir().expect("temporary repository");
            assert!(discover(repo.path()).is_empty());
        }
    }
}

#[rustfmt::skip]
mod production_orchestrator;

/// Production task-contract and schedule definitions used by the runtime coordinator.
///
/// The shipped coordinated path is `RuntimeController -> run_prompt ->
/// multi_agent_coordinator::run_preflight -> read-only AgentEngine teammates -> parent AgentEngine`.
pub mod orchestration_planning {
    pub use super::production_orchestrator::*;
}
