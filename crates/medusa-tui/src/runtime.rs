use std::{
    collections::{BTreeMap, VecDeque},
    env, fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, TryLockError, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageFormat, RgbaImage};
use medusa_config::{Config, Mode, credential_environment};
use medusa_daemon::{
    DaemonClient, DaemonLaunch, DaemonLifecycleState, DaemonSupervisor, FrontendArtifactKind,
    FrontendArtifactUpload, FrontendCommandAcknowledgement, FrontendControlResult,
    FrontendCredentialUpdate, FrontendTransientEvent, LiveSessionAttachmentView,
};
use medusa_protocol::frontend::{
    FRONTEND_PROTOCOL_VERSION, FrontendCommand, FrontendCommandEnvelope, FrontendEvent,
    FrontendEventEnvelope, FrontendKind, PresentationActivity, PresentationActivityKind,
    PresentationLifecycle,
};
use time::OffsetDateTime;

use crate::app::{
    QuestionOption, QuestionPrompt, TranscriptPlan, TranscriptPlanStep, TranscriptPlanStepState,
};
use crate::clipboard::{PromptAttachment, PromptDraft};
use crate::commands::{
    ConfigCommand, Effort, LearningCommand, ModelCommand, ModelConfiguration, ReviewCommand,
    SlashCommand, TeamCommand,
};

pub use medusa_runtime::{
    RecoveryActionRequest, RecoveryOperation, RecoveryPreflightEvidence, RecoveryView,
    RuntimeActivity, RuntimeActivityKind, RuntimeError, SubmitDisposition, TeamSnapshot,
};

const TUI_CLIENT_PREFIX: &str = "tui-primary";
const DAEMON_POLL_INTERVAL: Duration = Duration::from_millis(50);
static COMMAND_SEQUENCE: AtomicU64 = AtomicU64::new(1);

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
    state: Arc<Mutex<DaemonRuntimeState>>,
    events: Receiver<RuntimeEvent>,
    event_sender: Sender<RuntimeEvent>,
    submission_in_flight: Arc<AtomicBool>,
}

struct DaemonRuntimeState {
    repo: PathBuf,
    client_id: String,
    session_id: Option<String>,
    replay_cursor: u64,
    pending_ack_cursor: Option<u64>,
    presentation: CanonicalPresentation,
    supervisor: DaemonSupervisor,
    last_lifecycle: Option<DaemonLifecycleState>,
    provider: String,
    context_window_tokens: u64,
    auto_compact_percent: u8,
    last_poll_error: Option<String>,
    pending_startup_error: Option<String>,
    initial_settings: Option<RuntimeEvent>,
}

struct CanonicalPresentation {
    pending: VecDeque<RuntimeEvent>,
    run_active: bool,
}

impl CanonicalPresentation {
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            run_active: false,
        }
    }

    fn reset(&mut self) {
        self.pending.clear();
        self.run_active = false;
    }

    fn push(&mut self, envelopes: Vec<FrontendEventEnvelope>) {
        for envelope in envelopes {
            self.pending
                .extend(map_frontend_event(envelope, &mut self.run_active));
        }
    }

    fn push_transient(
        &mut self,
        event: FrontendTransientEvent,
        context_window_tokens: u64,
        auto_compact_percent: u8,
    ) {
        self.pending.push_back(map_transient_event(
            event,
            context_window_tokens,
            auto_compact_percent,
        ));
    }

    fn try_event(&mut self) -> Option<RuntimeEvent> {
        self.pending.pop_front()
    }

    fn is_busy(&self) -> bool {
        self.run_active
    }
}

