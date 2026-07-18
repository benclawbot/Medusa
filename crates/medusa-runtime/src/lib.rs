use std::{
    collections::{BTreeMap, VecDeque},
    env, io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
};

use medusa_agent::{
    AgentEngine, AgentPlanStep, AgentQuestion, AgentSession, StepOutcome, compact_session,
    update_session_objective,
};
use medusa_config::{Config, Mode};
use medusa_provider::{MiniMaxProvider, ModelProvider};

use crate::{
    commands::{Effort, ModelCommand, ModelConfiguration, SlashCommand},
    prompt::PromptDraft,
};

pub mod commands;
pub mod prompt;
mod support;
#[cfg(test)]
mod tests;

pub use medusa_agent::{
    AgentPlanStep as RuntimePlanStep, AgentPlanStepStatus, AgentQuestionItem, AgentQuestionOption,
};

use support::{
    SelectedSkill, UpdateState, configure_model, credential_environment, discover_skills,
    effort_for_turns, forward_update, is_supported_provider, load_selected_skill, message_blocks,
    model_configuration_details, objective_for, turns_for_effort,
};

#[derive(Debug)]
pub enum RuntimeCommand {
    Submit(PromptDraft),
    Slash(SlashCommand),
    ConfigureModel(ModelConfiguration),
    Shutdown,
}

