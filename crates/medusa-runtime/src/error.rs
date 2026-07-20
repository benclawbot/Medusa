use std::{
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

use medusa_agent::{AgentEngine, AgentSession};
use medusa_capabilities::CapabilityRegistry;
use medusa_provider::ConfiguredProvider;

use super::{
    RuntimeCommand, RuntimeController, RuntimeEvent, RuntimeState, SubmissionState,
    configure_model, execute_slash_command_with_submission, mark_idle, run_prompt,
};

#[derive(Debug)]
pub enum RuntimeError {
    Agent(String),
    Io(io::Error),
    Png(String),
    WorkerStopped,
    Busy,
    EmptyPrompt,
    TurnLimit(u32),
    InvalidCommand(String),
    BinaryFile { path: PathBuf },
    FileTooLarge { path: PathBuf, bytes: usize },
}

impl RuntimeError {
    pub(crate) fn agent(error: impl std::fmt::Display) -> Self {
        Self::Agent(error.to_string())
    }

    pub(crate) fn png(error: impl std::fmt::Display) -> Self {
        Self::Png(error.to_string())
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Agent(error) => write!(formatter, "agent runtime failed: {error}"),
            Self::Io(error) => write!(formatter, "runtime I/O failed: {error}"),
            Self::Png(error) => write!(formatter, "screenshot encoding failed: {error}"),
            Self::WorkerStopped => formatter.write_str("agent runtime worker stopped"),
            Self::Busy => formatter.write_str("an agent task is already running"),
            Self::EmptyPrompt => formatter.write_str("prompt and attachments are empty"),
            Self::TurnLimit(limit) => write!(formatter, "agent reached the {limit}-turn limit"),
            Self::InvalidCommand(error) => formatter.write_str(error),
            Self::BinaryFile { path } => write!(
                formatter,
                "attached file is not UTF-8 text: {}",
                path.display()
            ),
            Self::FileTooLarge { path, bytes } => write!(
                formatter,
                "attached file is too large for prompt context: {} ({bytes} bytes)",
                path.display()
            ),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<io::Error> for RuntimeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl RuntimeController {
    /// Starts a runtime with a verified durable session already attached.
    pub fn start_resumed(repo: PathBuf, session_id: &str) -> Result<Self, RuntimeError> {
        let mut state = RuntimeState::load(repo.clone())?;
        let provider =
            ConfiguredProvider::manager_from_config(&state.config, state.session_api_key.clone())
                .map_err(RuntimeError::agent)?;
        let engine = AgentEngine::new(provider, state.config.clone());
        let session = engine
            .load_session(&repo, session_id)
            .map_err(RuntimeError::agent)?;
        validate_resumed_session(&repo, &session)?;
        state.session = Some(session);

        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let submission = Arc::new(Mutex::new(SubmissionState::default()));
        let worker_cancel = Arc::clone(&cancel);
        let worker_submission = Arc::clone(&submission);
        let worker_events = event_tx.clone();
        thread::Builder::new()
            .name("medusa-runtime-resumed".to_owned())
            .spawn(move || {
                resumed_worker_loop(
                    state,
                    command_rx,
                    worker_events,
                    worker_cancel,
                    worker_submission,
                );
            })
            .map_err(RuntimeError::Io)?;

        Ok(Self {
            commands: command_tx,
            events: event_rx,
            cancel,
            submission,
        })
    }
}

fn validate_resumed_session(repo: &PathBuf, session: &AgentSession) -> Result<(), RuntimeError> {
    if session.repo != *repo {
        return Err(RuntimeError::InvalidCommand(format!(
            "session {} belongs to {}, not {}",
            session.id,
            session.repo.display(),
            repo.display()
        )));
    }
    Ok(())
}

fn resumed_worker_loop(
    mut state: RuntimeState,
    commands: mpsc::Receiver<RuntimeCommand>,
    events: mpsc::Sender<RuntimeEvent>,
    cancel: Arc<AtomicBool>,
    submission: Arc<Mutex<SubmissionState>>,
) {
    let _ = events.send(state.settings_event());
    if let Some(session) = state.session.as_ref() {
        let _ = events.send(RuntimeEvent::Notice {
            title: "Session resumed".to_owned(),
            details: vec![
                session.objective.clone(),
                format!("session: {}", session.id),
                format!("turn: {}", session.turn),
            ],
        });
        if !session.plan.is_empty() {
            let _ = events.send(RuntimeEvent::Plan(session.plan.clone()));
        }
        let _ = events.send(RuntimeEvent::Progress { turn: session.turn });
        if let Some(question) = session.pending_question.clone() {
            let _ = events.send(RuntimeEvent::Question(question));
        }
    }
    let capability_event = match CapabilityRegistry::discover(state.repo.clone()) {
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
    };
    let _ = events.send(capability_event);

    while let Ok(command) = commands.recv() {
        match command {
            RuntimeCommand::Submit(draft) => {
                let _ = events.send(RuntimeEvent::Started);
                let event = match run_prompt(&mut state, draft, &events, &cancel, &submission) {
                    Ok(event) => event,
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
            RuntimeCommand::Shutdown => break,
        }
    }
    cancel.store(true, Ordering::SeqCst);
    mark_idle(&submission, true);
}