impl DaemonRuntimeState {
    fn new(repo: PathBuf) -> Self {
        let (supervisor, launch_error) = match DaemonLaunch::for_current_executable() {
            Ok(launch) => (DaemonSupervisor::new(&repo, launch), None),
            Err(error) => (
                DaemonSupervisor::observe_only(&repo),
                Some(format!("daemon launch setup failed: {error}")),
            ),
        };
        let project = repo.join(".medusa/config.toml");
        let project = project.exists().then_some(project);
        let (config, config_error) =
            match Config::load_layers(None, project.as_deref(), &BTreeMap::new(), &BTreeMap::new())
            {
                Ok(config) => (config, None),
                Err(error) => (
                    Config::default(),
                    Some(format!("runtime configuration failed: {error}")),
                ),
            };
        let credential_configured = credential_environment(&config.model.provider)
            .is_some_and(|name| env::var(name).is_ok());
        let initial_settings = RuntimeEvent::Settings {
            model: format!("{} / {}", config.model.provider, config.model.name),
            effort: format!("effort:{}", effort_label_for_turns(config.agent.max_turns)),
            plan_mode: config.agent.mode == Mode::ReadOnly,
            credential_configured,
            context_window_tokens: config.model.context_window_tokens,
            auto_compact_percent: config.model.auto_compact_percent,
        };
        let pending_startup_error = [launch_error, config_error]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("; ");
        Self {
            repo,
            client_id: format!("{TUI_CLIENT_PREFIX}-{}", std::process::id()),
            session_id: None,
            replay_cursor: 0,
            pending_ack_cursor: None,
            presentation: CanonicalPresentation::new(),
            supervisor,
            last_lifecycle: None,
            provider: config.model.provider,
            context_window_tokens: config.model.context_window_tokens,
            auto_compact_percent: config.model.auto_compact_percent,
            last_poll_error: None,
            pending_startup_error: (!pending_startup_error.is_empty())
                .then_some(pending_startup_error),
            initial_settings: Some(initial_settings),
        }
    }

    fn ensure_daemon(&mut self) -> Result<(), RuntimeError> {
        let lifecycle = self.supervisor.ensure_running().map_err(runtime_error)?;
        self.last_lifecycle = Some(lifecycle.state);
        Ok(())
    }

    fn client(&self) -> DaemonClient {
        self.supervisor.client()
    }

