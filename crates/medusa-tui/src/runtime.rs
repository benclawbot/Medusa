use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    env, fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
    time::Instant,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use medusa_agent::{
    AgentEngine, AgentSession, AgentUpdate, StepOutcome, compact_session, update_session_objective,
};
use medusa_config::{Config, Mode};
use medusa_protocol::EventPayload;
use medusa_provider::{ImageSource, MessageBlock, MiniMaxProvider};
use serde_json::Value;

use crate::{
    app::{TranscriptPlan, TranscriptPlanStep, TranscriptPlanStepState},
    clipboard::{ImageAttachment, PromptAttachment, PromptDraft},
    commands::{Effort, ModelCommand, SlashCommand},
};

const MAX_FILE_CONTEXT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug)]
pub enum RuntimeCommand {
    Submit(PromptDraft),
    Slash(SlashCommand),
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeEvent {
    Started,
    Activity(RuntimeActivity),
    Plan(TranscriptPlan),
    Usage {
        output_tokens: u64,
    },
    Progress {
        turn: u32,
    },
    Settings {
        model: String,
        effort: String,
        plan_mode: bool,
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

pub struct RuntimeController {
    commands: Sender<RuntimeCommand>,
    events: Receiver<RuntimeEvent>,
    cancel: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
}

impl RuntimeController {
    pub fn start(repo: PathBuf) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let busy = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let worker_busy = Arc::clone(&busy);
        thread::Builder::new()
            .name("medusa-tui-runtime".to_owned())
            .spawn(move || {
                worker_loop(repo, command_rx, event_tx, worker_cancel, worker_busy);
            })
            .expect("spawn TUI runtime worker");
        Self {
            commands: command_tx,
            events: event_rx,
            cancel,
            busy,
        }
    }

    pub fn submit(&self, draft: PromptDraft) -> Result<(), RuntimeError> {
        self.busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| RuntimeError::Busy)?;
        self.cancel.store(false, Ordering::SeqCst);
        if self.commands.send(RuntimeCommand::Submit(draft)).is_err() {
            self.busy.store(false, Ordering::SeqCst);
            return Err(RuntimeError::WorkerStopped);
        }
        Ok(())
    }

    pub fn run_command(&self, command: SlashCommand) -> Result<(), RuntimeError> {
        if self.busy.load(Ordering::SeqCst) {
            return Err(RuntimeError::Busy);
        }
        if command.runs_agent() {
            self.busy
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .map_err(|_| RuntimeError::Busy)?;
            self.cancel.store(false, Ordering::SeqCst);
        }
        if self.commands.send(RuntimeCommand::Slash(command)).is_err() {
            self.busy.store(false, Ordering::SeqCst);
            return Err(RuntimeError::WorkerStopped);
        }
        Ok(())
    }

    pub fn cancel(&self) -> bool {
        if self.busy.load(Ordering::SeqCst) {
            self.cancel.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    pub fn try_event(&self) -> Result<Option<RuntimeEvent>, RuntimeError> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(RuntimeError::WorkerStopped),
        }
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
    busy: Arc<AtomicBool>,
) {
    let mut state = match RuntimeState::load(repo) {
        Ok(state) => state,
        Err(error) => {
            let _ = events.send(RuntimeEvent::Failed(error.to_string()));
            busy.store(false, Ordering::SeqCst);
            return;
        }
    };
    let _ = events.send(state.settings_event());
    while let Ok(command) = commands.recv() {
        match command {
            RuntimeCommand::Submit(draft) => {
                let _ = events.send(RuntimeEvent::Started);
                let outcome = run_prompt(&mut state, draft, &events, &cancel);
                let event = match outcome {
                    Ok(completed) => completed,
                    Err(error) => RuntimeEvent::Failed(error.to_string()),
                };
                busy.store(false, Ordering::SeqCst);
                let _ = events.send(event);
            }
            RuntimeCommand::Slash(command) => {
                let runs_agent = command.runs_agent();
                if runs_agent {
                    let _ = events.send(RuntimeEvent::Started);
                }
                match execute_slash_command(&mut state, command, &events, &cancel) {
                    Ok(Some(event)) => {
                        if runs_agent {
                            busy.store(false, Ordering::SeqCst);
                        }
                        let _ = events.send(event);
                    }
                    Ok(None) => {
                        if runs_agent {
                            busy.store(false, Ordering::SeqCst);
                        }
                    }
                    Err(error) => {
                        if runs_agent {
                            busy.store(false, Ordering::SeqCst);
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
            RuntimeCommand::Shutdown => break,
        }
    }
    busy.store(false, Ordering::SeqCst);
}

struct RuntimeState {
    repo: PathBuf,
    base_config: Config,
    config: Config,
    session: Option<AgentSession>,
    pending_goal: Option<String>,
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
        }
    }
}

fn run_prompt(
    state: &mut RuntimeState,
    draft: PromptDraft,
    events: &Sender<RuntimeEvent>,
    cancel: &AtomicBool,
) -> Result<RuntimeEvent, RuntimeError> {
    let config = state.config.clone();
    let max_turns = config.agent.max_turns;
    let provider =
        MiniMaxProvider::from_config_with_api_key(&config, state.session_api_key.clone())
            .map_err(RuntimeError::agent)?;
    let engine = AgentEngine::new(provider, config);
    let content = message_blocks(&draft)?;
    let mut session = match state.session.take() {
        Some(mut session) => {
            if let Err(error) = engine.append_user_message(&mut session, content) {
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
    let mut updates = UpdateState::new(state.config.agent.mode);
    let _ = events.send(RuntimeEvent::Plan(plan_for(
        PlanPhase::Understand,
        state.config.agent.mode,
    )));

    let result = (|| {
        while !session.completed && session.turn < max_turns {
            if cancel.load(Ordering::SeqCst) {
                return Ok(RuntimeEvent::Cancelled);
            }
            let outcome = engine
                .step_with_observer(&mut session, |update| {
                    forward_update(update, events, &mut updates);
                })
                .map_err(RuntimeError::agent)?;
            let _ = events.send(RuntimeEvent::Progress { turn: session.turn });
            if outcome == StepOutcome::Completed {
                break;
            }
        }

        if cancel.load(Ordering::SeqCst) {
            return Ok(RuntimeEvent::Cancelled);
        }
        if !session.completed {
            return Err(RuntimeError::TurnLimit(max_turns));
        }

        Ok(RuntimeEvent::Completed {
            session_id: session.id.to_string(),
        })
    })();
    state.session = Some(session);
    result
}

fn execute_slash_command(
    state: &mut RuntimeState,
    command: SlashCommand,
    events: &Sender<RuntimeEvent>,
    cancel: &AtomicBool,
) -> Result<Option<RuntimeEvent>, RuntimeError> {
    match command {
        SlashCommand::Help => {
            let _ = events.send(RuntimeEvent::Notice {
                title: "Slash commands".to_owned(),
                details: crate::commands::COMMAND_SPECS
                    .iter()
                    .map(|spec| format!("{} - {}", spec.usage, spec.description))
                    .collect(),
            });
        }
        SlashCommand::New => {
            state.session = None;
            state.pending_goal = None;
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
                    details: vec![objective],
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
                    skills
                },
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

fn effort_for_turns(max_turns: u32) -> Effort {
    match max_turns {
        0..=99 => Effort::Low,
        100..=299 => Effort::Medium,
        _ => Effort::High,
    }
}

fn turns_for_effort(effort: Effort) -> u32 {
    match effort {
        Effort::Low => 64,
        Effort::Medium => 200,
        Effort::High => 500,
        Effort::Auto => unreachable!("auto resolves to the configured default"),
    }
}

fn is_supported_provider(provider: &str) -> bool {
    matches!(provider, "minimax" | "anthropic" | "anthropic-compatible")
}

fn model_configuration_details(state: &RuntimeState) -> Vec<String> {
    let credential = if state.session_api_key.is_some()
        || credential_environment(&state.config.model.provider)
            .is_some_and(|name| env::var(name).is_ok())
    {
        "credential: configured"
    } else {
        "credential: missing"
    };
    vec![
        format!("provider: {}", state.config.model.provider),
        format!("model: {}", state.config.model.name),
        credential.to_owned(),
        "set provider: /model provider <minimax|anthropic|anthropic-compatible>".to_owned(),
        "set model: /model <model-name>".to_owned(),
        "set session key: /model key <api-key>".to_owned(),
    ]
}

fn credential_environment(provider: &str) -> Option<&'static str> {
    match provider {
        "minimax" => Some("MINIMAX_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "anthropic-compatible" => Some("MEDUSA_API_KEY"),
        _ => None,
    }
}

fn discover_skills(repo: &Path) -> Vec<String> {
    let mut roots = vec![
        ("project", repo.join(".medusa/skills")),
        ("project", repo.join(".claude/skills")),
    ];
    if let Some(home) = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
        roots.push(("user", home.join(".medusa/skills")));
        roots.push(("user", home.join(".claude/skills")));
    }
    let mut skills = BTreeSet::new();
    for (scope, root) in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let skill = path.join("SKILL.md");
            if skill.is_file() {
                let description = skill_description(&skill);
                skills.insert(format!(
                    "{} ({scope}){}",
                    entry.file_name().to_string_lossy(),
                    description
                        .map(|description| format!(" - {description}"))
                        .unwrap_or_default()
                ));
            }
        }
    }
    skills.into_iter().collect()
}

fn skill_description(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().and_then(|text| {
        text.lines().find_map(|line| {
            line.strip_prefix("description:")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.trim_matches('"').to_owned())
        })
    })
}

struct UpdateState {
    next_tool_id: u64,
    pending_tools: VecDeque<PendingTool>,
    plan_phase: PlanPhase,
    mode: Mode,
}

impl UpdateState {
    fn new(mode: Mode) -> Self {
        Self {
            next_tool_id: 0,
            pending_tools: VecDeque::new(),
            plan_phase: PlanPhase::Understand,
            mode,
        }
    }
}

struct PendingTool {
    id: String,
    tool: String,
    title: String,
    details: Vec<String>,
    started_at: Instant,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum PlanPhase {
    #[default]
    Understand,
    Inspect,
    Implement,
    Verify,
    Completed,
    Failed,
}

fn forward_update(update: &AgentUpdate, events: &Sender<RuntimeEvent>, state: &mut UpdateState) {
    match update {
        AgentUpdate::Event(EventPayload::ModelResponseReceived { usage, .. }) => {
            if let Some(output_tokens) = usage.get("output_tokens").and_then(Value::as_u64) {
                let _ = events.send(RuntimeEvent::Usage { output_tokens });
            }
        }
        AgentUpdate::Event(EventPayload::ToolCallRequested { tool, arguments }) => {
            publish_plan(events, state, plan_phase_for_tool(tool));
            state.next_tool_id = state.next_tool_id.saturating_add(1);
            let pending = PendingTool {
                id: format!("tool-{}", state.next_tool_id),
                tool: tool.clone(),
                title: tool_title(tool, arguments),
                details: tool_details(arguments),
                started_at: Instant::now(),
            };
            let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                id: Some(pending.id.clone()),
                kind: RuntimeActivityKind::Tool,
                title: pending.title.clone(),
                details: pending.details.clone(),
            }));
            state.pending_tools.push_back(pending);
        }
        AgentUpdate::Event(EventPayload::VerificationStarted { .. }) => {
            publish_plan(events, state, PlanPhase::Verify);
        }
        AgentUpdate::Event(EventPayload::VerificationCompleted { passed, evidence }) => {
            publish_plan(
                events,
                state,
                if *passed {
                    PlanPhase::Completed
                } else {
                    PlanPhase::Failed
                },
            );
            let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                id: None,
                kind: RuntimeActivityKind::Verification,
                title: if *passed {
                    "Verify fixes".to_owned()
                } else {
                    "Verification failed".to_owned()
                },
                details: evidence.iter().map(|line| summarize(line)).collect(),
            }));
        }
        AgentUpdate::AssistantText(text) => {
            let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
            if let Some(title) = lines.next() {
                let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                    id: None,
                    kind: RuntimeActivityKind::Assistant,
                    title: summarize(title),
                    details: lines.map(summarize).collect(),
                }));
            }
        }
        AgentUpdate::ToolOutput {
            tool,
            output,
            is_error,
        } => {
            let pending = state
                .pending_tools
                .iter()
                .position(|pending| pending.tool == *tool)
                .and_then(|index| state.pending_tools.remove(index));
            let mut activity = pending.map_or_else(
                || RuntimeActivity {
                    id: None,
                    kind: RuntimeActivityKind::Tool,
                    title: tool.clone(),
                    details: Vec::new(),
                },
                |pending| {
                    let mut details = pending.details;
                    let elapsed = pending.started_at.elapsed().as_secs_f32();
                    details.push(if *is_error {
                        format!("failed after {elapsed:.1}s")
                    } else {
                        format!("completed in {elapsed:.1}s")
                    });
                    RuntimeActivity {
                        id: Some(pending.id),
                        kind: RuntimeActivityKind::Tool,
                        title: pending.title,
                        details,
                    }
                },
            );
            activity.details.extend(tool_output_details(output));
            if *is_error {
                activity.kind = RuntimeActivityKind::Error;
            }
            let _ = events.send(RuntimeEvent::Activity(activity));
        }
        _ => {}
    }
}

