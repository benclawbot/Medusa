pub mod app;
pub mod clipboard;
pub mod commands;
pub mod draft_store;
pub mod input;
pub mod native_clipboard;
pub mod runtime;

use std::{
    collections::BTreeMap,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[cfg(unix)]
use std::thread;

use app::{
    AppAction, AppError, AppState, TranscriptActivity, TranscriptActivityKind, TranscriptEntry,
};
use clipboard::{ClipboardService, PromptAttachment, PromptDraft, UnsupportedClipboard};
use commands::command_suggestions;
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode, size,
    },
};
use medusa_config::Config;
use native_clipboard::NativeClipboard;
use runtime::{RuntimeActivityKind, RuntimeController, RuntimeEvent};

const MEDUSA_LOGO: [&str; 3] = [
    "╭┬╮╭─╴╶┬╮╷ ╷╭─╮╭─╮",
    "│││├╴  │││ │╰─╮├─┤",
    "╵ ╵╰─╴╶┴╯╰─╯╰─╯╵ ╵",
];
const HEADER_TOP_PADDING: u16 = 1;

#[cfg(unix)]
use medusa_daemon::{DaemonClient, JobRecord, Request, Response};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TuiOptions {
    pub repo: PathBuf,
    pub socket: Option<PathBuf>,
    pub initial_prompt: Option<String>,
    pub resume_session: Option<String>,
    pub continue_latest: bool,
}

impl TuiOptions {
    #[must_use]
    pub fn for_repo(repo: impl Into<PathBuf>) -> Self {
        Self {
            repo: repo.into(),
            socket: None,
            initial_prompt: None,
            resume_session: None,
            continue_latest: false,
        }
    }

    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        self.socket
            .clone()
            .unwrap_or_else(|| self.repo.join(".medusa/daemon/medusa.sock"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitReason {
    UserQuit,
    InputClosed,
}

pub fn run(options: TuiOptions) -> io::Result<ExitReason> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "interactive Medusa requires a TTY; use `medusa run` for headless execution",
        ));
    }

    let clipboard: Arc<dyn ClipboardService> = NativeClipboard::new()
        .map(|service| Arc::new(service) as Arc<dyn ClipboardService>)
        .unwrap_or_else(|_| Arc::new(UnsupportedClipboard));
    let draft_key = options
        .resume_session
        .clone()
        .unwrap_or_else(|| "current".to_owned());
    let mut app = AppState::new(
        options.repo.clone(),
        draft_key,
        options.initial_prompt.clone().unwrap_or_default(),
        clipboard,
    )?;
    let identity = UiIdentity::for_repo(&options.repo);
    let runtime = RuntimeController::start(options.repo.clone());
    let mut terminal = TerminalGuard::enter()?;
    run_loop(terminal.stdout(), &options, &identity, &mut app, &runtime)
}