    fn envelope(&self, command: FrontendCommand) -> FrontendCommandEnvelope {
        let command_id = next_command_id();
        FrontendCommandEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            command_id: command_id.clone(),
            idempotency_key: command_id,
            frontend: FrontendKind::Tui,
            client_id: self.client_id.clone(),
            session_id: self.session_id.clone(),
            turn_id: None,
            timestamp: OffsetDateTime::now_utc(),
            command,
        }
    }

    fn envelope_for_session(
        &self,
        session_id: String,
        command: FrontendCommand,
    ) -> FrontendCommandEnvelope {
        let mut envelope = self.envelope(command);
        envelope.session_id = Some(session_id);
        envelope
    }

    fn dispatch(
        &mut self,
        command: FrontendCommand,
    ) -> Result<FrontendCommandAcknowledgement, RuntimeError> {
        self.ensure_daemon()?;
        self.client()
            .frontend(self.envelope(command))
            .map_err(runtime_error)
    }

    fn dispatch_for_session(
        &mut self,
        session_id: String,
        command: FrontendCommand,
    ) -> Result<FrontendCommandAcknowledgement, RuntimeError> {
        self.ensure_daemon()?;
        self.client()
            .frontend(self.envelope_for_session(session_id, command))
            .map_err(runtime_error)
    }

    fn bind_attachment(&mut self, attachment: LiveSessionAttachmentView) {
        self.session_id = Some(attachment.session.id);
        self.replay_cursor = attachment.replay_cursor;
        self.pending_ack_cursor =
            (self.replay_cursor > attachment.acknowledged_cursor).then_some(self.replay_cursor);
        self.presentation.push(attachment.replay);
    }

    fn resume(&mut self, session_id: String) -> Result<(), RuntimeError> {
        let acknowledgement = self.dispatch_for_session(
            session_id.clone(),
            FrontendCommand::ResumeSession {
                session_id: session_id.clone(),
            },
        )?;
        let FrontendControlResult::RuntimeReady { attachment } = acknowledgement.result else {
            return Err(invalid_runtime(
                "daemon returned an unexpected resume result",
            ));
        };
        self.bind_attachment(attachment);
        Ok(())
    }

    fn continue_latest(&mut self) -> Result<(), RuntimeError> {
        let acknowledgement = self.dispatch(FrontendCommand::ListSessions)?;
        let FrontendControlResult::Sessions { sessions } = acknowledgement.result else {
            return Err(invalid_runtime(
                "daemon returned an unexpected session-list result",
            ));
        };
        let session_id = sessions
            .first()
            .map(|session| session.id.clone())
            .ok_or_else(|| {
                invalid_runtime(format!(
                    "no durable sessions exist for {}",
                    self.repo.display()
                ))
            })?;
        self.resume(session_id)
    }

    fn sync_credential(
        &mut self,
        provider: &str,
        credential: Option<String>,
    ) -> Result<(), RuntimeError> {
        let Some(credential) = credential.filter(|value| !value.trim().is_empty()) else {
            return Ok(());
        };
        self.ensure_daemon()?;
        self.client()
            .frontend_credential(FrontendCredentialUpdate {
                provider: provider.to_owned(),
                credential,
            })
            .map_err(runtime_error)
    }

    fn stage_draft(&mut self, draft: PromptDraft) -> Result<(String, Vec<String>), RuntimeError> {
        let PromptDraft {
            text,
            attachments,
            revision: _,
        } = draft;
        let mut artifact_ids = Vec::with_capacity(attachments.len());
        for attachment in attachments {
            let upload = match attachment {
                PromptAttachment::PastedText(attachment) => FrontendArtifactUpload {
                    display_name: attachment.display_name,
                    mime_type: Some("text/plain".to_owned()),
                    kind: FrontendArtifactKind::Text,
                    bytes_base64: STANDARD.encode(attachment.text.as_bytes()),
                },
                PromptAttachment::Image(attachment) => {
                    let image =
                        RgbaImage::from_raw(attachment.width, attachment.height, attachment.rgba)
                            .ok_or_else(|| {
                            invalid_runtime(format!(
                                "image attachment {} has invalid RGBA dimensions",
                                attachment.display_name
                            ))
                        })?;
                    let mut encoded = Cursor::new(Vec::new());
                    DynamicImage::ImageRgba8(image)
                        .write_to(&mut encoded, ImageFormat::Png)
                        .map_err(runtime_error)?;
                    FrontendArtifactUpload {
                        display_name: png_display_name(&attachment.display_name),
                        mime_type: Some("image/png".to_owned()),
                        kind: FrontendArtifactKind::Image,
                        bytes_base64: STANDARD.encode(encoded.into_inner()),
                    }
                }
                PromptAttachment::File(attachment) => {
                    let canonical = fs::canonicalize(&attachment.path).map_err(runtime_error)?;
                    if !canonical.starts_with(&self.repo) {
                        return Err(invalid_runtime(format!(
                            "attachment {} is outside the selected repository",
                            canonical.display()
                        )));
                    }
                    let bytes = fs::read(&canonical).map_err(runtime_error)?;
                    FrontendArtifactUpload {
                        display_name: canonical
                            .file_name()
                            .and_then(|value| value.to_str())
                            .unwrap_or("attachment.bin")
                            .to_owned(),
                        mime_type: None,
                        kind: FrontendArtifactKind::File,
                        bytes_base64: STANDARD.encode(bytes),
                    }
                }
            };
            artifact_ids.push(
                self.client()
                    .frontend_artifact(upload)
                    .map_err(runtime_error)?,
            );
        }
        Ok((text, artifact_ids))
    }

    fn submit_draft(&mut self, draft: PromptDraft) -> Result<SubmitDisposition, RuntimeError> {
        self.ensure_daemon()?;
        let (text, attachment_ids) = self.stage_draft(draft)?;
        let command = if self.session_id.is_none() {
            FrontendCommand::CreateSession {
                repository_profile: "tui".to_owned(),
                objective: (!text.trim().is_empty()).then_some(text),
                attachment_ids,
            }
        } else {
            FrontendCommand::Submit {
                text,
                attachment_ids,
            }
        };
        let acknowledgement = self.dispatch(command)?;
        let FrontendControlResult::SubmissionAccepted { session_id, queued } =
            acknowledgement.result
        else {
            return Err(invalid_runtime(
                "daemon returned an unexpected submission result",
            ));
        };
        self.session_id = Some(session_id);
        Ok(if queued {
            SubmitDisposition::Queued
        } else {
            SubmitDisposition::Started
        })
    }

    fn run_command(&mut self, command: SlashCommand) -> Result<(), RuntimeError> {
        match command {
            SlashCommand::New => {
                if self.session_id.is_none() {
                    self.reset_session();
                    return Ok(());
                }
                let acknowledgement = self.dispatch(FrontendCommand::NewSession)?;
                if !matches!(
                    acknowledgement.result,
                    FrontendControlResult::CommandAccepted { .. }
                ) {
                    return Err(invalid_runtime(
                        "daemon returned an unexpected new-session result",
                    ));
                }
                self.reset_session();
                Ok(())
            }
            SlashCommand::Model(ModelCommand::SetApiKey(api_key)) => {
                let provider = self.provider.clone();
                self.sync_credential(&provider, Some(api_key))
            }
            command => {
                let acknowledgement = self.dispatch(FrontendCommand::RunCommand {
                    input: slash_command_input(&command),
                })?;
                if matches!(
                    acknowledgement.result,
                    FrontendControlResult::CommandAccepted { .. }
                ) {
                    Ok(())
                } else {
                    Err(invalid_runtime(
                        "daemon returned an unexpected command result",
                    ))
                }
            }
        }
    }

    fn configure_model(&mut self, configuration: ModelConfiguration) -> Result<(), RuntimeError> {
        self.sync_credential(&configuration.provider, configuration.api_key)?;
        let provider = configuration.provider;
        let model = configuration.model;
        let acknowledgement = self.dispatch(FrontendCommand::ConfigureModel {
            provider: Some(provider.clone()),
            model,
        })?;
        if !matches!(
            acknowledgement.result,
            FrontendControlResult::CommandAccepted { .. }
        ) {
            return Err(invalid_runtime(
                "daemon returned an unexpected model result",
            ));
        }
        let acknowledgement = self.dispatch(FrontendCommand::SetEffort {
            effort: configuration.effort.label().to_owned(),
        })?;
        if !matches!(
            acknowledgement.result,
            FrontendControlResult::CommandAccepted { .. }
        ) {
            return Err(invalid_runtime(
                "daemon returned an unexpected effort result",
            ));
        }
        self.provider = provider;
        Ok(())
    }

    fn execute_recovery(
        &mut self,
        _view: RecoveryView,
        request: RecoveryActionRequest,
        _preflight: RecoveryPreflightEvidence,
    ) -> Result<(), RuntimeError> {
        let acknowledgement = self.dispatch_for_session(
            request.session_id,
            FrontendCommand::RecoveryAction {
                operation: recovery_operation_name(request.operation).to_owned(),
                checkpoint_id: request.checkpoint_id,
                confirmed_destructive_effects: request.confirmed_destructive_effects,
            },
        )?;
        if matches!(
            acknowledgement.result,
            FrontendControlResult::CommandAccepted { .. }
        ) {
            Ok(())
        } else {
            Err(invalid_runtime(
                "daemon returned an unexpected recovery result",
            ))
        }
    }

    fn cancel(&mut self) -> Result<bool, RuntimeError> {
        if self.session_id.is_none() {
            return Ok(false);
        }
        let acknowledgement = self.dispatch(FrontendCommand::CancelTurn)?;
        let FrontendControlResult::CancellationRequested { requested, .. } = acknowledgement.result
        else {
            return Err(invalid_runtime(
                "daemon returned an unexpected cancellation result",
            ));
        };
        Ok(requested)
    }

    fn lifecycle_event(&mut self) -> Option<RuntimeEvent> {
        let lifecycle = self.supervisor.poll();
        let suppress_connected_after_start = matches!(
            (self.last_lifecycle, lifecycle.state),
            (
                Some(DaemonLifecycleState::Started | DaemonLifecycleState::Recovered),
                DaemonLifecycleState::Connected
            )
        );
        let changed = self.last_lifecycle != Some(lifecycle.state);
        self.last_lifecycle = Some(lifecycle.state);
        if !changed || suppress_connected_after_start {
            return None;
        }
        Some(RuntimeEvent::Notice {
            title: format!("Background daemon {}", lifecycle.state.as_str()),
            details: vec![lifecycle.detail],
        })
    }

    fn poll_events(&mut self) -> Vec<RuntimeEvent> {
        let mut events = Vec::new();
        if let Some(error) = self.pending_startup_error.take() {
            events.push(RuntimeEvent::Failed(error));
        }
        if let Some(event) = self.lifecycle_event() {
            let recovered = matches!(self.last_lifecycle, Some(DaemonLifecycleState::Recovered));
            events.push(event);
            if recovered
                && let Some(session_id) = self.session_id.clone()
                && let Err(error) = self.resume(session_id)
            {
                events.push(RuntimeEvent::Failed(error.to_string()));
                return events;
            }
        }

        match self.poll_daemon() {
            Ok(()) => {
                self.last_poll_error = None;
                while let Some(event) = self.presentation.try_event() {
                    events.push(event);
                }
            }
            Err(error) => {
                let message = error.to_string();
                if self.last_poll_error.as_deref() != Some(&message) {
                    self.last_poll_error = Some(message.clone());
                    events.push(RuntimeEvent::Failed(message));
                }
            }
        }
        events
    }

    fn poll_daemon(&mut self) -> Result<(), RuntimeError> {
        let Some(session_id) = self.session_id.clone() else {
            return Ok(());
        };
        self.acknowledge_previous_delivery()?;

        let transient = self.dispatch(FrontendCommand::PollTransient)?;
        let FrontendControlResult::Transient { events } = transient.result else {
            return Err(invalid_runtime(
                "daemon returned an unexpected transient-event result",
            ));
        };
        for event in events {
            if matches!(event, FrontendTransientEvent::NewSession) {
                self.reset_session();
            }
            self.presentation.push_transient(
                event,
                self.context_window_tokens,
                self.auto_compact_percent,
            );
        }
        if self.session_id.is_none() {
            return Ok(());
        }

        let replay = self.dispatch(FrontendCommand::Replay {
            after_cursor: self.replay_cursor,
        })?;
        let FrontendControlResult::Events { replay } = replay.result else {
            return Err(invalid_runtime(
                "daemon returned an unexpected replay result",
            ));
        };
        if replay.session_id != session_id {
            return Err(invalid_runtime(
                "daemon replay switched sessions unexpectedly",
            ));
        }
        self.replay_cursor = replay.next_cursor;
        self.presentation.push(replay.events);
        if replay.next_cursor > replay.after_cursor {
            self.pending_ack_cursor = Some(replay.next_cursor);
        }
        Ok(())
    }

    fn acknowledge_previous_delivery(&mut self) -> Result<(), RuntimeError> {
        let Some(cursor) = self.pending_ack_cursor.take() else {
            return Ok(());
        };
        let acknowledgement = self.dispatch(FrontendCommand::AcknowledgeCursor { cursor })?;
        if !matches!(
            acknowledgement.result,
            FrontendControlResult::CursorAcknowledged { .. }
        ) {
            return Err(invalid_runtime(
                "daemon returned an unexpected cursor acknowledgement",
            ));
        }
        Ok(())
    }

    fn reset_session(&mut self) {
        self.session_id = None;
        self.replay_cursor = 0;
        self.pending_ack_cursor = None;
        self.presentation.reset();
    }
}