fn publish_plan(events: &Sender<RuntimeEvent>, state: &mut UpdateState, phase: PlanPhase) {
    if state.plan_phase != phase {
        state.plan_phase = phase;
        let _ = events.send(RuntimeEvent::Plan(plan_for(phase, state.mode)));
    }
}

fn plan_phase_for_tool(tool: &str) -> PlanPhase {
    match tool {
        "fs_read" | "search_text" | "code_index" => PlanPhase::Inspect,
        _ => PlanPhase::Implement,
    }
}

fn plan_for(phase: PlanPhase, mode: Mode) -> TranscriptPlan {
    use TranscriptPlanStepState::{Active, Completed, Failed, Pending};
    if mode == Mode::ReadOnly {
        let states = match phase {
            PlanPhase::Understand => [Active, Pending, Pending],
            PlanPhase::Inspect => [Completed, Active, Pending],
            PlanPhase::Implement | PlanPhase::Verify => [Completed, Completed, Active],
            PlanPhase::Completed => [Completed, Completed, Completed],
            PlanPhase::Failed => [Completed, Completed, Failed],
        };
        return TranscriptPlan {
            steps: [
                "Understand the task",
                "Inspect the codebase",
                "Draft the plan",
            ]
            .into_iter()
            .zip(states)
            .map(|(title, state)| TranscriptPlanStep {
                title: title.to_owned(),
                state,
            })
            .collect(),
        };
    }
    let states = match phase {
        PlanPhase::Understand => [Active, Pending, Pending, Pending],
        PlanPhase::Inspect => [Completed, Active, Pending, Pending],
        PlanPhase::Implement => [Completed, Completed, Active, Pending],
        PlanPhase::Verify => [Completed, Completed, Completed, Active],
        PlanPhase::Completed => [Completed, Completed, Completed, Completed],
        PlanPhase::Failed => [Completed, Completed, Completed, Failed],
    };
    TranscriptPlan {
        steps: [
            "Understand the task",
            "Inspect the codebase",
            "Implement the change",
            "Verify the result",
        ]
        .into_iter()
        .zip(states)
        .map(|(title, state)| TranscriptPlanStep {
            title: title.to_owned(),
            state,
        })
        .collect(),
    }
}

