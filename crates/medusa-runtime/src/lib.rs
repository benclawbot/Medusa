use std::{
    collections::{BTreeMap, VecDeque},
    env,
    path::{Path, PathBuf},
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
use medusa_config::{Config, ConfigurationChanged, Mode, ProviderProfileStore};
use medusa_protocol::{Actor, EventPayload};
use medusa_provider::{
    ConfiguredProvider, ImageSource, Message, MessageBlock, ModelProvider, ProviderExecutionPhase,
    Role,
};

use crate::{
    commands::{
        Effort, LearningCommand, ModelCommand, ModelConfiguration, ReviewCommand, SlashCommand,
    },
    invariants::{RuntimeInvariantContext, RuntimeInvariantRegistry, RuntimeInvariantRegistryError},
    prompt::PromptDraft,
};

pub mod attachment;
pub mod analysis_workspace;
pub mod analysis_contained;
pub mod component_runtime;
mod analysis_tool;
pub mod checkpoint_payload;
pub mod checkpoint_store;
mod command_router;
mod coding_trajectory;
mod delegation_contract;
pub mod commands;
mod config_command;
mod error;
pub mod execution_history;
pub mod frontend;
pub mod invariants;
pub mod learning_retrieval;
mod learning_authority;
pub mod learning_review;
mod multi_agent_coordinator;
mod mutating_worker_coordinator;
mod repository_context;
mod mutation_transaction;
pub mod openai_realtime;
pub mod observer;
pub mod prompt;
pub mod review;
pub mod wakeup_action_bridge;
pub mod scheduled_actions;
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
pub use observer::{
    ObservationMessage, ObservationStage, ObservationVerification, ObservedPlanStep,
    SessionObservationSnapshot, SideQuestionCancelToken, SideQuestionRequest, SideQuestionResponse,
    answer_side_question, observe_session,
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
use command_router::execute_slash_command_with_submission;

#[cfg(test)]
use command_router::execute_slash_command;

#[derive(Debug)]
pub enum RuntimeCommand {
    Submit {
        draft: PromptDraft,
        accepted: Sender<Result<(), String>>,
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
    invariants: Arc<Mutex<RuntimeInvariantRegistry>>,
}

impl RuntimeController {
    pub fn start(repo: PathBuf) -> Self {
        match RuntimeState::load(repo.clone()) {
            Ok(state) => Self::start_with_state(state),
            Err(error) => Self::failed_start(error),
        }
    }

    pub fn start_with_config(repo: PathBuf, config: Config) -> Self {
        match RuntimeState::from_config_with_runtime(repo, config) {
            Ok(state) => Self::start_with_state(state),
            Err(error) => Self::failed_start(error),
        }
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
            invariants: Arc::new(Mutex::new(RuntimeInvariantRegistry::default())),
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
            invariants: Arc::new(Mutex::new(RuntimeInvariantRegistry::default())),
        }
    }

    /// Registers a trusted in-process runtime check. Managed plugins cannot add checks through
    /// this API; plugin metadata must still pass through the capability and policy authorities.
    pub fn register_runtime_invariant<F>(
        &self,
        id: impl Into<String>,
        check: F,
    ) -> Result<(), RuntimeInvariantRegistryError>
    where
        F: Fn(&RuntimeInvariantContext) -> Result<(), String> + Send + Sync + 'static,
    {
        lock_runtime_invariants(&self.invariants).register(id, check)
    }

    #[must_use]
    pub fn remove_runtime_invariant(&self, id: &str) -> bool {
        lock_runtime_invariants(&self.invariants).remove(id)
    }

    #[must_use]
    pub fn runtime_invariant_ids(&self) -> Vec<String> {
        lock_runtime_invariants(&self.invariants)
            .ids()
            .map(str::to_owned)
            .collect()
    }

    fn check_runtime_invariants(&self, operation: &str) -> Result<(), RuntimeError> {
        let submission = lock_submission(&self.submission);
        let context = RuntimeInvariantContext::new(
            operation,
            self.repo.clone(),
            submission.busy,
            submission.active_session_id.clone(),
        );
        let violations = lock_runtime_invariants(&self.invariants).validate(&context);
        if violations.is_empty() {
            return Ok(());
        }
        let details = violations
            .into_iter()
            .map(|violation| format!("{}: {}", violation.id, violation.reason))
            .collect::<Vec<_>>()
            .join("; ");
        Err(RuntimeError::InvalidCommand(format!(
            "runtime invariant check failed before {operation}: {details}"
        )))
    }

    pub fn submit(&self, draft: PromptDraft) -> Result<SubmitDisposition, RuntimeError> {
        self.check_runtime_invariants("submit")?;
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
        match accepted_rx.recv() {
            Ok(Ok(())) => Ok(SubmitDisposition::Started),
            Ok(Err(error)) => {
                mark_idle(&self.submission, true);
                Err(RuntimeError::agent(format!(
                    "runtime failed before a durable session accepted the submission: {error}"
                )))
            }
            Err(_) => {
                mark_idle(&self.submission, true);
                Err(RuntimeError::agent(
                    "runtime worker stopped before a durable session accepted the submission",
                ))
            }
        }
    }

    pub fn run_command(&self, command: SlashCommand) -> Result<(), RuntimeError> {
        self.check_runtime_invariants("slash-command")?;
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
        self.check_runtime_invariants("configure-model")?;
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
        self.check_runtime_invariants("recovery")?;
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

    /// Returns the cancellation flag used by the active worker.
    ///
    /// Frontend hosts use this only for process-level emergency shutdown. Normal
    /// user cancellation must continue through [`Self::cancel`] so that the
    /// durable cancellation record is written first.
    #[must_use]
    pub fn cancellation_token(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
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
    if let EventPayload::CheckpointRestoreRequested { checkpoint_id, .. } = &payload
        && let Ok(checkpoints) = crate::checkpoint_store::list(repo, session_id)
        && let Some(target) = checkpoints
            .iter()
            .find(|record| record.checkpoint.fingerprint == *checkpoint_id)
    {
        // This artifact is advisory only. A summary failure must never block exact restore.
        let _ = medusa_agent::capture_restore_abandonment(
            &mut session,
            checkpoint_id,
            target.journal_cursor,
        );
    }
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

fn lock_runtime_invariants(
    invariants: &Mutex<RuntimeInvariantRegistry>,
) -> std::sync::MutexGuard<'_, RuntimeInvariantRegistry> {
    match invariants.lock() {
        Ok(registry) => registry,
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
                    Some(&accepted),
                );
                let event = match outcome {
                    Ok(completed) => completed,
                    Err(error) => {
                        let _ = accepted.send(Err(error.to_string()));
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
    let plugin_root = repo.join(".medusa/plugins");
    let registry = if plugin_root.is_dir() {
        CapabilityRegistry::discover_with_plugins(repo.clone(), &plugin_root)
    } else {
        CapabilityRegistry::discover(repo)
    };
    match registry {
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
    runtime_config_fingerprint: Option<String>,
    runtime_config_binding: Option<(u16, String, serde_json::Value)>,
    effort: Effort,
    plan_mode: bool,
    team_control: TeamControlPlane,
    codex_app_server: Option<openai_oauth::CodexAppServer>,
}

impl RuntimeState {
    fn load(repo: PathBuf) -> Result<Self, RuntimeError> {
        let project = repo.join(".medusa/config.toml");
        let project = project.exists().then_some(project);
        let config =
            Config::load_layers(None, project.as_deref(), &BTreeMap::new(), &BTreeMap::new())
                .map_err(RuntimeError::agent)?;
        Self::from_config_with_runtime(repo, config)
    }

    fn from_config_with_runtime(
        repo: PathBuf,
        mut config: Config,
    ) -> Result<Self, RuntimeError> {
        let effective = runtime_config_effective_for_repo(&repo, &config)?;
        apply_runtime_route(&mut config, &effective)?;
        let mut state = Self::from_config(repo, config);
        let binding = runtime_config_binding_from_effective(effective)?;
        state.runtime_config_fingerprint = Some(binding.1.clone());
        state.runtime_config_binding = Some(binding);
        Ok(state)
    }

    fn from_config(repo: PathBuf, config: Config) -> Self {
        let runtime_config_binding = runtime_config_binding(&config);
        let runtime_config_fingerprint = runtime_config_binding
            .as_ref()
            .map(|(_, fingerprint, _)| fingerprint.clone());
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
            runtime_config_fingerprint,
            runtime_config_binding,
            team_control: TeamControlPlane::default(),
            codex_app_server: None,
        }
    }

    fn settings_event(&self) -> RuntimeEvent {
        // A route configured with auth=none is ready without a Medusa-managed
        // API key. This includes ChatGPT OAuth, whose credential store is owned
        // by the Codex app-server rather than this runtime.
        let credential_configured = self.config.model.auth == "none"
            || self.session_api_key.is_some()
            || credential_environment(&self.config.model.provider)
                .is_some_and(|name| env::var(name).is_ok());
        RuntimeEvent::Settings {
            model: format!(
                "{} / {}",
                self.config.model.provider, self.config.model.name
            ),
            effort: format!("effort:{}", self.effort.label()),
            plan_mode: self.plan_mode,
            credential_configured,
            context_window_tokens: self.config.model.context_window_tokens,
            auto_compact_percent: self.config.model.auto_compact_percent,
        }
    }
}

fn runtime_config_binding(config: &Config) -> Option<(u16, String, serde_json::Value)> {
    let loop_config = crate::runtime_config::RuntimeLoopConfigV1 {
        provider: Some(config.model.provider.clone()),
        model: Some(config.model.name.clone()),
        ..crate::runtime_config::RuntimeLoopConfigV1::default()
    };
    crate::runtime_config::compile_effective_config(
        loop_config,
        BTreeMap::from([
            ("provider".to_owned(), "resolved_model_config".to_owned()),
            ("model".to_owned(), "resolved_model_config".to_owned()),
        ]),
        crate::runtime_config::RuntimeConfigHardLimits::default(),
        true,
    )
    .ok()
    .and_then(|effective| {
        let fingerprint = effective.fingerprint.clone();
        serde_json::to_value(&effective)
            .ok()
            .map(|snapshot| (effective.schema_version, fingerprint, snapshot))
    })
}

fn runtime_config_binding_for_repo(
    repo: &std::path::Path,
    config: &Config,
) -> Result<(u16, String, serde_json::Value), RuntimeError> {
    let effective = runtime_config_effective_for_repo(repo, config)?;
    runtime_config_binding_from_effective(effective)
}

fn runtime_config_effective_for_repo(
    repo: &std::path::Path,
    config: &Config,
) -> Result<crate::runtime_config::EffectiveRuntimeConfigV1, RuntimeError> {
    let loop_config = crate::runtime_config::RuntimeLoopConfigV1 {
        provider: Some(config.model.provider.clone()),
        model: Some(config.model.name.clone()),
        ..crate::runtime_config::RuntimeLoopConfigV1::default()
    };
    let user_path = ProviderProfileStore::user()
        .ok()
        .and_then(|store| store.path().parent().map(|path| path.join("runtime.toml")));
    let repository_path = repo.join(".medusa/runtime.toml");
    let code_mode_ready = medusa_agent::tools::ToolManager::new(Default::default())
        .code_mode_sdk(repo, config.agent.mode == Mode::ReadOnly)
        .is_ok();
    let effective = crate::runtime_config::compile_layered_config(
        loop_config,
        user_path.as_deref(),
        Some(repository_path.as_path()),
        None,
        None,
        crate::runtime_config::RuntimeConfigHardLimits::default(),
        code_mode_ready,
    )
    .map_err(|errors| RuntimeError::agent(errors.join("; ")))?;
    if let (Some(provider), Some(model)) = (&effective.config.provider, &effective.config.model)
        && !is_admitted_runtime_route(config, provider, model)
    {
        return Err(RuntimeError::agent(format!(
            "runtime configuration selected {provider}/{model}, which is not an admitted provider/model route"
        )));
    }
    Ok(effective)
}

fn runtime_config_binding_from_effective(
    effective: crate::runtime_config::EffectiveRuntimeConfigV1,
) -> Result<(u16, String, serde_json::Value), RuntimeError> {
    let fingerprint = effective.fingerprint.clone();
    let snapshot = serde_json::to_value(&effective).map_err(RuntimeError::agent)?;
    Ok((effective.schema_version, fingerprint, snapshot))
}

fn is_admitted_runtime_route(config: &Config, provider: &str, model: &str) -> bool {
    (config.model.provider == provider && config.model.name == model)
        || config
            .model
            .fallback_providers
            .iter()
            .any(|route| route.provider == provider && route.name == model)
}

fn apply_runtime_route(
    config: &mut Config,
    effective: &crate::runtime_config::EffectiveRuntimeConfigV1,
) -> Result<(), RuntimeError> {
    let Some(provider) = effective.config.provider.as_deref() else {
        return Ok(());
    };
    let Some(model) = effective.config.model.as_deref() else {
        return Err(RuntimeError::agent(
            "runtime configuration provider/model selection is incomplete",
        ));
    };
    if config.model.provider == provider && config.model.name == model {
        return Ok(());
    }
    let Some(route) = config
        .model
        .fallback_providers
        .iter()
        .find(|route| route.provider == provider && route.name == model)
        .cloned()
    else {
        return Err(RuntimeError::agent(format!(
            "runtime configuration selected {provider}/{model}, which is not an admitted provider/model route"
        )));
    };
    config.model.provider = route.provider;
    config.model.name = route.name;
    config.model.protocol = route.protocol;
    config.model.base_url = route.base_url;
    config.model.auth = route.auth;
    config.model.tool_calling = route.tool_calling;
    config.model.streaming = route.streaming;
    config.model.max_retries = route.max_retries;
    config.model.retry_base_delay_ms = route.retry_base_delay_ms;
    config.model.retry_max_delay_ms = route.retry_max_delay_ms;
    config.model.retry_jitter_ms = route.retry_jitter_ms;
    Ok(())
}

fn session_runtime_config_binding(
    session: &AgentSession,
) -> Option<(u16, String, serde_json::Value)> {
    session.events.iter().find_map(|event| match &event.payload {
        EventPayload::RuntimeConfigurationBound {
            schema_version,
            fingerprint,
            snapshot,
        } => Some((*schema_version, fingerprint.clone(), snapshot.clone())),
        _ => None,
    })
}

fn validate_session_runtime_config_binding(
    current: Option<&(u16, String, serde_json::Value)>,
    persisted: Option<&(u16, String, serde_json::Value)>,
) -> Result<(), RuntimeError> {
    let Some(persisted) = persisted else {
        return Ok(());
    };
    let Some(current) = current else {
        return Err(RuntimeError::agent(
            "active session has a runtime configuration binding but the current runtime could not compile one",
        ));
    };
    if current.0 != persisted.0 || current.1 != persisted.1 {
        return Err(RuntimeError::agent(
            "active session is bound to a different runtime configuration; start a new session",
        ));
    }
    Ok(())
}

fn bound_model(snapshot: &serde_json::Value) -> Option<(&str, &str)> {
    let config = snapshot.get("config")?;
    Some((
        config.get("provider")?.as_str()?,
        config.get("model")?.as_str()?,
    ))
}

const GENERAL_CHAT_TURN_INSTRUCTION: &str = "General conversation mode: answer the user's request directly in this turn. Do not inspect the repository, create a plan, or call coding, file, shell, or desktop tools unless the user explicitly asks for repository work. Use web tools only when current or source-linked information is actually needed. A clear text answer is complete; do not invent follow-up work.";

fn is_general_chat_request(text: &str, attachment_count: usize) -> bool {
    if attachment_count != 0 {
        return false;
    }
    let normalized = text.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    [
        "implement", "fix", "modify", "refactor", "edit", "write code", "codebase",
        "repository", "repo", "file", "src/", "test", "bug", "crash", "compile",
        "build a website", "webpage", "component", "function", "pull request", "commit",
        "push changes",
    ]
    .iter()
    .all(|marker| !normalized.contains(marker))
}

fn should_capture_review_baseline_for_plan(
    general_chat: bool,
    resuming_pending_question: bool,
    repository_work: bool,
) -> bool {
    !general_chat && !resuming_pending_question && repository_work
}

fn execution_plan_for_prompt(
    repo: &Path,
    draft: &PromptDraft,
    general_chat: bool,
) -> Result<crate::production_orchestrator::ProductionExecutionPlan, RuntimeError> {
    let plan = if general_chat {
        crate::production_orchestrator::plan_for_general_chat(draft)
    } else {
        crate::production_orchestrator::plan_for_repository(repo, draft)
    };
    plan.map_err(RuntimeError::agent)
}

fn oauth_input_from_content(content: &[MessageBlock]) -> Result<Vec<serde_json::Value>, RuntimeError> {
    let mut input = Vec::new();
    for block in content {
        match block {
            MessageBlock::Text { text } => {
                if !text.is_empty() {
                    input.push(serde_json::json!({"type": "text", "text": text}));
                }
            }
            MessageBlock::Image {
                source: ImageSource::Base64 { media_type, data },
                ..
            } => input.push(serde_json::json!({
                "type": "image",
                "url": format!("data:{media_type};base64,{data}")
            })),
            MessageBlock::Image {
                source: ImageSource::AttachmentRef { .. },
                ..
            } => {
                return Err(RuntimeError::agent(
                    "ChatGPT OAuth app-server turns require encoded image attachments",
                ));
            }
            MessageBlock::ToolUse { .. } | MessageBlock::ToolResult { .. } => {
                return Err(RuntimeError::agent(
                    "ChatGPT OAuth app-server input cannot contain tool transcript blocks",
                ));
            }
        }
    }
    if input.is_empty() {
        return Err(RuntimeError::EmptyPrompt);
    }
    Ok(input)
}

fn oauth_content_text(content: &[MessageBlock]) -> String {
    let text = content
        .iter()
        .filter_map(|block| match block {
            MessageBlock::Text { text } => Some(text.as_str()),
            MessageBlock::Image { .. } => Some("[image attachment]"),
            MessageBlock::ToolUse { .. } | MessageBlock::ToolResult { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    text.trim().to_owned()
}

fn append_oauth_answer(
    session: &mut AgentSession,
    content: Vec<MessageBlock>,
) -> Result<(), RuntimeError> {
    let question = session.pending_question.take().ok_or_else(|| {
        RuntimeError::agent("there is no pending OAuth app-server question to answer")
    })?;
    let answer = oauth_content_text(&content);
    if answer.is_empty() {
        session.pending_question = Some(question);
        return Err(RuntimeError::EmptyPrompt);
    }
    session.completed = false;
    session.turn = 0;
    session.messages.push(Message {
        role: Role::User,
        content,
    });
    medusa_agent::record_session_event(
        session,
        Actor::User,
        EventPayload::ApprovalDecisionRecorded {
            decision: serde_json::json!({"answer": answer}),
        },
    )
    .map_err(RuntimeError::agent)?;
    medusa_agent::record_session_event(
        session,
        Actor::User,
        EventPayload::UserPromptReceived { text: answer },
    )
    .map_err(RuntimeError::agent)?;
    medusa_agent::record_session_event(session, Actor::Coordinator, EventPayload::SessionResumed)
        .map_err(RuntimeError::agent)?;
    Ok(())
}

fn latest_oauth_request_id(session: &AgentSession) -> Option<String> {
    session.events.iter().rev().find_map(|event| match &event.payload {
        EventPayload::ModelRequestStarted { request_id, .. } => request_id.clone(),
        _ => None,
    })
}

fn oauth_question(
    pending: &openai_oauth::PendingServerRequest,
) -> Result<AgentQuestion, RuntimeError> {
    let params = &pending.params;
    let item_id = params
        .get("itemId")
        .or_else(|| params.get("item_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let option = |label: &str, description: &str| AgentQuestionOption {
        label: label.to_owned(),
        description: description.to_owned(),
    };
    let questions = match pending.method.as_str() {
        "item/commandExecution/requestApproval" => {
            let command = params
                .get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("the requested Codex command");
            vec![AgentQuestionItem {
                header: "Codex command".to_owned(),
                question: format!(
                    "Allow Codex to run `{}`?",
                    command.chars().take(300).collect::<String>()
                ),
                options: vec![
                    option("Approve", "Run this exact command."),
                    option("Approve for session", "Allow matching commands for this session."),
                    option("Decline", "Do not run the command."),
                ],
                multi_select: false,
            }]
        }
        "item/fileChange/requestApproval" => {
            let root = params
                .get("grantRoot")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("the requested files");
            vec![AgentQuestionItem {
                header: "Codex file change".to_owned(),
                question: format!("Allow Codex to change `{root}`?"),
                options: vec![
                    option("Approve", "Apply this exact file change."),
                    option("Approve for session", "Allow matching changes for this session."),
                    option("Decline", "Do not apply the change."),
                ],
                multi_select: false,
            }]
        }
        "item/permissions/requestApproval" => {
            let reason = params
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Codex requested additional permissions");
            vec![AgentQuestionItem {
                header: "Codex permissions".to_owned(),
                question: reason.to_owned(),
                options: vec![
                    option("Approve", "Grant the requested permissions for this turn."),
                    option("Approve for session", "Grant the requested permissions for this session."),
                    option("Decline", "Do not grant additional permissions."),
                ],
                multi_select: false,
            }]
        }
        "item/tool/requestUserInput" => params
            .get("questions")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .map(|question| {
                let options = question
                    .get("options")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|option| AgentQuestionOption {
                        label: option
                            .get("label")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("Option")
                            .to_owned(),
                        description: option
                            .get("description")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    })
                    .collect();
                AgentQuestionItem {
                    header: question
                        .get("header")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("Codex question")
                        .to_owned(),
                    question: question
                        .get("question")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("Codex requested input")
                        .to_owned(),
                    options,
                    multi_select: false,
                }
            })
            .collect(),
        "mcpServer/elicitation/request" => vec![AgentQuestionItem {
            header: "Codex integration".to_owned(),
            question: params
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Codex requested confirmation")
                .to_owned(),
            options: vec![
                option("Approve", "Accept the requested integration action."),
                option("Decline", "Decline the requested integration action."),
            ],
            multi_select: false,
        }],
        _ => Vec::new(),
    };
    serde_json::from_value(serde_json::json!({
        "tool_use_id": item_id,
        "questions": questions
    }))
    .map_err(RuntimeError::agent)
}

fn oauth_activity(event: &openai_oauth::CodexTurnEvent) -> Option<RuntimeActivity> {
    let openai_oauth::CodexTurnEvent::Activity { method, params } = event else {
        return None;
    };
    let item = params.get("item").unwrap_or(params);
    let item_type = item
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Codex activity");
    if item_type == "agentMessage" || item_type == "userMessage" {
        return None;
    }
    let title = match item_type {
        "commandExecution" => "Codex command".to_owned(),
        "fileChange" => "Codex file change".to_owned(),
        "mcpToolCall" => "Codex integration".to_owned(),
        other => other.to_owned(),
    };
    Some(RuntimeActivity {
        id: item
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        kind: if method.ends_with("completed") {
            RuntimeActivityKind::Done
        } else {
            RuntimeActivityKind::Tool
        },
        title,
        details: Vec::new(),
    })
}

fn oauth_usage(value: Option<&serde_json::Value>, turn: u32, duration_ms: u64) -> TurnUsage {
    let source = value
        .and_then(|value| value.get("last").or(Some(value)))
        .unwrap_or(&serde_json::Value::Null);
    let number = |snake: &str, camel: &str| {
        source
            .get(snake)
            .or_else(|| source.get(camel))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default()
    };
    let input_tokens = number("input_tokens", "inputTokens");
    let output_tokens = number("output_tokens", "outputTokens");
    let cache_read_input_tokens = number("cache_read_input_tokens", "cachedInputTokens");
    let cache_creation_input_tokens = number("cache_creation_input_tokens", "cacheWriteInputTokens");
    let total_tokens = number("total_tokens", "totalTokens").max(
        input_tokens
            .saturating_add(output_tokens)
            .saturating_add(cache_read_input_tokens)
            .saturating_add(cache_creation_input_tokens),
    );
    TurnUsage {
        turn,
        input_tokens,
        output_tokens,
        cache_read_input_tokens,
        cache_creation_input_tokens,
        total_tokens,
        duration_ms,
        tokens_per_second_milli: if duration_ms == 0 {
            0
        } else {
            total_tokens.saturating_mul(1_000_000) / duration_ms
        },
        estimated_cost_microusd: 0,
        provenance: if value.is_some() {
            medusa_agent::UsageProvenance::ProviderReported
        } else {
            medusa_agent::UsageProvenance::Estimated
        },
    }
}

fn oauth_sandbox_policy(mode: Mode) -> (&'static str, &'static str) {
    match mode {
        Mode::ReadOnly => ("never", "readOnly"),
        Mode::Review | Mode::Yolo => ("on-request", "workspaceWrite"),
    }
}

fn run_openai_oauth_prompt(
    state: &mut RuntimeState,
    config: Config,
    draft: PromptDraft,
    events: &Sender<RuntimeEvent>,
    cancel: &Arc<AtomicBool>,
    submission: &Arc<Mutex<SubmissionState>>,
    accepted: Option<&Sender<Result<(), String>>>,
) -> Result<RuntimeEvent, RuntimeError> {
    let content = message_blocks(&draft)?;
    let pending_answer = state
        .session
        .as_ref()
        .is_some_and(|session| session.pending_question.is_some());
    if pending_answer && state.codex_app_server.is_none() {
        return Err(RuntimeError::agent(
            "a Codex approval is pending, but the app-server process is no longer available; restart the session and review the request again",
        ));
    }
    let provider = ConfiguredProvider::manager_from_config(&config, state.session_api_key.clone())
        .map_err(RuntimeError::agent)?;
    let mut engine = AgentEngine::new_with_cancellation(provider, config.clone(), Arc::clone(cancel));
    if let Some((_, fingerprint, _)) = state
        .session
        .as_ref()
        .and_then(session_runtime_config_binding)
    {
        engine = engine.with_runtime_config_fingerprint(fingerprint);
    } else if let Some((schema_version, fingerprint, snapshot)) = state.runtime_config_binding.clone() {
        engine = engine.with_runtime_config_binding(schema_version, fingerprint, snapshot);
    } else {
        engine = engine.with_runtime_config_fingerprint(
            state
                .runtime_config_fingerprint
                .clone()
                .unwrap_or_else(|| "runtime-config-unavailable".to_owned()),
        );
    }
    let mut session = match state.session.take() {
        Some(mut session) => {
            if !pending_answer {
                if let Err(error) = engine
                    .append_user_message(&mut session, content.clone())
                    .map_err(RuntimeError::agent)
                {
                    state.session = Some(session);
                    return Err(error);
                }
            }
            session
        }
        None => {
            let objective = state
                .pending_goal
                .take()
                .unwrap_or_else(|| objective_for(&draft));
            engine
                .create_session_with_content(&state.repo, objective, content.clone())
                .map_err(RuntimeError::agent)?
        }
    };
    let mut server = if let Some(server) = state.codex_app_server.take() {
        server
    } else {
        match openai_oauth::CodexAppServer::connect() {
            Ok(server) => server,
            Err(error) => {
                state.session = Some(session);
                return Err(RuntimeError::agent(error));
            }
        }
    };
    let turn_setup = (|| -> Result<(String, String, String), RuntimeError> {
        if pending_answer {
            let answer = oauth_content_text(&content);
            let mut answered_session = session.clone();
            append_oauth_answer(&mut answered_session, content.clone())?;
            server
                .respond_pending_with_answer(&answer)
                .map_err(RuntimeError::agent)?;
            medusa_agent::persist_session(&answered_session).map_err(RuntimeError::agent)?;
            session = answered_session;
            let Some((thread_id, turn_id)) = server.active_turn() else {
                return Err(RuntimeError::agent(
                    "Codex app-server did not retain the active turn while resuming an approval",
                ));
            };
            let request_id = latest_oauth_request_id(&session)
                .unwrap_or_else(|| format!("codex-turn-{}", session.turn.saturating_add(1)));
            Ok((thread_id.to_owned(), turn_id.to_owned(), request_id))
        } else {
            server.ensure_authenticated().map_err(RuntimeError::agent)?;
            let (approval_policy, sandbox) = oauth_sandbox_policy(config.agent.mode);
            let thread_id = server
                .start_or_resume_thread(
                    &state.repo,
                    &config.model.name,
                    approval_policy,
                    if config.agent.mode == Mode::ReadOnly {
                        "read-only"
                    } else {
                        "workspace-write"
                    },
                    session.codex_thread_id.as_deref(),
                )
                .map_err(RuntimeError::agent)?;
            if session.codex_thread_id.as_deref() != Some(thread_id.as_str()) {
                session.codex_thread_id = Some(thread_id.clone());
                medusa_agent::persist_session(&session).map_err(RuntimeError::agent)?;
            }
            let input = oauth_input_from_content(&content)?;
            let request_id = format!("codex-turn-{}", session.turn.saturating_add(1));
            medusa_agent::record_session_event(
                &mut session,
                Actor::Coordinator,
                EventPayload::ModelRequestStarted {
                    provider: config.model.provider.clone(),
                    model: config.model.name.clone(),
                    request_id: Some(request_id.clone()),
                    request_fingerprint: None,
                    manifest_ref: None,
                    attempt_ordinal: 0,
                    parent_request_id: None,
                },
            )
            .map_err(RuntimeError::agent)?;
            let turn_id = server
                .start_turn(
                    &thread_id,
                    input,
                    &config.model.name,
                    state.effort.label(),
                    &state.repo,
                    sandbox,
                )
                .map_err(RuntimeError::agent)?;
            Ok((thread_id, turn_id, request_id))
        }
    })();
    let (thread_id, turn_id, request_id) = match turn_setup {
        Ok(setup) => setup,
        Err(error) => {
            state.session = Some(session);
            state.codex_app_server = Some(server);
            return Err(error);
        }
    };
    lock_submission(submission).active_session_id = Some(session.id.to_string());
    if let Some(accepted) = accepted {
        let _ = accepted.send(Ok(()));
    }
    let started_at = std::time::Instant::now();
    let mut assistant_text = String::new();
    let mut reported_usage = None;
    let mut interrupt_sent = false;
    loop {
        if cancel.load(Ordering::SeqCst) && !interrupt_sent {
            server.interrupt(&thread_id, &turn_id).map_err(RuntimeError::agent)?;
            interrupt_sent = true;
        }
        let turn_event = match server
            .next_turn_event_with_cancel((!interrupt_sent).then_some(cancel.as_ref()))
            .map_err(RuntimeError::agent)
        {
            Ok(event) => event,
            Err(error) => {
                state.session = Some(session);
                state.codex_app_server = Some(server);
                mark_idle(submission, true);
                return Err(error);
            }
        };
        match turn_event {
            openai_oauth::CodexTurnEvent::AssistantDelta(delta) => {
                assistant_text.push_str(&delta);
                let _ = events.send(RuntimeEvent::AssistantText(delta));
            }
            event @ openai_oauth::CodexTurnEvent::Activity { .. } => {
                if let Some(activity) = oauth_activity(&event) {
                    let _ = events.send(RuntimeEvent::Activity(activity));
                }
            }
            openai_oauth::CodexTurnEvent::Plan(value) => {
                if let Ok(plan) = serde_json::from_value::<Vec<AgentPlanStep>>(value) {
                    session.plan = plan.clone();
                    let _ = events.send(RuntimeEvent::Plan(plan));
                }
            }
            openai_oauth::CodexTurnEvent::Usage(value) => reported_usage = Some(value),
            openai_oauth::CodexTurnEvent::Approval(pending) => {
                let question_result = (|| -> Result<AgentQuestion, RuntimeError> {
                    let question = oauth_question(&pending)?;
                    session.pending_question = Some(question.clone());
                    medusa_agent::record_session_event(
                        &mut session,
                        Actor::Coordinator,
                        EventPayload::QuestionRequested {
                            question: serde_json::to_value(&question).map_err(RuntimeError::agent)?,
                        },
                    )
                    .map_err(RuntimeError::agent)?;
                    medusa_agent::record_session_event(
                        &mut session,
                        Actor::Coordinator,
                        EventPayload::ApprovalRequested {
                            request: serde_json::json!({
                                "method": pending.method,
                                "params": pending.params
                            }),
                        },
                    )
                    .map_err(RuntimeError::agent)?;
                    Ok(question)
                })();
                let question = match question_result {
                    Ok(question) => question,
                    Err(error) => {
                        state.session = Some(session);
                        state.codex_app_server = Some(server);
                        mark_idle(submission, true);
                        return Err(error);
                    }
                };
                state.session = Some(session);
                state.codex_app_server = Some(server);
                mark_idle(submission, false);
                return Ok(RuntimeEvent::Question(question));
            }
            openai_oauth::CodexTurnEvent::Completed(completion) => {
                let elapsed_ms = u64::try_from(started_at.elapsed().as_millis())
                    .unwrap_or(u64::MAX)
                    .max(1);
                let text = if completion.text.is_empty() {
                    assistant_text.clone()
                } else {
                    completion.text
                };
                let usage = oauth_usage(
                    completion.usage.as_ref().or(reported_usage.as_ref()),
                    session.turn.saturating_add(1),
                    elapsed_ms,
                );
                if !text.trim().is_empty() {
                    let message = Message {
                        role: Role::Assistant,
                        content: vec![MessageBlock::Text { text: text.clone() }],
                    };
                    session.messages.push(message.clone());
                    medusa_agent::record_session_event(
                        &mut session,
                        Actor::Coordinator,
                        EventPayload::AssistantMessageRecorded {
                            message: serde_json::to_value(&message).map_err(RuntimeError::agent)?,
                        },
                    )
                    .map_err(RuntimeError::agent)?;
                    if assistant_text.is_empty() {
                        let _ = events.send(RuntimeEvent::AssistantText(text));
                    }
                }
                if completion.status == "completed" {
                    session.turn = session.turn.saturating_add(1);
                    medusa_agent::record_session_event(
                        &mut session,
                        Actor::Coordinator,
                        EventPayload::ModelResponseReceived {
                            response_id: Some(turn_id.clone()),
                            usage: serde_json::to_value(&usage).map_err(RuntimeError::agent)?,
                            request_id: Some(request_id.clone()),
                            request_fingerprint: None,
                        },
                    )
                    .map_err(RuntimeError::agent)?;
                    medusa_agent::record_session_event(
                        &mut session,
                        Actor::Coordinator,
                        EventPayload::ProviderExecutionRecorded {
                            status: serde_json::json!({
                                "backend": "codex-app-server",
                                "status": completion.status
                            }),
                        },
                    )
                    .map_err(RuntimeError::agent)?;
                    let _ = events.send(RuntimeEvent::Usage {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cache_read_input_tokens: usage.cache_read_input_tokens,
                        cache_creation_input_tokens: usage.cache_creation_input_tokens,
                        total_tokens: usage.total_tokens,
                        duration_ms: usage.duration_ms,
                        tokens_per_second_milli: usage.tokens_per_second_milli,
                        estimated_cost_microusd: usage.estimated_cost_microusd,
                        provenance: usage.provenance,
                    });
                    if let Err(error) = medusa_agent::persist_session(&session).map_err(RuntimeError::agent) {
                        state.session = Some(session);
                        state.codex_app_server = Some(server);
                        return Err(error);
                    }
                    let queued = finish_or_take_followups(submission);
                    let mut queued = queued.into_iter();
                    if let Some(next) = queued.next() {
                        let remaining = queued.collect::<Vec<_>>();
                        if !remaining.is_empty() {
                            lock_submission(submission).followups.extend(remaining);
                        }
                        medusa_agent::record_session_event(
                            &mut session,
                            Actor::User,
                            EventPayload::UserFollowupDequeued {
                                command_id: next.command_id.clone(),
                                text: next.draft.text.clone(),
                            },
                        )
                        .map_err(RuntimeError::agent)?;
                        medusa_agent::persist_session(&session).map_err(RuntimeError::agent)?;
                        state.session = Some(session);
                        state.codex_app_server = Some(server);
                        return run_openai_oauth_prompt(
                            state,
                            config,
                            next.draft,
                            events,
                            cancel,
                            submission,
                            None,
                        );
                    }
                    state.session = Some(session);
                    state.codex_app_server = Some(server);
                    return Ok(RuntimeEvent::TurnFinished);
                }
                state.session = Some(session);
                state.codex_app_server = Some(server);
                if interrupt_sent || completion.status == "interrupted" {
                    mark_idle(submission, true);
                    return Ok(RuntimeEvent::Cancelled);
                }
                let message = completion
                    .error
                    .unwrap_or_else(|| "Codex app-server reported a failed turn".to_owned());
                mark_idle(submission, true);
                return Err(RuntimeError::agent(message));
            }
        }
    }
}

fn run_prompt(
    state: &mut RuntimeState,
    draft: PromptDraft,
    events: &Sender<RuntimeEvent>,
    cancel: &Arc<AtomicBool>,
    submission: &Arc<Mutex<SubmissionState>>,
    accepted: Option<&Sender<Result<(), String>>>,
) -> Result<RuntimeEvent, RuntimeError> {
    let config = state.config.clone();
    let session_binding = state
        .session
        .as_ref()
        .and_then(session_runtime_config_binding);
    validate_session_runtime_config_binding(
        state.runtime_config_binding.as_ref(),
        session_binding.as_ref(),
    )?;
    if let Some((_, _, snapshot)) = &session_binding
        && let Some((provider, model)) = bound_model(snapshot)
        && (provider != config.model.provider || model != config.model.name)
    {
        return Err(RuntimeError::agent(
            "active session is bound to a different provider/model configuration; start a new session",
        ));
    }
    if config.model.provider == medusa_config::openai_oauth::PROVIDER {
        return run_openai_oauth_prompt(
            state,
            config,
            draft,
            events,
            cancel,
            submission,
            accepted,
        );
    }
    let max_turns = config.agent.max_turns;
    let provider = ConfiguredProvider::manager_from_config(&config, state.session_api_key.clone())
        .map_err(RuntimeError::agent)?;
    let resuming_pending_question = state
        .session
        .as_ref()
        .is_some_and(|session| session.pending_question.is_some());
    let general_chat = is_general_chat_request(&draft.text, draft.attachments.len());
    let turn_instruction = general_chat.then_some(GENERAL_CHAT_TURN_INSTRUCTION);
    let selected_skill = state.pending_skill.clone();
    let execution_plan = execution_plan_for_prompt(&state.repo, &draft, general_chat)?;
    let repository_work = execution_plan.planning.intent
        != medusa_multi_agent_scheduler::PlanningIntent::Conversation;
    if should_capture_review_baseline_for_plan(
        general_chat,
        resuming_pending_question,
        repository_work,
    ) {
        crate::review::capture_review_baseline(&state.repo)
            .map_err(|error| RuntimeError::agent(error.to_string()))?;
    }
    let coordinated =
        execution_plan.mode == crate::production_orchestrator::ExecutionMode::Orchestrated;
    let analysis_host = Arc::new(crate::analysis_tool::RuntimeAnalysisHost::new(
        state.repo.clone(),
        config.clone(),
        state.session_api_key.clone(),
        state.team_control.clone(),
        events.clone(),
        Arc::clone(cancel),
    ));
    let mut engine = AgentEngine::new_with_cancellation(provider, config.clone(), Arc::clone(cancel))
        .with_general_chat(general_chat);
    if let Some((_, fingerprint, _)) = session_binding {
        engine = engine.with_runtime_config_fingerprint(fingerprint);
    } else if let Some((schema_version, fingerprint, snapshot)) = state.runtime_config_binding.clone() {
        engine = engine.with_runtime_config_binding(schema_version, fingerprint, snapshot);
    } else {
        engine = engine.with_runtime_config_fingerprint(
            state
                .runtime_config_fingerprint
                .clone()
                .unwrap_or_else(|| "runtime-config-unavailable".to_owned()),
        );
    }
    let engine = engine.with_analysis_workspace_host(analysis_host);
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
        let _ = accepted.send(Ok(()));
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
        let speculation_policy = medusa_multi_agent_scheduler::speculation::policy_for(
            &execution_plan.planning,
        )
        .map_err(RuntimeError::agent)?;
        let preflight = if crate::production_orchestrator::uses_deterministic_preflight(
            &execution_plan,
        ) {
            crate::multi_agent_coordinator::run_deterministic_fast_preflight(
                &state.repo,
                &config,
                &execution_plan,
                &state.team_control,
                events,
            )
        } else if speculation_policy.eligible {
            let speculative_control = TeamControlPlane::default();
            let (preflight, speculative) = std::thread::scope(|scope| {
                let speculative = scope.spawn(|| {
                    crate::mutating_worker_coordinator::run_speculative_implementation(
                        &state.repo,
                        &config,
                        state.session_api_key.clone(),
                        &execution_plan,
                        cancel,
                        (&speculative_control, events),
                    )
                });
                let preflight = crate::multi_agent_coordinator::run_preflight(
                    &state.repo,
                    &config,
                    state.session_api_key.clone(),
                    &execution_plan,
                    cancel,
                    &state.team_control,
                    events,
                );
                let speculative = speculative
                    .join()
                    .map_err(|_| "speculative implementer thread terminated unexpectedly".to_owned())
                    .and_then(|result| result);
                (preflight, speculative)
            });
            match speculative {
                Ok(crate::mutating_worker_coordinator::SpeculationPreparation::Prepared {
                    candidate,
                    turns,
                    elapsed_ms,
                }) => {
                    let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                        id: Some(execution_plan.fingerprint.clone()),
                        kind: RuntimeActivityKind::Progress,
                        title: "Speculative candidate awaiting promotion".to_owned(),
                        details: vec![
                            format!("candidate={candidate}"),
                            format!("turns={turns}"),
                            format!("overlapped_preflight_ms={elapsed_ms}"),
                        ],
                    }));
                }
                Ok(crate::mutating_worker_coordinator::SpeculationPreparation::Skipped {
                    reason,
                }) => {
                    let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                        id: Some(execution_plan.fingerprint.clone()),
                        kind: RuntimeActivityKind::Progress,
                        title: "Speculation skipped".to_owned(),
                        details: vec![reason],
                    }));
                }
                Ok(crate::mutating_worker_coordinator::SpeculationPreparation::Discarded {
                    reason,
                }) => {
                    let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                        id: Some(execution_plan.fingerprint.clone()),
                        kind: RuntimeActivityKind::Progress,
                        title: "Speculation discarded; cold path retained".to_owned(),
                        details: vec![reason],
                    }));
                }
                Err(error) => {
                    let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                        id: Some(execution_plan.fingerprint.clone()),
                        kind: RuntimeActivityKind::Progress,
                        title: "Speculation unavailable; cold path retained".to_owned(),
                        details: vec![error],
                    }));
                }
            }
            preflight
        } else {
            crate::multi_agent_coordinator::run_preflight(
                &state.repo,
                &config,
                state.session_api_key.clone(),
                &execution_plan,
                cancel,
                &state.team_control,
                events,
            )
        };
        match preflight {
            Ok(evidence) => {
                if let Some(ledger) = execution_ledger.as_mut() {
                    crate::production_orchestrator::succeed_kinds(
                        ledger,
                        &execution_plan,
                        &[
                            medusa_multi_agent_scheduler::TaskKind::Analysis,
                            medusa_multi_agent_scheduler::TaskKind::RiskReview,
                        ],
                        if crate::production_orchestrator::uses_deterministic_preflight(
                            &execution_plan,
                        ) {
                            "durable deterministic fast-lane evidence recorded"
                        } else {
                            "durable preflight worker evidence recorded"
                        },
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
            let first_implementation = crate::mutating_worker_coordinator::run_implementation(
                &state.repo,
                &config,
                state.session_api_key.clone(),
                &execution_plan,
                preflight,
                cancel,
                (&state.team_control, events),
            );
            let implementation = match first_implementation {
                Err(error)
                    if crate::mutating_worker_coordinator::is_speculation_invalidation(&error) =>
                {
                    let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                        id: Some(execution_plan.fingerprint.clone()),
                        kind: RuntimeActivityKind::Progress,
                        title: "Speculation invalidated; restarting authoritative cold path"
                            .to_owned(),
                        details: vec![error],
                    }));
                    crate::mutating_worker_coordinator::run_implementation(
                        &state.repo,
                        &config,
                        state.session_api_key.clone(),
                        &execution_plan,
                        preflight,
                        cancel,
                        (&state.team_control, events),
                    )
                }
                result => result,
            };
            match implementation {
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
    let trajectory_context = if general_chat {
        String::new()
    } else {
        let session = state.session.as_ref().ok_or_else(|| {
            RuntimeError::agent("runtime session disappeared before trajectory projection")
        })?;
        crate::coding_trajectory::sync_and_render(&state.repo, session, None)?
    };
    let mut task_context = vec![
        orchestration_context,
        tool_policy_context,
        verification_context,
        trajectory_context,
    ];
    if implementation_evidence.is_none()
        && let Some(evidence) = coordinator_evidence.as_ref()
    {
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
    if coordinated && implementation_evidence.is_none() {
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

    let result = if implementation_evidence.is_some() {
        Ok(RuntimeEvent::TurnFinished)
    } else {
        (|| {
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
            let mut provider_phase = match state.config.agent.mode {
                Mode::ReadOnly => ProviderExecutionPhase::Planning,
                Mode::Review => ProviderExecutionPhase::HighRiskReview,
                Mode::Yolo => ProviderExecutionPhase::Implementation,
            };
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
                let trajectory_context = if general_chat {
                    String::new()
                } else {
                    crate::coding_trajectory::sync_and_render(&state.repo, &session, None)?
                };
                let repository_context = if general_chat {
                    String::new()
                } else {
                    crate::repository_context::assemble_and_render(
                        &state.repo,
                        &session,
                        &draft.text,
                    )?
                };
                let turn_context = format!(
                    "{skill_context}\n\n{trajectory_context}\n\n{repository_context}"
                );
                match engine.step_with_observer_and_context_and_turn_instruction_for_phase(
                    &mut session,
                    Some(turn_context.as_str()),
                    turn_instruction,
                    provider_phase,
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
                            provider_phase = ProviderExecutionPhase::Repair;
                            continue;
                        }
                        return Err(RuntimeError::agent(error));
                    }
                }
            };
            let _ = events.send(RuntimeEvent::Progress { turn: session.turn });
            if !general_chat {
                let _ = crate::coding_trajectory::sync_and_render(&state.repo, &session, None)?;
            }

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
        })()
    };
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
                    if let Some(ledger) = execution_ledger.as_mut() {
                        crate::production_orchestrator::begin_kinds(
                            ledger,
                            &execution_plan,
                            &[medusa_multi_agent_scheduler::TaskKind::Review],
                            "dedicated-parent-review",
                        )
                        .map_err(RuntimeError::agent)?;
                        let _ = events.send(RuntimeEvent::Plan(
                            crate::production_orchestrator::projection(ledger),
                        ));
                    }
                    let review_provider = ConfiguredProvider::manager_from_config(
                        &state.config,
                        state.session_api_key.clone(),
                    )
                    .map_err(RuntimeError::agent)?;
                    match crate::mutation_transaction::complete_after_parent_review(
                        &evidence.transaction_path,
                        &state.repo,
                        &review_provider,
                        &state.config,
                        cancel.as_ref(),
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
                            let completion_text = mutation_completion_text(
                                &evidence.summary,
                                &receipt.commit,
                                &receipt.changed_paths,
                            );
                            let message = Message {
                                role: Role::Assistant,
                                content: vec![MessageBlock::Text {
                                    text: completion_text.clone(),
                                }],
                            };
                            session.messages.push(message.clone());
                            medusa_agent::record_session_event(
                                &mut session,
                                Actor::Coordinator,
                                EventPayload::AssistantMessageRecorded {
                                    message: serde_json::to_value(&message)
                                        .map_err(RuntimeError::agent)?,
                                },
                            )
                            .map_err(RuntimeError::agent)?;
                            session.completed = true;
                            medusa_agent::record_session_event(
                                &mut session,
                                Actor::Coordinator,
                                EventPayload::SessionCompleted {
                                    report_ref: format!("commit:{}", receipt.commit),
                                },
                            )
                            .map_err(RuntimeError::agent)?;
                            let _ = events.send(RuntimeEvent::AssistantText(completion_text));
                            result = Ok(RuntimeEvent::Completed {
                                session_id: session.id.to_string(),
                            });
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

fn mutation_completion_text(summary: &str, commit: &str, changed_paths: &[String]) -> String {
    let visible_summary = summary
        .rsplit_once("</think>")
        .map_or(summary, |(_, visible)| visible)
        .trim();
    let status = format!(
        "Verified and integrated commit `{commit}`. Changed paths: {}.",
        changed_paths.join(", ")
    );
    if visible_summary.is_empty() {
        status
    } else {
        format!("{visible_summary}\n\n{status}")
    }
}

#[cfg(test)]
mod mutation_completion_tests {
    use super::mutation_completion_text;

    #[test]
    fn hides_reasoning_and_preserves_visible_implementer_result() {
        let text = mutation_completion_text(
            "<think>private implementation reasoning</think>\n\nMEDUSA_TUI_MINIMAX_OK",
            "abc123",
            &["src/lib.rs".to_owned()],
        );
        assert!(!text.contains("private implementation reasoning"));
        assert!(text.starts_with("MEDUSA_TUI_MINIMAX_OK"));
        assert!(text.contains("Verified and integrated commit `abc123`"));
        assert!(text.contains("src/lib.rs"));
    }

    #[test]
    fn falls_back_to_verified_status_when_summary_has_no_visible_text() {
        let text = mutation_completion_text(
            "<think>private implementation reasoning</think>",
            "abc123",
            &["src/lib.rs".to_owned()],
        );
        assert_eq!(
            text,
            "Verified and integrated commit `abc123`. Changed paths: src/lib.rs."
        );
    }

    #[test]
    fn durable_completion_event_marks_the_session_completed() {
        use medusa_agent::AgentSession;
        use medusa_core::SessionId;
        use medusa_protocol::{Actor, EventPayload};
        use time::OffsetDateTime;

        let directory = tempfile::tempdir().expect("temporary repository");
        let mut session = AgentSession {
            id: SessionId::new(),
            objective: "durable mutation completion".to_owned(),
            repo: directory.path().to_path_buf(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            completed: false,
            turn: 0,
            plan: Vec::new(),
            pending_question: None,
            messages: Vec::new(),
            events: Vec::new(),
            evidence: Vec::new(),
            tool_artifacts: Vec::new(),
            world_model: None,
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
            codex_thread_id: None,
        };
        session.completed = true;
        medusa_agent::record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::SessionCompleted {
                report_ref: "commit:abc123".to_owned(),
            },
        )
        .expect("persist completion");

        let persisted = medusa_agent::session_browser::load_session(
            directory.path(),
            session.id.as_str(),
        )
        .expect("reload completed session");
        assert!(persisted.completed);
        assert!(matches!(
            persisted.events.last().map(|event| &event.payload),
            Some(EventPayload::SessionCompleted { report_ref }) if report_ref == "commit:abc123"
        ));
    }
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

mod recovery;

#[rustfmt::skip]
mod production_orchestrator;

/// Production task-contract and schedule definitions used by the runtime coordinator.
///
/// The shipped coordinated path is `RuntimeController -> run_prompt ->
/// multi_agent_coordinator::run_preflight -> isolated implementer -> dedicated no-tools parent reviewer`.
pub mod orchestration_planning {
    pub use super::production_orchestrator::*;
}