struct TerminalGuard {
    stdout: io::Stdout,
    active: bool,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, Hide) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self {
            stdout,
            active: true,
        })
    }

    fn stdout(&mut self) -> &mut io::Stdout {
        &mut self.stdout
    }

    fn restore(&mut self) {
        if !self.active {
            return;
        }
        let _ = disable_raw_mode();
        let _ = execute!(
            self.stdout,
            DisableBracketedPaste,
            Show,
            LeaveAlternateScreen
        );
        self.active = false;
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(unix)]
fn run_loop(
    stdout: &mut io::Stdout,
    options: &TuiOptions,
    identity: &UiIdentity,
    app: &mut AppState,
    runtime: &RuntimeController,
) -> io::Result<ExitReason> {
    let client = DaemonClient::new(options.socket_path());
    loop {
        drain_runtime_events(app, runtime)?;
        let (jobs, daemon_status) = match client.request(Request::List) {
            Ok(Response::Jobs { jobs }) => (jobs, "connected".to_owned()),
            Ok(other) => (Vec::new(), format!("unexpected response: {other:?}")),
            Err(error) => (Vec::new(), format!("disconnected: {error}")),
        };
        draw(stdout, options, identity, app, &jobs, &daemon_status)?;
        if event::poll(Duration::from_millis(100))? {
            let terminal_event = event::read()?;
            if ctrl_l_redraw(&terminal_event) {
                continue;
            }
            if ctrl_d_on_empty(&terminal_event, app) {
                return Ok(ExitReason::InputClosed);
            }
            if handle_app_action(app, runtime, terminal_event)? {
                return Ok(ExitReason::UserQuit);
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(not(unix))]
fn run_loop(
    stdout: &mut io::Stdout,
    _options: &TuiOptions,
    identity: &UiIdentity,
    app: &mut AppState,
    runtime: &RuntimeController,
) -> io::Result<ExitReason> {
    let mut last_frame: Option<Vec<StyledLine>> = None;
    loop {
        drain_runtime_events(app, runtime)?;
        app.tick();
        let (width, height) = size()?;
        let frame = render_frame(identity, app, width, height);
        if last_frame.as_ref() != Some(&frame) {
            draw_frame(stdout, width, &frame, last_frame.as_deref())?;
            stdout.flush()?;
            last_frame = Some(frame);
        }
        if event::poll(Duration::from_millis(100))? {
            let terminal_event = event::read()?;
            if matches!(terminal_event, Event::Resize(_, _)) {
                last_frame = None;
            }
            if ctrl_l_redraw(&terminal_event) {
                last_frame = None;
                continue;
            }
            if ctrl_d_on_empty(&terminal_event, app) {
                return Ok(ExitReason::InputClosed);
            }
            if handle_app_action(app, runtime, terminal_event)? {
                return Ok(ExitReason::UserQuit);
            }
        }
    }
}

fn handle_app_action(
    app: &mut AppState,
    runtime: &RuntimeController,
    terminal_event: Event,
) -> io::Result<bool> {
    match app.handle_event(terminal_event).map_err(app_error)? {
        AppAction::Quit => Ok(true),
        AppAction::Interrupt => {
            app.status = if runtime.cancel() {
                "cancellation requested".to_owned()
            } else {
                "no running task to cancel".to_owned()
            };
            Ok(false)
        }
        AppAction::Submit(draft) => {
            let bytes = draft.text.len();
            let attachments = draft.attachments.len();
            match runtime.submit(draft) {
                Ok(()) => {
                    app.status =
                        format!("running prompt: {bytes} bytes, {attachments} attachment(s)");
                }
                Err(error) => {
                    app.transcript
                        .push(TranscriptEntry::System(format!("error: {error}")));
                    app.status = "submission rejected".to_owned();
                }
            }
            Ok(false)
        }
        AppAction::AnswerQuestion(answer) => {
            let draft = PromptDraft {
                text: answer,
                ..PromptDraft::default()
            };
            match runtime.submit(draft) {
                Ok(()) => {
                    app.status = "continuing with your answer".to_owned();
                }
                Err(error) => {
                    app.transcript
                        .push(TranscriptEntry::System(format!("error: {error}")));
                    app.status = "answer rejected".to_owned();
                }
            }
            Ok(false)
        }
        AppAction::Command(command) => {
            match runtime.run_command(command) {
                Ok(()) => {
                    app.status = "command running".to_owned();
                }
                Err(error) => {
                    app.transcript
                        .push(TranscriptEntry::System(format!("error: {error}")));
                    app.status = "command rejected".to_owned();
                }
            }
            Ok(false)
        }
        AppAction::ConfigureModel(configuration) => {
            match runtime.configure_model(configuration) {
                Ok(()) => {
                    app.status = "updating model configuration".to_owned();
                }
                Err(error) => {
                    app.transcript
                        .push(TranscriptEntry::System(format!("error: {error}")));
                    app.status = "model configuration rejected".to_owned();
                }
            }
            Ok(false)
        }
        AppAction::None | AppAction::Redraw => Ok(false),
    }
}

fn drain_runtime_events(app: &mut AppState, runtime: &RuntimeController) -> io::Result<()> {
    while let Some(event) = runtime.try_event().map_err(runtime_error)? {
        match event {
            RuntimeEvent::Started => {
                app.begin_run();
            }
            RuntimeEvent::Activity(activity) => {
                app.record_activity(TranscriptActivity {
                    id: activity.id,
                    kind: match activity.kind {
                        RuntimeActivityKind::Assistant => TranscriptActivityKind::Assistant,
                        RuntimeActivityKind::Done => TranscriptActivityKind::Done,
                        RuntimeActivityKind::Error => TranscriptActivityKind::Error,
                        RuntimeActivityKind::Tool => TranscriptActivityKind::Tool,
                        RuntimeActivityKind::Verification => TranscriptActivityKind::Verification,
                    },
                    title: activity.title,
                    details: activity.details,
                });
            }
            RuntimeEvent::Plan(plan) => {
                app.set_plan(plan);
            }
            RuntimeEvent::Question(question) => {
                app.open_question(question.question, question.options);
            }
            RuntimeEvent::Usage { output_tokens } => {
                app.add_output_tokens(output_tokens);
            }
            RuntimeEvent::Progress { turn } => {
                app.update_turn(turn);
            }
            RuntimeEvent::Settings {
                model,
                effort,
                plan_mode,
                credential_configured,
            } => {
                app.set_runtime_settings(model, effort, plan_mode, credential_configured);
            }
            RuntimeEvent::Notice { title, details } => {
                let status = title.to_ascii_lowercase();
                app.record_activity(TranscriptActivity {
                    id: None,
                    kind: TranscriptActivityKind::Progress,
                    title,
                    details,
                });
                app.status = status;
            }
            RuntimeEvent::NewSession => {
                app.clear_for_new_session();
            }
            RuntimeEvent::Compacted { message } => {
                app.compact_transcript(message);
            }
            RuntimeEvent::Completed { session_id } => {
                app.record_activity(TranscriptActivity {
                    id: None,
                    kind: TranscriptActivityKind::Done,
                    title: "Task completed".to_owned(),
                    details: vec![format!("session {session_id}")],
                });
                app.status = "completed".to_owned();
                app.finish_run();
            }
            RuntimeEvent::Cancelled => {
                app.record_activity(TranscriptActivity {
                    id: None,
                    kind: TranscriptActivityKind::Done,
                    title: "Task cancelled".to_owned(),
                    details: Vec::new(),
                });
                app.status = "cancelled".to_owned();
                app.finish_run();
            }
            RuntimeEvent::Failed(error) => {
                app.record_activity(TranscriptActivity {
                    id: None,
                    kind: TranscriptActivityKind::Error,
                    title: "Task failed".to_owned(),
                    details: vec![error],
                });
                app.status = "agent failed".to_owned();
                app.finish_run();
            }
        }
    }
    Ok(())
}

fn ctrl_d_on_empty(event: &Event, app: &AppState) -> bool {
    matches!(
        event,
        Event::Key(key)
            if key.kind == KeyEventKind::Press
                && key.code == KeyCode::Char('d')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && app.composer.draft.text.is_empty()
                && app.composer.draft.attachments.is_empty()
    )
}

fn ctrl_l_redraw(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(key)
            if key.kind == KeyEventKind::Press
                && key.code == KeyCode::Char('l')
                && key.modifiers.contains(KeyModifiers::CONTROL)
    )
}

#[cfg(unix)]
fn draw(
    stdout: &mut io::Stdout,
    _options: &TuiOptions,
    identity: &UiIdentity,
    app: &AppState,
    _jobs: &[JobRecord],
    _daemon_status: &str,
) -> io::Result<()> {
    draw_common(stdout, identity, app)
}

#[cfg(not(unix))]
#[allow(dead_code)]
fn draw_portable(
    stdout: &mut io::Stdout,
    _options: &TuiOptions,
    identity: &UiIdentity,
    app: &AppState,
) -> io::Result<()> {
    let (width, height) = size()?;
    draw_frame(
        stdout,
        width,
        &render_frame(identity, app, width, height),
        None,
    )?;
    stdout.flush()
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct PortableRenderSnapshot {
    terminal_size: (u16, u16),
    status: String,
    transcript: Vec<TranscriptEntry>,
    plan: Option<app::TranscriptPlan>,
    token_count: u64,
    elapsed_seconds: Option<u64>,
    draft: PromptDraft,
    command_selection: usize,
    model_label: Option<String>,
    effort_label: Option<String>,
    plan_mode: bool,
    spinner_frame: u8,
    model_modal: Option<app::ModelModal>,
}

#[cfg(test)]
fn portable_render_snapshot(app: &AppState, terminal_size: (u16, u16)) -> PortableRenderSnapshot {
    PortableRenderSnapshot {
        terminal_size,
        status: app.status.clone(),
        transcript: app.transcript.clone(),
        plan: app.plan.clone(),
        token_count: app.token_count,
        elapsed_seconds: app.elapsed_seconds(),
        draft: app.composer.draft.clone(),
        command_selection: app.command_selection,
        model_label: app.model_label.clone(),
        effort_label: app.effort_label.clone(),
        plan_mode: app.plan_mode,
        spinner_frame: app.spinner_frame,
        model_modal: app.model_modal().cloned(),
    }
}

fn running_status(app: &AppState) -> String {
    format!(
        "{} ({} · ↑ {} tokens)",
        app.status,
        format_elapsed(app.elapsed_seconds().unwrap_or_default()),
        format_token_count(app.token_count)
    )
}

fn format_elapsed(seconds: u64) -> String {
    let minutes = seconds / 60;
    if minutes == 0 {
        return format!("{seconds}s");
    }
    format!("{minutes}m {}s", seconds % 60)
}

fn format_token_count(tokens: u64) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }
    format!("{:.1}k", tokens as f64 / 1_000.0)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct UiIdentity {
    model: String,
    effort: String,
}

impl UiIdentity {
    fn for_repo(repo: &Path) -> Self {
        let project = repo.join(".medusa/config.toml");
        let project = project.exists().then_some(project);
        let config =
            Config::load_layers(None, project.as_deref(), &BTreeMap::new(), &BTreeMap::new())
                .unwrap_or_default();
        Self {
            model: config.model.name,
            effort: effort_label(config.agent.max_turns).to_owned(),
        }
    }
}

fn effort_label(max_turns: u32) -> &'static str {
    match max_turns {
        0..=99 => "effort:low",
        100..=299 => "effort:medium",
        _ => "effort:high",
    }
}

#[cfg(unix)]
fn draw_common(stdout: &mut io::Stdout, identity: &UiIdentity, app: &AppState) -> io::Result<()> {
    let (width, height) = size()?;
    let frame = render_frame(identity, app, width, height);
    draw_frame(stdout, width, &frame, None)?;
    stdout.flush()
}

#[allow(dead_code)]
fn legacy_draw_common(
    stdout: &mut io::Stdout,
    identity: &UiIdentity,
    app: &AppState,
) -> io::Result<()> {
    let (width, height) = size()?;
    queue!(
        stdout,
        MoveTo(0, 0),
        Clear(ClearType::CurrentLine),
        MoveTo(0, HEADER_TOP_PADDING)
    )?;
    for logo_line in MEDUSA_LOGO {
        print_styled_line(stdout, width, logo_line, Color::Cyan, Attribute::Bold)?;
    }
    queue!(
        stdout,
        Clear(ClearType::UntilNewLine),
        SetForegroundColor(Color::Magenta),
        SetAttribute(Attribute::Bold),
        Print(truncate(
            &format!(
                "{} {}",
                app.model_label.as_deref().unwrap_or(&identity.model),
                app.effort_label.as_deref().unwrap_or(&identity.effort)
            ),
            width
        )),
        SetAttribute(Attribute::Reset),
        ResetColor,
        Print("\r\n"),
    )?;
    let header_height = HEADER_TOP_PADDING + 4;
    let model_modal = app.model_modal();
    let modal_lines = model_modal.map(model_modal_lines).unwrap_or_default();
    let suggestions = model_modal
        .is_none()
        .then(|| command_suggestions(&app.composer.draft.text))
        .unwrap_or_default();
    let available_suggestion_rows = height.saturating_sub(header_height.saturating_add(4));
    let visible_suggestions = suggestions
        .iter()
        .take(usize::from(available_suggestion_rows))
        .collect::<Vec<_>>();
    let requested_composer_height = if model_modal.is_some() {
        3_u16.saturating_add(u16::try_from(modal_lines.len()).unwrap_or(u16::MAX))
    } else {
        4_u16.saturating_add(u16::try_from(visible_suggestions.len()).unwrap_or(u16::MAX))
    };
    let composer_height = requested_composer_height.min(height.saturating_sub(header_height));
    let content_rows = height.saturating_sub(composer_height + header_height) as usize;
    let mut lines = Vec::new();
    for entry in &app.transcript {
        match entry {
            TranscriptEntry::User(draft) => {
                lines.push(StyledLine::with_marker(
                    "> ",
                    Color::Cyan,
                    draft.text.replace('\n', " / "),
                    Color::White,
                ));
                lines.extend(draft.attachments.iter().map(|attachment| {
                    StyledLine::new(
                        format!("  └ {}", attachment_label(attachment)),
                        Color::DarkGrey,
                    )
                }));
            }
            TranscriptEntry::Activity(activity) => lines.extend(activity_lines(activity)),
            TranscriptEntry::System(message) => lines.push(system_line(message)),
        }
    }
    if app.is_running() {
        lines.push(StyledLine::with_marker(
            spinner_marker(app.spinner_frame),
            Color::Magenta,
            running_status(app),
            Color::Grey,
        ));
    }
    if let Some(plan) = &app.plan {
        lines.extend(plan_lines(plan));
    }
    let visible_content = lines
        .iter()
        .rev()
        .take(content_rows)
        .rev()
        .collect::<Vec<_>>();
    for line in &visible_content {
        line.print(stdout, width)?;
    }
    for _ in visible_content.len()..content_rows {
        queue!(stdout, Clear(ClearType::UntilNewLine), Print("\r\n"))?;
    }

    let composer_top = height.saturating_sub(composer_height);
    queue!(
        stdout,
        MoveTo(0, composer_top),
        SetForegroundColor(Color::DarkGrey),
        Print("─".repeat(width as usize)),
        ResetColor,
        Print("\r\n")
    )?;
    if model_modal.is_some() {
        let available_modal_rows = composer_height.saturating_sub(3);
        for line in modal_lines.iter().take(usize::from(available_modal_rows)) {
            line.print(stdout, width)?;
        }
        print_separator(stdout, width)?;
        StyledLine::with_marker(
            "› ",
            Color::Magenta,
            "up/down choose · tab focus · enter set for this session · esc cancel",
            Color::DarkGrey,
        )
        .print(stdout, width)?;
        return stdout.flush();
    }
    for (index, suggestion) in visible_suggestions.iter().enumerate() {
        let selected = index == app.command_selection;
        StyledLine::with_marker(
            if selected { "> " } else { "  " },
            if selected {
                Color::Magenta
            } else {
                Color::DarkGrey
            },
            format!("{:<34} {}", suggestion.usage, suggestion.description),
            if selected { Color::White } else { Color::Grey },
        )
        .print(stdout, width)?;
    }
    let prompt = if app.composer.draft.text.is_empty() {
        "Describe a coding task...".to_owned()
    } else {
        composer_prompt_text(&app.composer.draft.text)
    };
    StyledLine::with_marker(
        "> ",
        Color::Cyan,
        prompt,
        if app.composer.draft.text.is_empty() {
            Color::DarkGrey
        } else {
            Color::White
        },
    )
    .print(stdout, width)?;
    print_separator(stdout, width)?;
    StyledLine::with_marker(
        "› ",
        Color::Magenta,
        if app.is_running() {
            "working · ctrl+c to interrupt · esc to exit"
        } else {
            "enter to submit · ctrl+v to paste · tab to complete commands · esc to exit"
        },
        Color::DarkGrey,
    )
    .print(stdout, width)?;
    stdout.flush()
}

fn render_frame(identity: &UiIdentity, app: &AppState, width: u16, height: u16) -> Vec<StyledLine> {
    let blank = StyledLine::new("", Color::Reset);
    let mut frame = vec![blank.clone(); usize::from(height)];
    let mut row = usize::from(HEADER_TOP_PADDING);
    for logo_line in MEDUSA_LOGO {
        set_frame_line(&mut frame, row, StyledLine::new(logo_line, Color::Cyan));
        row = row.saturating_add(1);
    }
    set_frame_line(
        &mut frame,
        row,
        StyledLine::new(
            format!(
                "{} {}",
                app.model_label.as_deref().unwrap_or(&identity.model),
                app.effort_label.as_deref().unwrap_or(&identity.effort)
            ),
            Color::Magenta,
        ),
    );

    let header_height = HEADER_TOP_PADDING + 4;
    let question_modal = app.question_modal();
    let model_modal = app.model_modal();
    let modal_lines = question_modal
        .map(question_modal_lines)
        .or_else(|| model_modal.map(model_modal_lines))
        .unwrap_or_default();
    let is_modal = question_modal.is_some() || model_modal.is_some();
    let plan_panel = (!is_modal && app.task_list_visible)
        .then(|| app.plan.as_ref().map(plan_lines).unwrap_or_default())
        .unwrap_or_default();
    let panel_rows = u16::try_from(plan_panel.len()).unwrap_or(u16::MAX);
    let base_composer_rows = 4_u16.saturating_add(panel_rows);
    let suggestions = (!is_modal)
        .then(|| command_suggestions(&app.composer.draft.text))
        .unwrap_or_default();
    let available_suggestion_rows =
        height.saturating_sub(header_height.saturating_add(base_composer_rows));
    let visible_suggestions = suggestions
        .into_iter()
        .take(usize::from(available_suggestion_rows))
        .collect::<Vec<_>>();
    let requested_composer_height = if is_modal {
        3_u16.saturating_add(u16::try_from(modal_lines.len()).unwrap_or(u16::MAX))
    } else {
        base_composer_rows
            .saturating_add(u16::try_from(visible_suggestions.len()).unwrap_or(u16::MAX))
    };
    let composer_height = requested_composer_height.min(height.saturating_sub(header_height));
    let content_rows = usize::from(height.saturating_sub(composer_height + header_height));
    let mut content = transcript_lines(app);
    if app.is_running() {
        content.push(StyledLine::with_marker(
            spinner_marker(app.spinner_frame),
            Color::Magenta,
            running_status(app),
            Color::Grey,
        ));
    }
    let visible_content = content
        .iter()
        .rev()
        .take(content_rows)
        .rev()
        .collect::<Vec<_>>();
    let mut content_row = usize::from(header_height);
    for line in visible_content {
        set_frame_line(&mut frame, content_row, line.clone());
        content_row = content_row.saturating_add(1);
    }

    let mut bottom_row = usize::from(height.saturating_sub(composer_height));
    set_frame_line(&mut frame, bottom_row, separator_line(width));
    bottom_row = bottom_row.saturating_add(1);
    if is_modal {
        for line in modal_lines
            .into_iter()
            .take(usize::from(composer_height.saturating_sub(3)))
        {
            set_frame_line(&mut frame, bottom_row, line);
            bottom_row = bottom_row.saturating_add(1);
        }
        set_frame_line(&mut frame, bottom_row, separator_line(width));
        bottom_row = bottom_row.saturating_add(1);
        let help = if question_modal.is_some() {
            "up/down choose - type an answer - enter submit"
        } else {
            "up/down choose - tab focus - enter set - esc cancel"
        };
        set_frame_line(
            &mut frame,
            bottom_row,
            StyledLine::with_marker("> ", Color::Magenta, help, Color::DarkGrey),
        );
        return frame;
    }

    for line in plan_panel {
        set_frame_line(&mut frame, bottom_row, line);
        bottom_row = bottom_row.saturating_add(1);
    }
    for (index, suggestion) in visible_suggestions.iter().enumerate() {
        let selected = index == app.command_selection;
        set_frame_line(
            &mut frame,
            bottom_row,
            StyledLine::with_marker(
                if selected { "> " } else { "  " },
                if selected {
                    Color::Magenta
                } else {
                    Color::DarkGrey
                },
                format!("{:<34} {}", suggestion.usage, suggestion.description),
                if selected { Color::White } else { Color::Grey },
            ),
        );
        bottom_row = bottom_row.saturating_add(1);
    }
    let prompt = if app.composer.draft.text.is_empty() {
        "Describe a coding task...".to_owned()
    } else {
        composer_prompt_text(&app.composer.draft.text)
    };
    set_frame_line(
        &mut frame,
        bottom_row,
        StyledLine::with_marker(
            "> ",
            Color::Cyan,
            prompt,
            if app.composer.draft.text.is_empty() {
                Color::DarkGrey
            } else {
                Color::White
            },
        ),
    );
    bottom_row = bottom_row.saturating_add(1);
    set_frame_line(&mut frame, bottom_row, separator_line(width));
    bottom_row = bottom_row.saturating_add(1);
    set_frame_line(
        &mut frame,
        bottom_row,
        StyledLine::with_marker(
            "> ",
            Color::Magenta,
            if app.is_running() {
                "working - ctrl+c interrupt - ctrl+t tasks"
            } else {
                "enter submit - ctrl+v paste - tab commands - ctrl+t tasks"
            },
            Color::DarkGrey,
        ),
    );
    frame
}

fn transcript_lines(app: &AppState) -> Vec<StyledLine> {
    let mut lines = Vec::new();
    for entry in &app.transcript {
        match entry {
            TranscriptEntry::User(draft) => {
                lines.push(StyledLine::with_marker(
                    "> ",
                    Color::Cyan,
                    draft.text.replace('\n', " / "),
                    Color::White,
                ));
                lines.extend(draft.attachments.iter().map(|attachment| {
                    StyledLine::new(
                        format!("  - {}", attachment_label(attachment)),
                        Color::DarkGrey,
                    )
                }));
            }
            TranscriptEntry::Activity(activity) => lines.extend(activity_lines(activity)),
            TranscriptEntry::System(message) => lines.push(system_line(message)),
        }
    }
    lines
}

fn set_frame_line(frame: &mut [StyledLine], row: usize, line: StyledLine) {
    if let Some(slot) = frame.get_mut(row) {
        *slot = line;
    }
}

fn separator_line(width: u16) -> StyledLine {
    StyledLine::new("-".repeat(usize::from(width)), Color::DarkGrey)
}

fn draw_frame(
    stdout: &mut io::Stdout,
    width: u16,
    frame: &[StyledLine],
    previous: Option<&[StyledLine]>,
) -> io::Result<()> {
    for (row, line) in frame.iter().enumerate() {
        if previous.is_some_and(|previous| previous.get(row) == Some(line)) {
            continue;
        }
        line.print_at(stdout, width, u16::try_from(row).unwrap_or(u16::MAX))?;
    }
    Ok(())
}

fn spinner_marker(frame: u8) -> &'static str {
    match frame % 4 {
        0 => ". ",
        1 => "o ",
        2 => "O ",
        _ => "o ",
    }
}