fn tool_title(tool: &str, arguments: &Value) -> String {
    match tool {
        "fs_read" => format!("Read({})", json_string(arguments, "path")),
        "fs_write" => format!("Write({})", json_string(arguments, "path")),
        "search_text" => format!("Search({})", json_string(arguments, "query")),
        "code_index" => {
            let name = json_string(arguments, "name");
            if name.is_empty() {
                "Index repository".to_owned()
            } else {
                format!("Index({name})")
            }
        }
        "patch_apply" => "Edit files".to_owned(),
        "symbol_rename" => format!(
            "Rename({} -> {})",
            json_string(arguments, "old_name"),
            json_string(arguments, "new_name")
        ),
        "shell_run" => format!("Bash({})", shell_command(arguments)),
        "git_checkpoint" => format!("Checkpoint({})", json_string(arguments, "message")),
        _ => tool.to_owned(),
    }
}

fn tool_details(arguments: &Value) -> Vec<String> {
    match arguments {
        Value::Object(map) => map
            .iter()
            .filter_map(|(key, value)| {
                if key == "content" || key == "replacement" || key == "expected" {
                    None
                } else {
                    Some(format!("{key}: {}", summarize(&value_to_display(value))))
                }
            })
            .take(3)
            .collect(),
        _ => Vec::new(),
    }
}