impl RuntimeController {
    pub fn start(repo: PathBuf) -> Self {
        Self::from_state(DaemonRuntimeState::new(repo))
    }

    pub fn start_resumed(repo: PathBuf, session_id: &str) -> Result<Self, RuntimeError> {
        let mut state = DaemonRuntimeState::new(repo);
        state.resume(session_id.to_owned())?;
        Ok(Self::from_state(state))
    }

    pub fn start_continue_latest(repo: PathBuf) -> Result<Self, RuntimeError> {
        let mut state = DaemonRuntimeState::new(repo);
        state.continue_latest()?;
        Ok(Self::from_state(state))
    }

    fn from_state(mut state: DaemonRuntimeState) -> Self {
        let initial_settings = state.initial_settings.take();
        let state = Arc::new(Mutex::new(state));
        let (event_sender, events) = mpsc::channel();
        if let Some(event) = initial_settings {
            let _ = event_sender.send(event);
        }
        if let Err(error) = spawn_event_poller(Arc::downgrade(&state), event_sender.clone()) {
            let _ = event_sender.send(RuntimeEvent::Failed(format!(
                "daemon event worker failed: {error}"
            )));
        }
        Self {
            state,
            events,
            event_sender,
            submission_in_flight: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn submit(&self, draft: PromptDraft) -> Result<SubmitDisposition, RuntimeError> {
        let state = Arc::clone(&self.state);
        dispatch_submission(
            Arc::clone(&self.submission_in_flight),
            self.event_sender.clone(),
            move || lock_state(&state).submit_draft(draft),
        )
    }

    pub fn run_command(&self, command: SlashCommand) -> Result<(), RuntimeError> {
        if self.submission_in_flight.load(Ordering::Acquire) {
            return Err(RuntimeError::Busy);
        }
        try_lock_state(&self.state)?.run_command(command)
    }

    pub fn configure_model(&self, configuration: ModelConfiguration) -> Result<(), RuntimeError> {
        if self.submission_in_flight.load(Ordering::Acquire) {
            return Err(RuntimeError::Busy);
        }
        try_lock_state(&self.state)?.configure_model(configuration)
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
        try_lock_state(&self.state)?.execute_recovery(view, request, preflight)
    }

    pub fn cancel(&self) -> bool {
        match self.state.try_lock() {
            Ok(mut state) => state.cancel().unwrap_or(false),
            Err(TryLockError::Poisoned(poisoned)) => {
                poisoned.into_inner().cancel().unwrap_or(false)
            }
            Err(TryLockError::WouldBlock) => {
                let state = Arc::clone(&self.state);
                let _ = thread::Builder::new()
                    .name("medusa-tui-deferred-cancel".to_owned())
                    .spawn(move || {
                        let _ = lock_state(&state).cancel();
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
        match self.state.try_lock() {
            Ok(state) => state.presentation.is_busy(),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().presentation.is_busy(),
            Err(TryLockError::WouldBlock) => true,
        }
    }

    pub fn try_event(&self) -> Result<Option<RuntimeEvent>, RuntimeError> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(RuntimeError::WorkerStopped),
        }
    }
}

fn spawn_event_poller(
    state: Weak<Mutex<DaemonRuntimeState>>,
    events: Sender<RuntimeEvent>,
) -> Result<(), RuntimeError> {
    thread::Builder::new()
        .name("medusa-tui-daemon-events".to_owned())
        .spawn(move || {
            loop {
                let Some(state) = state.upgrade() else {
                    break;
                };
                match state.try_lock() {
                    Ok(mut state) => {
                        for event in state.poll_events() {
                            if events.send(event).is_err() {
                                return;
                            }
                        }
                    }
                    Err(TryLockError::Poisoned(poisoned)) => {
                        for event in poisoned.into_inner().poll_events() {
                            if events.send(event).is_err() {
                                return;
                            }
                        }
                    }
                    Err(TryLockError::WouldBlock) => {}
                }
                thread::sleep(DAEMON_POLL_INTERVAL);
            }
        })
        .map(|_| ())
        .map_err(RuntimeError::Io)
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
            if let Err(error) = operation() {
                let _ = events.send(RuntimeEvent::Failed(format!(
                    "submission rejected: {error}"
                )));
            }
            worker_flag.store(false, Ordering::Release);
        });
    if spawn.is_err() {
        in_flight.store(false, Ordering::Release);
        return Err(RuntimeError::WorkerStopped);
    }
    Ok(SubmitDisposition::Started)
}

fn try_lock_state(
    state: &Arc<Mutex<DaemonRuntimeState>>,
) -> Result<std::sync::MutexGuard<'_, DaemonRuntimeState>, RuntimeError> {
    match state.try_lock() {
        Ok(state) => Ok(state),
        Err(TryLockError::Poisoned(poisoned)) => Ok(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => Err(RuntimeError::Busy),
    }
}

fn lock_state<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn next_command_id() -> String {
    format!(
        "tui-command-{}-{}",
        std::process::id(),
        COMMAND_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn png_display_name(name: &str) -> String {
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("attachment");
    format!("{stem}.png")
}

fn runtime_error(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Agent(error.to_string())
}

fn invalid_runtime(message: impl Into<String>) -> RuntimeError {
    RuntimeError::InvalidCommand(message.into())
}

fn effort_label_for_turns(max_turns: u32) -> &'static str {
    match max_turns {
        0..=99 => "low",
        100..=299 => "medium",
        _ => "high",
    }
}

fn recovery_operation_name(operation: RecoveryOperation) -> &'static str {
    match operation {
        RecoveryOperation::Inspect => "inspect",
        RecoveryOperation::Resume => "resume",
        RecoveryOperation::RestoreCheckpoint => "restore_checkpoint",
        RecoveryOperation::RetryVerification => "retry_verification",
        RecoveryOperation::Abandon => "abandon",
    }
}

fn slash_command_input(command: &SlashCommand) -> String {
    match command {
        SlashCommand::Help => "/help".to_owned(),
        SlashCommand::Learning { action } => match action {
            LearningCommand::Show { filter } => option_command("/learning show", filter.as_deref()),
            LearningCommand::Inspect { id } => format!("/learning inspect {id}"),
            LearningCommand::Propose { scope, key, value } => {
                format!("/learning propose {scope} {key} {value}")
            }
            LearningCommand::Evaluate { id, passed } => {
                format!("/learning evaluate {id} {}", if *passed { "pass" } else { "fail" })
            }
            LearningCommand::Approve { id } => format!("/learning approve {id}"),
            LearningCommand::Reject { id } => format!("/learning reject {id}"),
            LearningCommand::Defer { id } => format!("/learning defer {id}"),
            LearningCommand::Validate { id } => format!("/learning validate {id}"),
            LearningCommand::Activate { id } => format!("/learning activate {id}"),
            LearningCommand::Suspend { id } => format!("/learning suspend {id}"),
            LearningCommand::Rollback { id } => format!("/learning rollback {id}"),
            LearningCommand::Delete { id } => format!("/learning delete {id}"),
            LearningCommand::Privacy => "/learning privacy".to_owned(),
            LearningCommand::Export => "/learning export".to_owned(),
        },
        SlashCommand::Review { action } => match action {
            ReviewCommand::Show { filter } => option_command("/review show", filter.as_deref()),
            ReviewCommand::AcceptFile { path } => format!("/review accept {path}"),
            ReviewCommand::AcceptTask => "/review accept-all".to_owned(),
            ReviewCommand::RevertFile { path } => format!("/review revert {path}"),
            ReviewCommand::RevertHunk { path, hunk_id } => {
                format!("/review revert-hunk {path} {hunk_id}")
            }
            ReviewCommand::Export => "/review export".to_owned(),
        },
        SlashCommand::Config(command) => match command {
            ConfigCommand::Show => "/config show".to_owned(),
            ConfigCommand::Profiles => "/config profiles".to_owned(),
            ConfigCommand::UseProfile { name } => format!("/config use {name}"),
            ConfigCommand::Set { key, value } => format!("/config set {key} {value}"),
            ConfigCommand::Unset { key } => format!("/config unset {key}"),
            ConfigCommand::Validate => "/config validate".to_owned(),
        },
        SlashCommand::New => "/new".to_owned(),
        SlashCommand::Compact { focus } => option_command("/compact", focus.as_deref()),
        SlashCommand::Goal { objective } => option_command("/goal", objective.as_deref()),
        SlashCommand::Model(command) => match command {
            ModelCommand::Show => "/model".to_owned(),
            ModelCommand::SetModel(model) => format!("/model model {model}"),
            ModelCommand::SetProvider(provider) => format!("/model provider {provider}"),
            ModelCommand::SetApiKey(_) => "/model key <redacted>".to_owned(),
        },
        SlashCommand::Effort { effort } => match effort {
            Some(effort) => format!("/effort {}", effort.label()),
            None => "/effort".to_owned(),
        },
        SlashCommand::Skills => "/skills".to_owned(),
        SlashCommand::Skill { selector, task } => {
            option_command(&format!("/{selector}"), task.as_deref())
        }
        SlashCommand::Plan { task } => option_command("/plan", task.as_deref()),
        SlashCommand::Team(command) => match command {
            TeamCommand::Show => "/team".to_owned(),
            TeamCommand::Steer {
                worker_id,
                instruction,
            } => format!("/steer {worker_id} {instruction}"),
            TeamCommand::StopWorker { worker_id } => format!("/stop-worker {worker_id}"),
            TeamCommand::StopTeam => "/stop-team".to_owned(),
        },
    }
}

fn option_command(prefix: &str, value: Option<&str>) -> String {
    value.map_or_else(|| prefix.to_owned(), |value| format!("{prefix} {value}"))
}

fn map_transient_event(
    event: FrontendTransientEvent,
    context_window_tokens: u64,
    auto_compact_percent: u8,
) -> RuntimeEvent {
    match event {
        FrontendTransientEvent::RecoveryAvailable { recovery } => {
            match serde_json::from_value::<RecoveryView>(recovery) {
                Ok(view) => RuntimeEvent::Notice {
                    title: "Recovery available".to_owned(),
                    details: recovery_details(&view),
                },
                Err(error) => RuntimeEvent::Notice {
                    title: "Recovery available".to_owned(),
                    details: vec![format!(
                        "Daemon recovery evidence could not be rendered: {error}"
                    )],
                },
            }
        }
        FrontendTransientEvent::RecoveryCompleted { record, audit_path } => RuntimeEvent::Notice {
            title: "Recovery completed".to_owned(),
            details: vec![format!("Audit: {audit_path}"), format!("Record: {record}")],
        },
        FrontendTransientEvent::Settings {
            model,
            effort,
            plan_mode,
            credential_configured,
        } => RuntimeEvent::Settings {
            model,
            effort,
            plan_mode,
            credential_configured,
            context_window_tokens,
            auto_compact_percent,
        },
        FrontendTransientEvent::ConfigurationChanged {
            revision,
            active_profile,
            changed_keys,
            origin,
            apply_timing,
        } => RuntimeEvent::Notice {
            title: format!("Configuration revision {revision} applied"),
            details: vec![
                format!("Profile: {active_profile}"),
                format!("Changed: {}", changed_keys.join(", ")),
                format!("Origin: {origin}"),
                format!("Apply timing: {apply_timing}"),
            ],
        },
        FrontendTransientEvent::Notice { title, details } if title == "Runtime capabilities" => {
            RuntimeEvent::Activity(RuntimeActivity {
                id: Some("runtime-capabilities".to_owned()),
                kind: RuntimeActivityKind::Done,
                title,
                details,
            })
        }
        FrontendTransientEvent::Notice { title, details } => {
            RuntimeEvent::Notice { title, details }
        }
        FrontendTransientEvent::NewSession => RuntimeEvent::NewSession,
        FrontendTransientEvent::Progress { turn } => RuntimeEvent::Progress { turn },
        FrontendTransientEvent::Failed { message } => RuntimeEvent::Failed(message),
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
    use medusa_protocol::frontend::{
        FRONTEND_PROTOCOL_VERSION, FrontendEvent, FrontendEventEnvelope, PresentationLifecycle,
    };
    use std::time::{Duration, Instant};

    fn envelope(event: FrontendEvent) -> FrontendEventEnvelope {
        FrontendEventEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            event_id: "event-1:tui".to_owned(),
            cursor: 1,
            session_id: "session-1".to_owned(),
            turn_id: Some("turn-1".to_owned()),
            parent_event_id: None,
            correlation_id: "correlation-1".to_owned(),
            timestamp: OffsetDateTime::UNIX_EPOCH,
            lifecycle: PresentationLifecycle::Active,
            event,
        }
    }

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
        let events = map_frontend_event(envelope(FrontendEvent::TurnFinished), &mut run_active);
        assert!(matches!(events.front(), Some(RuntimeEvent::TurnFinished)));
        assert!(!run_active);
    }

    #[test]
    fn transient_settings_keep_the_tui_contract() {
        assert!(matches!(
            map_transient_event(
                FrontendTransientEvent::Settings {
                    model: "MiniMax-M3".to_owned(),
                    effort: "high".to_owned(),
                    plan_mode: false,
                    credential_configured: true,
                },
                1_000_000,
                40,
            ),
            RuntimeEvent::Settings {
                credential_configured: true,
                context_window_tokens: 1_000_000,
                auto_compact_percent: 40,
                ..
            }
        ));
    }

    #[test]
    fn slash_commands_round_trip_to_daemon_inputs() {
        assert_eq!(
            slash_command_input(&SlashCommand::Team(TeamCommand::Steer {
                worker_id: "worker-1".to_owned(),
                instruction: "focus tests".to_owned(),
            })),
            "/steer worker-1 focus tests"
        );
        assert_eq!(
            slash_command_input(&SlashCommand::Config(ConfigCommand::Set {
                key: "model.name".to_owned(),
                value: "MiniMax-M3".to_owned(),
            })),
            "/config set model.name MiniMax-M3"
        );
        assert_eq!(
            slash_command_input(&SlashCommand::Model(ModelCommand::SetApiKey(
                "secret".to_owned()
            ))),
            "/model key <redacted>"
        );
    }

    #[test]
    fn command_identity_is_process_scoped_and_monotonic() {
        let first = next_command_id();
        let second = next_command_id();
        assert_ne!(first, second);
        assert!(first.starts_with("tui-command-"));
        assert!(second.starts_with("tui-command-"));
    }

    #[test]
    fn tui_adapter_has_no_in_process_runtime_authority() {
        let source = include_str!("runtime.rs");
        assert!(!source.contains(concat!("medusa_runtime::", "RuntimeController")));
        assert!(source.contains("DaemonSupervisor"));
        assert!(source.contains("FrontendCommandEnvelope"));
    }
}
