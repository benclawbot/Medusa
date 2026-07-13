pub mod app;
pub mod clipboard;
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

use app::{AppAction, AppError, AppState, TranscriptEntry};
use clipboard::{ClipboardService, PromptAttachment, PromptDraft, UnsupportedClipboard};
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute, queue,
    style::{
        Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
    },
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode, size,
    },
};
use medusa_config::Config;
use native_clipboard::NativeClipboard;
use runtime::{RuntimeController, RuntimeEvent};

const MEDUSA_LOGO: [&str; 3] = [
    "╭┬╮╭─╴╶┬╮╷ ╷╭─╮╭─╮",
    "│││├╴  │││ │╰─╮├─┤",
    "╵ ╵╰─╴╶┴╯╰─╯╰─╯╵ ╵",
];

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
    options: &TuiOptions,
    identity: &UiIdentity,
    app: &mut AppState,
    runtime: &RuntimeController,
) -> io::Result<ExitReason> {
    let mut last_render = None;
    loop {
        drain_runtime_events(app, runtime)?;
        let snapshot = portable_render_snapshot(app, size()?);
        if last_render.as_ref() != Some(&snapshot) {
            draw_portable(stdout, options, identity, app)?;
            last_render = Some(snapshot);
        }
        if event::poll(Duration::from_millis(100))? {
            let terminal_event = event::read()?;
            if matches!(terminal_event, Event::Resize(_, _)) {
                last_render = None;
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
        AppAction::None | AppAction::Redraw => Ok(false),
    }
}

fn drain_runtime_events(app: &mut AppState, runtime: &RuntimeController) -> io::Result<()> {
    while let Some(event) = runtime.try_event().map_err(runtime_error)? {
        match event {
            RuntimeEvent::Started => {
                app.transcript
                    .push(TranscriptEntry::System("step: agent started".to_owned()));
                app.status = "agent running".to_owned();
            }
            RuntimeEvent::Progress { turn } => {
                app.transcript
                    .push(TranscriptEntry::System(format!("step: turn {turn}")));
                app.status = running_status(turn);
            }
            RuntimeEvent::Completed {
                session_id,
                assistant_text,
                evidence,
            } => {
                if !assistant_text.trim().is_empty() {
                    app.transcript.push(TranscriptEntry::System(assistant_text));
                }
                for line in evidence {
                    app.transcript
                        .push(TranscriptEntry::System(format!("evidence: {line}")));
                }
                app.transcript.push(TranscriptEntry::System(format!(
                    "session {session_id} completed"
                )));
                app.status = format!("session {session_id} completed");
            }
            RuntimeEvent::Cancelled => {
                app.transcript
                    .push(TranscriptEntry::System("task cancelled".to_owned()));
                app.status = "cancelled".to_owned();
            }
            RuntimeEvent::Failed(error) => {
                app.transcript
                    .push(TranscriptEntry::System(format!("error: {error}")));
                app.status = "agent failed".to_owned();
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

#[cfg(unix)]
fn draw(
    stdout: &mut io::Stdout,
    options: &TuiOptions,
    identity: &UiIdentity,
    app: &AppState,
    jobs: &[JobRecord],
    daemon_status: &str,
) -> io::Result<()> {
    let job_lines = jobs
        .iter()
        .rev()
        .take(3)
        .map(|job| {
            format!(
                "job {} {:?} {} {}",
                job.id,
                job.state,
                job.program,
                job.args.join(" ")
            )
        })
        .collect::<Vec<_>>();
    draw_common(stdout, options, identity, app, &job_lines, daemon_status)
}

#[cfg(not(unix))]
fn draw_portable(
    stdout: &mut io::Stdout,
    options: &TuiOptions,
    identity: &UiIdentity,
    app: &AppState,
) -> io::Result<()> {
    draw_common(
        stdout,
        options,
        identity,
        app,
        &[],
        portable_daemon_status(),
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PortableRenderSnapshot {
    terminal_size: (u16, u16),
    status: String,
    transcript: Vec<TranscriptEntry>,
    draft: PromptDraft,
}

fn portable_render_snapshot(app: &AppState, terminal_size: (u16, u16)) -> PortableRenderSnapshot {
    PortableRenderSnapshot {
        terminal_size,
        status: app.status.clone(),
        transcript: app.transcript.clone(),
        draft: app.composer.draft.clone(),
    }
}

fn portable_daemon_status() -> &'static str {
    "direct runtime (no daemon required)"
}

fn running_status(turn: u32) -> String {
    format!("agent running - turn {turn}; results appear when complete")
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

fn draw_common(
    stdout: &mut io::Stdout,
    options: &TuiOptions,
    identity: &UiIdentity,
    app: &AppState,
    job_lines: &[String],
    daemon_status: &str,
) -> io::Result<()> {
    let (width, height) = size()?;
    queue!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
    for logo_line in MEDUSA_LOGO {
        print_styled_line(stdout, width, logo_line, Color::Cyan, None, Attribute::Bold)?;
    }
    queue!(
        stdout,
        SetForegroundColor(Color::Magenta),
        SetAttribute(Attribute::Bold),
        Print(truncate(
            &format!("{} {}", identity.model, identity.effort),
            width
        )),
        SetAttribute(Attribute::Reset),
        ResetColor,
        Print("\r\n"),
    )?;
    print_status_row(
        stdout,
        width,
        "repo",
        &options.repo.display().to_string(),
        Color::DarkGrey,
    )?;
    print_status_row(stdout, width, "daemon", daemon_status, Color::Blue)?;
    print_status_row(
        stdout,
        width,
        "status",
        &app.status,
        status_color(&app.status),
    )?;
    print_separator(stdout, width)?;

    let header_height = 8_u16;
    let composer_height = 7_u16.min(height.saturating_sub(header_height));
    let content_rows = height.saturating_sub(composer_height + header_height) as usize;
    let mut lines = Vec::new();
    for entry in &app.transcript {
        match entry {
            TranscriptEntry::User(draft) => {
                lines.push(StyledLine::user(format!(
                    "● user {}",
                    draft.text.replace('\n', " / ")
                )));
                lines.extend(draft.attachments.iter().map(|attachment| {
                    StyledLine::user(format!("  {}", attachment_label(attachment)))
                }));
            }
            TranscriptEntry::System(message) => lines.push(system_line(message)),
        }
    }
    lines.extend(
        job_lines
            .iter()
            .map(|line| StyledLine::new(format!("● {line}"), Color::Magenta, None)),
    );
    for line in lines.iter().rev().take(content_rows).rev() {
        line.print(stdout, width)?;
    }

    let composer_top = height.saturating_sub(composer_height + 1);
    queue!(
        stdout,
        MoveTo(0, composer_top),
        SetForegroundColor(Color::DarkGrey),
        Print("─".repeat(width as usize)),
        ResetColor,
        MoveTo(0, composer_top.saturating_add(1)),
        SetForegroundColor(Color::Cyan),
        SetAttribute(Attribute::Bold),
        Print("● Prompt"),
        SetAttribute(Attribute::Reset),
        ResetColor,
        Print("  Ctrl+V paste | Enter submit | Ctrl+C cancel | Esc exit\r\n")
    )?;
    let prompt = if app.composer.draft.text.is_empty() {
        "Describe a coding task...".to_owned()
    } else {
        app.composer.draft.text.replace('\n', " / ")
    };
    print_styled_line(
        stdout,
        width,
        &prompt,
        Color::White,
        Some(Color::DarkBlue),
        Attribute::NoBold,
    )?;
    for attachment in app.composer.draft.attachments.iter().take(3) {
        print_styled_line(
            stdout,
            width,
            &attachment_label(attachment),
            Color::White,
            Some(Color::DarkBlue),
            Attribute::NoBold,
        )?;
    }
    stdout.flush()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StyledLine {
    text: String,
    foreground: Color,
    background: Option<Color>,
}

impl StyledLine {
    fn new(text: impl Into<String>, foreground: Color, background: Option<Color>) -> Self {
        Self {
            text: text.into(),
            foreground,
            background,
        }
    }

    fn user(text: impl Into<String>) -> Self {
        Self::new(text, Color::White, Some(Color::DarkBlue))
    }

    fn print(&self, stdout: &mut io::Stdout, width: u16) -> io::Result<()> {
        print_styled_line(
            stdout,
            width,
            &self.text,
            self.foreground,
            self.background,
            Attribute::NoBold,
        )
    }
}

fn system_line(message: &str) -> StyledLine {
    if message.starts_with("error:") {
        StyledLine::new(format!("● {message}"), Color::Red, None)
    } else if message.starts_with("evidence:") {
        StyledLine::new(format!("● {message}"), Color::Blue, None)
    } else if message.starts_with("step:") {
        StyledLine::new(format!("● {message}"), Color::Yellow, None)
    } else if message.contains("cancelled") {
        StyledLine::new(format!("● {message}"), Color::DarkYellow, None)
    } else {
        StyledLine::new(format!("● {message}"), Color::Green, None)
    }
}

fn status_color(status: &str) -> Color {
    if status.contains("failed") || status.contains("error") || status.contains("rejected") {
        Color::Red
    } else if status.contains("running") || status.contains("turn") {
        Color::Yellow
    } else if status.contains("completed") {
        Color::Green
    } else {
        Color::Cyan
    }
}

fn print_status_row(
    stdout: &mut io::Stdout,
    width: u16,
    label: &str,
    value: &str,
    dot_color: Color,
) -> io::Result<()> {
    queue!(
        stdout,
        SetForegroundColor(dot_color),
        Print("● "),
        SetForegroundColor(Color::DarkGrey),
        Print(label),
        ResetColor,
        Print(" "),
        Print(truncate(
            value,
            width.saturating_sub(label.len() as u16 + 3)
        )),
        Print("\r\n")
    )
}

fn print_separator(stdout: &mut io::Stdout, width: u16) -> io::Result<()> {
    queue!(
        stdout,
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
    background: Option<Color>,
    attribute: Attribute,
) -> io::Result<()> {
    queue!(
        stdout,
        SetForegroundColor(foreground),
        SetAttribute(attribute)
    )?;
    if let Some(background) = background {
        queue!(stdout, SetBackgroundColor(background))?;
    }
    queue!(
        stdout,
        Print(pad_to_width(&truncate(text, width), width)),
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

fn pad_to_width(value: &str, width: u16) -> String {
    let limit = usize::from(width);
    let count = value.chars().count();
    if count >= limit {
        return value.to_owned();
    }
    format!("{value}{}", " ".repeat(limit - count))
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
    }

    #[test]
    fn portable_status_explains_direct_runtime_and_deferred_results() {
        assert_eq!(
            portable_daemon_status(),
            "direct runtime (no daemon required)"
        );
        assert_eq!(
            running_status(3),
            "agent running - turn 3; results appear when complete"
        );
    }

    #[test]
    fn effort_label_tracks_turn_budget() {
        assert_eq!(effort_label(50), "effort:low");
        assert_eq!(effort_label(100), "effort:medium");
        assert_eq!(effort_label(500), "effort:high");
    }
}