fn tool_output_details(output: &str) -> Vec<String> {
    let lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(summarize)
        .collect::<Vec<_>>();
    let mut preview = lines.iter().take(3).cloned().collect::<Vec<_>>();
    if lines.len() > preview.len() {
        preview.push(format!("... +{} lines", lines.len() - preview.len()));
    }
    preview
}

fn json_string(arguments: &Value, key: &str) -> String {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn shell_command(arguments: &Value) -> String {
    let program = json_string(arguments, "program");
    let args = arguments
        .get("args")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    if args.is_empty() {
        program
    } else {
        format!("{program} {args}")
    }
}

fn value_to_display(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn summarize(value: &str) -> String {
    let compact = value.replace('\n', " ");
    if compact.chars().count() <= 140 {
        return compact;
    }
    compact.chars().take(137).chain("...".chars()).collect()
}

fn objective_for(draft: &PromptDraft) -> String {
    let trimmed = draft.text.trim();
    if trimmed.is_empty() {
        format!(
            "Use the {} attached item(s) as context and complete the coding task.",
            draft.attachments.len()
        )
    } else {
        trimmed.to_owned()
    }
}

pub fn message_blocks(draft: &PromptDraft) -> Result<Vec<MessageBlock>, RuntimeError> {
    let mut blocks = Vec::new();
    if !draft.text.is_empty() {
        blocks.push(MessageBlock::Text {
            text: draft.text.clone(),
        });
    }
    for attachment in &draft.attachments {
        match attachment {
            PromptAttachment::PastedText(text) => blocks.push(MessageBlock::Text {
                text: format!(
                    "<pasted_text name=\"{}\">\n{}\n</pasted_text>",
                    text.display_name, text.text
                ),
            }),
            PromptAttachment::Image(image) => blocks.push(image_block(image)?),
            PromptAttachment::File(file) => {
                let bytes = fs::read(&file.path)?;
                if bytes.len() > MAX_FILE_CONTEXT_BYTES {
                    return Err(RuntimeError::FileTooLarge {
                        path: file.path.clone(),
                        bytes: bytes.len(),
                    });
                }
                let text = String::from_utf8(bytes).map_err(|_| RuntimeError::BinaryFile {
                    path: file.path.clone(),
                })?;
                blocks.push(MessageBlock::Text {
                    text: format!(
                        "<attached_file path=\"{}\">\n{}\n</attached_file>",
                        file.path.display(),
                        text
                    ),
                });
            }
        }
    }
    if blocks.is_empty() {
        return Err(RuntimeError::EmptyPrompt);
    }
    Ok(blocks)
}

fn image_block(image: &ImageAttachment) -> Result<MessageBlock, RuntimeError> {
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, image.width, image.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(RuntimeError::png)?;
        writer
            .write_image_data(&image.rgba)
            .map_err(RuntimeError::png)?;
    }
    Ok(MessageBlock::Image {
        source: ImageSource::Base64 {
            media_type: "image/png".to_owned(),
            data: STANDARD.encode(encoded),
        },
        alt_text: Some(image.display_name.clone()),
    })
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

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use medusa_agent::AgentUpdate;
    use medusa_protocol::EventPayload;
    use serde_json::json;

    use super::*;
    use crate::clipboard::{ImageAttachment, PromptAttachment};
    use tempfile::tempdir;

    #[test]
    fn text_prompt_becomes_user_message_block() {
        let draft = PromptDraft {
            text: "fix the failing test".to_owned(),
            ..PromptDraft::default()
        };
        assert_eq!(
            message_blocks(&draft).expect("message blocks"),
            vec![MessageBlock::Text {
                text: "fix the failing test".to_owned()
            }]
        );
    }

    #[test]
    fn screenshot_is_encoded_as_png_image_block() {
        let draft = PromptDraft {
            attachments: vec![PromptAttachment::Image(ImageAttachment {
                display_name: "screen.png".to_owned(),
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 255],
                source_format: Some("image/rgba8".to_owned()),
            })],
            ..PromptDraft::default()
        };
        let blocks = message_blocks(&draft).expect("message blocks");
        assert!(matches!(
            &blocks[0],
            MessageBlock::Image {
                source: ImageSource::Base64 { media_type, data },
                ..
            } if media_type == "image/png" && !data.is_empty()
        ));
    }

    #[test]
    fn attached_utf8_file_is_bounded_and_included() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("error.txt");
        fs::write(&path, "compiler error").expect("write fixture");
        let draft = PromptDraft {
            attachments: vec![PromptAttachment::File(crate::clipboard::FileAttachment {
                path,
                byte_len: 14,
            })],
            ..PromptDraft::default()
        };
        let blocks = message_blocks(&draft).expect("message blocks");
        assert!(matches!(
            &blocks[0],
            MessageBlock::Text { text } if text.contains("compiler error")
        ));
    }

    #[test]
    fn tool_call_is_shown_before_its_result_updates_the_same_row() {
        let (sender, receiver) = mpsc::channel();
        let mut state = UpdateState::new(Mode::Yolo);
        forward_update(
            &AgentUpdate::Event(EventPayload::ToolCallRequested {
                tool: "fs_read".to_owned(),
                arguments: json!({"path": "src/lib.rs"}),
            }),
            &sender,
            &mut state,
        );

        assert!(matches!(
            receiver.recv().expect("plan update"),
            RuntimeEvent::Plan(_)
        ));
        let started = match receiver.recv().expect("tool start") {
            RuntimeEvent::Activity(activity) => activity,
            other => panic!("expected tool activity, received {other:?}"),
        };

        forward_update(
            &AgentUpdate::ToolOutput {
                tool: "fs_read".to_owned(),
                output: "line one\nline two".to_owned(),
                is_error: false,
            },
            &sender,
            &mut state,
        );

        let completed = match receiver.recv().expect("tool result") {
            RuntimeEvent::Activity(activity) => activity,
            other => panic!("expected tool activity, received {other:?}"),
        };
        assert_eq!(started.id, completed.id);
        assert!(
            completed
                .details
                .iter()
                .any(|detail| detail.contains("completed in"))
        );
        assert!(completed.details.iter().any(|detail| detail == "line one"));
    }

    #[test]
    fn idle_runtime_cancel_is_a_noop() {
        let directory = tempdir().expect("temporary directory");
        let runtime = RuntimeController::start(directory.path().to_path_buf());
        assert!(!runtime.cancel());
        assert!(!runtime.is_busy());
    }

    #[test]
    fn model_configuration_redacts_session_api_keys() {
        let directory = tempdir().expect("temporary directory");
        let mut state = RuntimeState::load(directory.path().to_path_buf()).expect("runtime state");
        state.session_api_key = Some("secret-value".to_owned());
        let details = model_configuration_details(&state).join("\n");
        assert!(details.contains("credential: configured"));
        assert!(!details.contains("secret-value"));
    }

    #[test]
    fn effort_command_updates_the_real_turn_budget() {
        let directory = tempdir().expect("temporary directory");
        let mut state = RuntimeState::load(directory.path().to_path_buf()).expect("runtime state");
        let (sender, receiver) = mpsc::channel();
        execute_slash_command(
            &mut state,
            SlashCommand::Effort {
                effort: Some(Effort::Medium),
            },
            &sender,
            &AtomicBool::new(false),
        )
        .expect("set effort");
        assert_eq!(state.config.agent.max_turns, 200);
        assert!(matches!(
            receiver.recv().expect("settings update"),
            RuntimeEvent::Settings { effort, .. } if effort == "effort:medium"
        ));
    }

    #[test]
    fn skills_command_discovers_project_skill_metadata() {
        let directory = tempdir().expect("temporary directory");
        let skill = directory.path().join(".claude/skills/release/SKILL.md");
        fs::create_dir_all(skill.parent().expect("skill directory")).expect("create skills");
        fs::write(
            &skill,
            "---\nname: release\ndescription: Prepare a release\n---\nBody",
        )
        .expect("write skill");
        assert!(
            discover_skills(directory.path())
                .iter()
                .any(|skill| skill == "release (project) - Prepare a release")
        );
    }

    #[test]
    fn planning_mode_uses_a_read_only_three_step_plan() {
        let plan = plan_for(PlanPhase::Understand, Mode::ReadOnly);
        assert_eq!(plan.steps.len(), 3);
        assert_eq!(plan.steps[2].title, "Draft the plan");
    }
}
