pub mod app;
pub mod clipboard;
pub mod draft_store;
pub mod input;
pub mod native_clipboard;
pub mod runtime;

use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
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
    style::{Attribute, Print, SetAttribute},
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode, size,
    },
};
use native_clipboard::NativeClipboard;
use runtime::{RuntimeController, RuntimeEvent};

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
    let runtime = RuntimeController::start(options.repo.clone());
    let mut terminal = TerminalGuard::enter()?;
    run_loop(terminal.stdout(), &options, &mut app, &runtime)
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
        draw(stdout, options, app, &jobs, &daemon_status)?;
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
    app: &mut AppState,
    runtime: &RuntimeController,
) -> io::Result<ExitReason> {
    let mut last_render = None;
    loop {
        drain_runtime_events(app, runtime)?;
        let snapshot = portable_render_snapshot(app, size()?);
        if last_render.as_ref() != Some(&snapshot) {
            draw_portable(stdout, options, app)?;
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
                app.status = "agent running".to_owned();
            }
            RuntimeEvent::Progress { turn } => {
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
    draw_common(stdout, options, app, &job_lines, daemon_status)
}

#[cfg(not(unix))]
fn draw_portable(stdout: &mut io::Stdout, options: &TuiOptions, app: &AppState) -> io::Result<()> {
    draw_common(stdout, options, app, &[], portable_daemon_status())
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

fn draw_common(
    stdout: &mut io::Stdout,
    options: &TuiOptions,
    app: &AppState,
    job_lines: &[String],
    daemon_status: &str,
) -> io::Result<()> {
    let (width, height) = size()?;
    queue!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
    queue!(
        stdout,
        SetAttribute(Attribute::Bold),
        Print("Medusa interactive"),
        SetAttribute(Attribute::Reset),
        Print(format!("  {}\r\n", options.repo.display())),
        Print(format!("daemon: {daemon_status}\r\n")),
        Print(format!("status: {}\r\n", app.status)),
        Print("-".repeat(width as usize)),
        Print("\r\n")
    )?;

    let composer_height = 7_u16.min(height.saturating_sub(5));
    let content_rows = height.saturating_sub(composer_height + 5) as usize;
    let mut lines = Vec::new();
    for entry in &app.transcript {
        match entry {
            TranscriptEntry::User(draft) => {
                lines.push(format!("> {}", draft.text.replace('\n', " / ")));
                lines.extend(draft.attachments.iter().map(attachment_label));
            }
            TranscriptEntry::System(message) => lines.push(format!("* {message}")),
        }
    }
    lines.extend(job_lines.iter().cloned());
    for line in lines.iter().rev().take(content_rows).rev() {
        queue!(stdout, Print(truncate(line, width)), Print("\r\n"))?;
    }

    let composer_top = height.saturating_sub(composer_height + 1);
    queue!(
        stdout,
        MoveTo(0, composer_top),
        Print("-".repeat(width as usize)),
        MoveTo(0, composer_top.saturating_add(1)),
        SetAttribute(Attribute::Bold),
        Print("Prompt"),
        SetAttribute(Attribute::Reset),
        Print("  Ctrl+V paste | Enter submit | Ctrl+C cancel | Esc exit\r\n")
    )?;
    let prompt = if app.composer.draft.text.is_empty() {
        "Describe a coding task...".to_owned()
    } else {
        app.composer.draft.text.replace('\n', " / ")
    };
    queue!(stdout, Print(truncate(&prompt, width)), Print("\r\n"))?;
    for attachment in app.composer.draft.attachments.iter().take(3) {
        queue!(
            stdout,
            Print(truncate(&attachment_label(attachment), width)),
            Print("\r\n")
        )?;
    }
    stdout.flush()
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
}