fn model_modal_lines(model_modal: &app::ModelModal) -> Vec<StyledLine> {
    use app::ModelModalFocus::{ApiKey, Effort, Model, Provider};

    let focus = model_modal.focus();
    let mut lines = vec![StyledLine::new("Select model", Color::Cyan)];
    lines.push(StyledLine::with_marker(
        if focus == Provider { "› " } else { "  " },
        if focus == Provider {
            Color::Magenta
        } else {
            Color::DarkGrey
        },
        format!("Provider  {}", model_modal.provider()),
        if focus == Provider {
            Color::White
        } else {
            Color::Grey
        },
    ));
    for (index, model) in model_modal.model_options().iter().enumerate() {
        let selected = index == model_modal.selected_model_index();
        lines.push(StyledLine::with_marker(
            if selected && focus == Model {
                "› "
            } else {
                "  "
            },
            if selected && focus == Model {
                Color::Magenta
            } else {
                Color::DarkGrey
            },
            if selected {
                format!("{model}  selected")
            } else {
                model.clone()
            },
            if selected { Color::Green } else { Color::Grey },
        ));
    }
    lines.push(StyledLine::with_marker(
        if focus == Effort { "› " } else { "  " },
        if focus == Effort {
            Color::Magenta
        } else {
            Color::DarkGrey
        },
        format!("{} effort", model_modal.effort().label()),
        if focus == Effort {
            Color::White
        } else {
            Color::Grey
        },
    ));
    lines.push(StyledLine::with_marker(
        if focus == ApiKey { "› " } else { "  " },
        if focus == ApiKey {
            Color::Magenta
        } else {
            Color::DarkGrey
        },
        format!("API key  {}", model_modal.api_key_mask()),
        if focus == ApiKey {
            Color::White
        } else {
            Color::Grey
        },
    ));
    lines
}