#[derive(Debug)]
pub enum RuntimeEvent {
    Started,
    AssistantText(String),
    Activity(RuntimeActivity),
    Plan(Vec<AgentPlanStep>),
    Question(AgentQuestion),
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cache_read_input_tokens: u64,
        cache_creation_input_tokens: u64,
        model_elapsed_millis: u64,
    },
    Progress {
        turn: u32,
    },
    Settings {
        model: String,
        effort: String,
        plan_mode: bool,
        credential_configured: bool,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeActivityKind {
    Assistant,
    Done,
    Error,
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

#[derive(Default)]
struct SubmissionState {
    busy: bool,
    followups: VecDeque<PromptDraft>,
}

pub struct RuntimeController {
    commands: Sender<RuntimeCommand>,
    events: Receiver<RuntimeEvent>,
    cancel: Arc<AtomicBool>,
    submission: Arc<Mutex<SubmissionState>>,
}

impl RuntimeController {
    pub fn start(repo: PathBuf) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let submission = Arc::new(Mutex::new(SubmissionState::default()));
        let worker_cancel = Arc::clone(&cancel);
        let worker_submission = Arc::clone(&submission);
        let worker_events = event_tx.clone();
        if let Err(error) = thread::Builder::new()
            .name("medusa-runtime".to_owned())
            .spawn(move || {
                worker_loop(
                    repo,
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
        }
    }

    pub fn submit(&self, draft: PromptDraft) -> Result<SubmitDisposition, RuntimeError> {
        let mut submission = lock_submission(&self.submission);
        if submission.busy {
            submission.followups.push_back(draft);
            return Ok(SubmitDisposition::Queued);
        }
        submission.busy = true;
        self.cancel.store(false, Ordering::SeqCst);
        if self.commands.send(RuntimeCommand::Submit(draft)).is_err() {
            submission.busy = false;
            return Err(RuntimeError::WorkerStopped);
        }
        Ok(SubmitDisposition::Started)
    }

    pub fn run_command(&self, command: SlashCommand) -> Result<(), RuntimeError> {
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

    pub fn cancel(&self) -> bool {
        if lock_submission(&self.submission).busy {
            self.cancel.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn is_busy(&self) -> bool {
        lock_submission(&self.submission).busy
    }

    pub fn try_event(&self) -> Result<Option<RuntimeEvent>, RuntimeError> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(RuntimeError::WorkerStopped),
        }
    }
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

fn worker_loop(
    repo: PathBuf,
    commands: Receiver<RuntimeCommand>,
    events: Sender<RuntimeEvent>,
    cancel: Arc<AtomicBool>,
    submission: Arc<Mutex<SubmissionState>>,
) {
    let mut state = match RuntimeState::load(repo) {
        Ok(state) => state,
        Err(error) => {
            let _ = events.send(RuntimeEvent::Failed(error.to_string()));
            mark_idle(&submission, true);
            return;
        }
    };
    let _ = events.send(state.settings_event());
    while let Ok(command) = commands.recv() {
        match command {
            RuntimeCommand::Submit(draft) => {
                let _ = events.send(RuntimeEvent::Started);
                let outcome = run_prompt(&mut state, draft, &events, &cancel, &submission);
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
            RuntimeCommand::Shutdown => break,
        }
    }
    mark_idle(&submission, true);
}

fn cancel_requested(cancel: &AtomicBool, submission: &Arc<Mutex<SubmissionState>>) -> bool {
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

fn take_followups(submission: &Arc<Mutex<SubmissionState>>) -> Vec<PromptDraft> {
    lock_submission(submission).followups.drain(..).collect()
}

fn finish_or_take_followups(submission: &Arc<Mutex<SubmissionState>>) -> Vec<PromptDraft> {
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
}

impl RuntimeState {
    fn load(repo: PathBuf) -> Result<Self, RuntimeError> {
        let project = repo.join(".medusa/config.toml");
        let project = project.exists().then_some(project);
        let config =
            Config::load_layers(None, project.as_deref(), &BTreeMap::new(), &BTreeMap::new())
                .map_err(RuntimeError::agent)?;
        Ok(Self {
            repo,
            base_config: config.clone(),
            effort: effort_for_turns(config.agent.max_turns),
            plan_mode: config.agent.mode == Mode::ReadOnly,
            config,
            session: None,
            pending_goal: None,
            pending_skill: None,
            session_api_key: None,
        })
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
        }
    }
}

fn run_prompt(
    state: &mut RuntimeState,
    draft: PromptDraft,
    events: &Sender<RuntimeEvent>,
    cancel: &AtomicBool,
    submission: &Arc<Mutex<SubmissionState>>,
) -> Result<RuntimeEvent, RuntimeError> {
    let config = state.config.clone();
    let max_turns = config.agent.max_turns;
    let provider =
        MiniMaxProvider::from_config_with_api_key(&config, state.session_api_key.clone())
            .map_err(RuntimeError::agent)?;
    let engine = AgentEngine::new(provider, config);
    let selected_skill = state.pending_skill.clone();
    let skill_context = selected_skill.as_ref().map(SelectedSkill::prompt_context);
    let content = message_blocks(&draft)?;
    let mut session = match state.session.take() {
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
    let mut updates = UpdateState::new();
    if !session.plan.is_empty() {
        let _ = events.send(RuntimeEvent::Plan(session.plan.clone()));
    }

    let result = (|| {
        loop {
            if cancel_requested(cancel, submission) {
                return Ok(RuntimeEvent::Cancelled);
            }
            append_followups(&engine, &mut session, take_followups(submission))?;
            if session.turn >= max_turns {
                return Err(RuntimeError::TurnLimit(max_turns));
            }
            let outcome = engine
                .step_with_observer_and_context(&mut session, skill_context.as_deref(), |update| {
                    forward_update(update, events, &mut updates);
                })
                .map_err(RuntimeError::agent)?;
            let _ = events.send(RuntimeEvent::Progress { turn: session.turn });

            if cancel_requested(cancel, submission) {
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
    state.session = Some(session);
    result
}

fn append_followups<P: ModelProvider>(
    engine: &AgentEngine<P>,
    session: &mut AgentSession,
    drafts: Vec<PromptDraft>,
) -> Result<(), RuntimeError> {
    for draft in drafts {
        engine
            .append_user_message(session, message_blocks(&draft)?)
            .map_err(RuntimeError::agent)?;
    }
    Ok(())
}

#[cfg(test)]
fn execute_slash_command(
    state: &mut RuntimeState,
    command: SlashCommand,
    events: &Sender<RuntimeEvent>,
    cancel: &AtomicBool,
) -> Result<Option<RuntimeEvent>, RuntimeError> {
    let submission = Arc::new(Mutex::new(SubmissionState {
        busy: true,
        followups: VecDeque::new(),
    }));
    execute_slash_command_with_submission(state, command, events, cancel, &submission)
}

fn execute_slash_command_with_submission(
    state: &mut RuntimeState,
    command: SlashCommand,
    events: &Sender<RuntimeEvent>,
    cancel: &AtomicBool,
    submission: &Arc<Mutex<SubmissionState>>,
) -> Result<Option<RuntimeEvent>, RuntimeError> {
    match command {
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
            state.session = None;
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
                    return Err(RuntimeError::InvalidCommand(
                        "supported providers are minimax, anthropic, and anthropic-compatible"
                            .to_owned(),
                    ));
                }
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
    fn agent(error: impl std::fmt::Display) -> Self {
        Self::Agent(error.to_string())
    }

    fn png(error: impl std::fmt::Display) -> Self {
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
