use std::{
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

use medusa_agent::{
    AgentPlanStepStatus, AgentSession, persist_session,
    session_browser::{list_sessions, load_session},
};
use super::{
    RuntimeCommand, RuntimeController, RuntimeEvent, RuntimeState, SubmissionState,
    configure_model, dispatch_runtime_events, execute_slash_command_with_submission, mark_idle,
    restore_queued_followups, run_prompt,
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
    InvalidImage { path: PathBuf },
    ImagePixelLimit { path: PathBuf, pixels: u64, limit: u64 },
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
                "attached file is not UTF-8 text or a supported image: {}",
                path.display()
            ),
            Self::InvalidImage { path } => write!(
                formatter,
                "attached image has an invalid encoded structure: {}",
                path.display()
            ),
            Self::ImagePixelLimit { path, pixels, limit } => write!(
                formatter,
                "attached image has {pixels} pixels; limit is {limit}: {}",
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
        let state = RuntimeState::load(repo.clone())?;
        Self::start_resumed_with_state(repo, session_id, state)
    }

    pub fn start_resumed_with_config(
        repo: PathBuf,
        session_id: &str,
        config: medusa_config::Config,
    ) -> Result<Self, RuntimeError> {
        let state = RuntimeState::from_config(repo.clone(), config);
        Self::start_resumed_with_state(repo, session_id, state)
    }

    /// Continues the most recently updated durable session for this repository.
    pub fn start_continue_latest(repo: PathBuf) -> Result<Self, RuntimeError> {
        let session_id = latest_session_id(&repo)?;
        Self::start_resumed(repo, &session_id)
    }

    fn start_resumed_with_state(
        repo: PathBuf,
        session_id: &str,
        mut state: RuntimeState,
    ) -> Result<Self, RuntimeError> {
        let mut session = load_session(&repo, session_id).map_err(RuntimeError::agent)?;
        crate::execution_history::verify_resumed_session(&repo, &session)?;
        validate_resumed_session(&repo, &session)?;
        let interrupted_steps = recover_interrupted_session(&repo, &mut session)?;
        let restored_followups = restore_queued_followups(&session)?;
        let _ = crate::coding_trajectory::restore_for_resume(&repo, &session, false)?;
        let active_session_id = Some(session.id.to_string());
        state.session = Some(session);

        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let (runtime_event_tx, runtime_event_rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let submission = Arc::new(Mutex::new(SubmissionState {
            followups: restored_followups,
            active_session_id,
            ..SubmissionState::default()
        }));
        let worker_cancel = Arc::clone(&cancel);
        let worker_submission = Arc::clone(&submission);
        let worker_events = runtime_event_tx.clone();
        let team_control = state.team_control.clone();
        let dispatch_repo = repo.clone();
        let dispatch_submission = Arc::clone(&submission);
        let dispatch_events = event_tx.clone();
        thread::Builder::new()
            .name("medusa-runtime-resumed-events".to_owned())
            .spawn(move || {
                dispatch_runtime_events(
                    &dispatch_repo,
                    &dispatch_submission,
                    runtime_event_rx,
                    &dispatch_events,
                );
            })
            .map_err(RuntimeError::Io)?;
        thread::Builder::new()
            .name("medusa-runtime-resumed".to_owned())
            .spawn(move || {
                resumed_worker_loop(
                    state,
                    command_rx,
                    worker_events,
                    worker_cancel,
                    worker_submission,
                    interrupted_steps,
                );
            })
            .map_err(RuntimeError::Io)?;

        Ok(Self {
            commands: command_tx,
            events: event_rx,
            cancel,
            submission,
            event_sender: runtime_event_tx,
            team_control,
            repo,
            invariants: Arc::new(Mutex::new(crate::invariants::RuntimeInvariantRegistry::default())),
        })
    }
}

fn latest_session_id(repo: &Path) -> Result<String, RuntimeError> {
    list_sessions(repo)
        .map_err(RuntimeError::agent)?
        .into_iter()
        .next()
        .map(|session| session.id)
        .ok_or_else(|| {
            RuntimeError::InvalidCommand(format!(
                "no durable sessions exist for {}",
                repo.display()
            ))
        })
}

fn recover_interrupted_session(
    _repo: &Path,
    session: &mut AgentSession,
) -> Result<Vec<String>, RuntimeError> {
    let mut interrupted = Vec::new();
    for step in &mut session.plan {
        if step.status == AgentPlanStepStatus::InProgress {
            step.status = AgentPlanStepStatus::Failed;
            interrupted.push(step.title.clone());
        }
    }
    if interrupted.is_empty() {
        return Ok(interrupted);
    }
    session.updated_at = time::OffsetDateTime::now_utc();
    persist_session(session).map_err(RuntimeError::agent)?;
    Ok(interrupted)
}

fn validate_resumed_session(repo: &Path, session: &AgentSession) -> Result<(), RuntimeError> {
    if session.repo.as_path() != repo {
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
    interrupted_steps: Vec<String>,
) {
    let _ = events.send(state.settings_event());
    if !interrupted_steps.is_empty() {
        let _ = events.send(RuntimeEvent::Notice {
            title: "Interrupted work recovered".to_owned(),
            details: interrupted_steps
                .into_iter()
                .map(|title| format!("Marked failed after restart: {title}"))
                .collect(),
        });
    }
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
    let _ = events.send(crate::capability_event(state.repo.clone()));

    while let Ok(command) = commands.recv() {
        match command {
            RuntimeCommand::Submit { draft, accepted } => {
                let _ = events.send(RuntimeEvent::Started);
                let event = match run_prompt(
                    &mut state,
                    draft,
                    &events,
                    &cancel,
                    &submission,
                    Some(&accepted),
                ) {
                    Ok(event) => event,
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
            } => match super::recovery::execute_action(&state.repo, &view, &request, preflight) {
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
    cancel.store(true, Ordering::SeqCst);
    mark_idle(&submission, true);
}

#[cfg(test)]
mod interruption_tests {
    use super::*;
    use medusa_agent::AgentPlanStep;

    #[test]
    fn resumed_session_never_preserves_in_progress_plan_steps() {
        let directory = tempfile::tempdir().expect("tempdir");
        let repo = directory.path();
        let now = time::OffsetDateTime::now_utc();
        let mut session = AgentSession {
            id: medusa_core::SessionId::new(),
            objective: "recover".into(),
            repo: repo.to_path_buf(),
            created_at: now,
            updated_at: now,
            completed: false,
            turn: 1,
            plan: vec![AgentPlanStep {
                title: "provider request".into(),
                status: AgentPlanStepStatus::InProgress,
            }],
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
        let interrupted = recover_interrupted_session(repo, &mut session).expect("recover");
        assert_eq!(interrupted, ["provider request"]);
        assert_eq!(session.plan[0].status, AgentPlanStepStatus::Failed);
        let compatibility_snapshot = repo
            .join(".medusa/sessions")
            .join(format!("{}.json", session.id));
        let journal = repo
            .join(".medusa/journals")
            .join(format!("{}.events", session.id));
        assert!(compatibility_snapshot.is_file());
        assert!(journal.is_file());
        std::fs::remove_file(&compatibility_snapshot).expect("remove compatibility snapshot");
        let session_id = session.id.to_string();
        let restored = load_session(repo, &session_id).expect("restore from journal");
        assert_eq!(restored.plan[0].status, AgentPlanStepStatus::Failed);
        assert!(compatibility_snapshot.is_file());
        assert!(
            recover_interrupted_session(repo, &mut restored.clone())
                .expect("idempotent")
                .is_empty()
        );
    }
}