fn question_modal_lines(question_modal: &app::QuestionModal) -> Vec<StyledLine> {
    let mut lines = vec![StyledLine::new("Question", Color::Cyan)];
    lines.extend(
        question_modal
            .question()
            .lines()
            .map(|line| StyledLine::new(line.trim(), Color::White)),
    );
    for (index, option) in question_modal.options().iter().enumerate() {
        let selected = index == question_modal.selected_option();
        lines.push(StyledLine::with_marker(
            if selected { "> " } else { "  " },
            if selected {
                Color::Magenta
            } else {
                Color::DarkGrey
            },
            option,
            if selected { Color::White } else { Color::Grey },
        ));
    }
    let answer = question_modal.custom_answer();
    lines.push(StyledLine::with_marker(
        "> ",
        Color::Cyan,
        if answer.is_empty() {
            "Type a custom answer...".to_owned()
        } else {
            answer.to_owned()
        },
        if answer.is_empty() {
            Color::DarkGrey
        } else {
            Color::White
        },
    ));
    lines
}

fn composer_prompt_text(text: &str) -> String {
    for prefix in ["/model key ", "/model api-key "] {
        if let Some(secret) = text.strip_prefix(prefix) {
            return format!("{prefix}{}", "*".repeat(secret.chars().count()));
        }
    }
    text.replace('\n', " / ")
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StyledLine {
    marker: Option<(String, Color)>,
    text: String,
    foreground: Color,
}

impl StyledLine {
    fn new(text: impl Into<String>, foreground: Color) -> Self {
        Self {
            marker: None,
            text: text.into(),
            foreground,
        }
    }

    fn with_marker(
        marker: impl Into<String>,
        marker_color: Color,
        text: impl Into<String>,
        foreground: Color,
    ) -> Self {
        Self {
            marker: Some((marker.into(), marker_color)),
            text: text.into(),
            foreground,
        }
    }

    fn print(&self, stdout: &mut io::Stdout, width: u16) -> io::Result<()> {
        if let Some((marker, marker_color)) = &self.marker {
            let marker = truncate(marker, width);
            let remaining = width.saturating_sub(marker.chars().count() as u16);
            return queue!(
                stdout,
                Clear(ClearType::UntilNewLine),
                SetAttribute(Attribute::Reset),
                ResetColor,
                SetForegroundColor(*marker_color),
                Print(marker),
                SetForegroundColor(self.foreground),
                Print(truncate(&self.text, remaining)),
                SetAttribute(Attribute::Reset),
                ResetColor,
                Print("\r\n")
            );
        }
        print_styled_line(stdout, width, &self.text, self.foreground, Attribute::Reset)
    }

    fn print_at(&self, stdout: &mut io::Stdout, width: u16, row: u16) -> io::Result<()> {
        queue!(
            stdout,
            MoveTo(0, row),
            Clear(ClearType::CurrentLine),
            SetAttribute(Attribute::Reset),
            ResetColor,
        )?;
        if let Some((marker, marker_color)) = &self.marker {
            let marker = truncate(marker, width);
            let remaining = width.saturating_sub(marker.chars().count() as u16);
            queue!(
                stdout,
                SetForegroundColor(*marker_color),
                Print(marker),
                SetForegroundColor(self.foreground),
                Print(truncate(&self.text, remaining)),
            )?;
        } else {
            queue!(
                stdout,
                SetForegroundColor(self.foreground),
                Print(truncate(&self.text, width)),
            )?;
        }
        queue!(stdout, SetAttribute(Attribute::Reset), ResetColor)
    }
}

fn system_line(message: &str) -> StyledLine {
    if message.starts_with("error:") {
        StyledLine::new(format!("● {message}"), Color::Red)
    } else if message.starts_with("evidence:") {
        StyledLine::new(format!("● {message}"), Color::Blue)
    } else if message.starts_with("step:") {
        StyledLine::new(format!("● {message}"), Color::Yellow)
    } else if message.contains("cancelled") {
        StyledLine::new(format!("● {message}"), Color::DarkYellow)
    } else {
        StyledLine::new(format!("● {message}"), Color::Green)
    }
}

fn activity_lines(activity: &TranscriptActivity) -> Vec<StyledLine> {
    let color = match activity.kind {
        TranscriptActivityKind::Assistant => Color::Green,
        TranscriptActivityKind::Done => Color::Green,
        TranscriptActivityKind::Error => Color::Red,
        TranscriptActivityKind::Progress => Color::Yellow,
        TranscriptActivityKind::Tool => Color::Green,
        TranscriptActivityKind::Verification => Color::Blue,
    };
    let foreground = if matches!(
        activity.kind,
        TranscriptActivityKind::Assistant
            | TranscriptActivityKind::Error
            | TranscriptActivityKind::Tool
    ) {
        Color::White
    } else {
        Color::Grey
    };
    let marker = if matches!(activity.kind, TranscriptActivityKind::Error) {
        "✻"
    } else {
        "●"
    };
    let mut lines = vec![StyledLine::with_marker(
        format!("{marker} "),
        color,
        &activity.title,
        foreground,
    )];
    lines.extend(
        activity
            .details
            .iter()
            .map(|detail| StyledLine::new(format!("  └ {detail}"), Color::DarkGrey)),
    );
    lines
}

fn plan_lines(plan: &app::TranscriptPlan) -> Vec<StyledLine> {
    use app::TranscriptPlanStepState::{Active, Completed, Failed, Pending};

    plan.steps
        .iter()
        .map(|step| match step.state {
            Active => StyledLine::with_marker("▪ ", Color::Yellow, &step.title, Color::White),
            Completed => StyledLine::with_marker("✓ ", Color::Green, &step.title, Color::Grey),
            Failed => StyledLine::with_marker("✻ ", Color::Red, &step.title, Color::White),
            Pending => StyledLine::with_marker("□ ", Color::DarkGrey, &step.title, Color::DarkGrey),
        })
        .collect()
}

fn print_separator(stdout: &mut io::Stdout, width: u16) -> io::Result<()> {
    queue!(
        stdout,
        Clear(ClearType::UntilNewLine),
        SetAttribute(Attribute::Reset),
        ResetColor,
        SetForegroundColor(Color::DarkGrey),
        Print("─".repeat(width as usize)),
        ResetColor,
        Print("\r\n")
    )
}

fn print_styled_line(
    stdout: &mut io::Stdout,
    width: u16,
    text: &str,
    foreground: Color,
    attribute: Attribute,
) -> io::Result<()> {
    queue!(
        stdout,
        Clear(ClearType::UntilNewLine),
        SetAttribute(Attribute::Reset),
        ResetColor,
        SetForegroundColor(foreground),
        SetAttribute(attribute)
    )?;
    queue!(
        stdout,
        Print(truncate(text, width)),
        SetAttribute(Attribute::Reset),
        ResetColor,
        Print("\r\n")
    )
}

fn attachment_label(attachment: &PromptAttachment) -> String {
    match attachment {
        PromptAttachment::PastedText(text) => {
            format!("[text] {} | {} bytes", text.display_name, text.text.len())
        }
        PromptAttachment::Image(image) => format!(
            "[image] {} | {}x{} | {} bytes",
            image.display_name,
            image.width,
            image.height,
            image.rgba.len()
        ),
        PromptAttachment::File(file) => {
            format!("[file] {} | {} bytes", file.path.display(), file.byte_len)
        }
    }
}

fn truncate(value: &str, width: u16) -> String {
    let limit = usize::from(width);
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    value
        .chars()
        .take(limit.saturating_sub(1))
        .chain(std::iter::once('~'))
        .collect()
}

fn app_error(error: AppError) -> io::Error {
    io::Error::other(error)
}

fn runtime_error(error: runtime::RuntimeError) -> io::Error {
    io::Error::other(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::ImageAttachment;

    #[test]
    fn default_socket_is_repository_scoped() {
        let options = TuiOptions::for_repo("/tmp/example");
        assert_eq!(
            options.socket_path(),
            PathBuf::from("/tmp/example/.medusa/daemon/medusa.sock")
        );
    }

    #[test]
    fn explicit_socket_wins() {
        let mut options = TuiOptions::for_repo("/tmp/example");
        options.socket = Some(PathBuf::from("/tmp/medusa.sock"));
        assert_eq!(options.socket_path(), PathBuf::from("/tmp/medusa.sock"));
    }

    #[test]
    fn ctrl_l_requests_a_terminal_redraw() {
        assert!(ctrl_l_redraw(&Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('l'),
            KeyModifiers::CONTROL,
        ))));
    }

    #[test]
    fn image_attachment_label_includes_dimensions() {
        let attachment = PromptAttachment::Image(ImageAttachment {
            display_name: "shot.png".to_owned(),
            width: 10,
            height: 20,
            rgba: vec![0; 8],
            source_format: Some("image/rgba8".to_owned()),
        });
        assert!(attachment_label(&attachment).contains("10x20"));
    }

    #[test]
    fn portable_render_snapshot_changes_only_with_visible_state() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "redraw-test",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");

        let initial = portable_render_snapshot(&app, (80, 24));
        assert_eq!(initial, portable_render_snapshot(&app, (80, 24)));

        app.status = "agent running".to_owned();
        assert_ne!(initial, portable_render_snapshot(&app, (80, 24)));

        app.begin_run();
        let running = portable_render_snapshot(&app, (80, 24));
        app.tick();
        assert_ne!(running, portable_render_snapshot(&app, (80, 24)));
    }

    #[test]
    fn question_and_plan_are_rendered_in_the_bottom_panels() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "panel-test",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");
        app.set_plan(app::TranscriptPlan {
            steps: vec![app::TranscriptPlanStep {
                title: "Inspect the repository".to_owned(),
                state: app::TranscriptPlanStepState::Active,
            }],
        });
        let plan_frame = render_frame(&UiIdentity::for_repo(directory.path()), &app, 80, 24);
        assert!(
            plan_frame
                .iter()
                .rev()
                .take(6)
                .any(|line| { line.text.contains("Inspect the repository") })
        );

        app.open_question(
            "Which project should I use?".to_owned(),
            vec!["Projects/site-a".to_owned()],
        );
        let question_frame = render_frame(&UiIdentity::for_repo(directory.path()), &app, 80, 24);
        assert!(
            question_frame
                .iter()
                .rev()
                .take(8)
                .any(|line| { line.text.contains("Which project should I use?") })
        );
        assert!(
            !question_frame
                .iter()
                .any(|line| { line.text.contains("Describe a coding task") })
        );
    }

    #[test]
    fn spinner_changes_only_one_retained_frame_row() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "render-diff",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");
        app.begin_run();
        app.tick();
        let before = render_frame(&UiIdentity::for_repo(directory.path()), &app, 80, 24);
        app.tick();
        let after = render_frame(&UiIdentity::for_repo(directory.path()), &app, 80, 24);
        assert_eq!(
            before
                .iter()
                .zip(after.iter())
                .filter(|(left, right)| left != right)
                .count(),
            1
        );
    }

    #[test]
    fn running_status_includes_elapsed_time_and_tokens() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new(
            directory.path().to_path_buf(),
            "status-test",
            "",
            Arc::new(UnsupportedClipboard),
        )
        .expect("app");
        app.begin_run();
        app.add_output_tokens(1_200);
        assert_eq!(running_status(&app), "Working (0s · ↑ 1.2k tokens)");
    }

    #[test]
    fn effort_label_tracks_turn_budget() {
        assert_eq!(effort_label(50), "effort:low");
        assert_eq!(effort_label(100), "effort:medium");
        assert_eq!(effort_label(500), "effort:high");
    }
}
