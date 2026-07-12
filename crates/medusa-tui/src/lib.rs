pub mod app;
pub mod clipboard;
pub mod draft_store;
pub mod input;
pub mod native_clipboard;

use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
    sync::Arc,
    thread,
    time::Duration,
};

use app::{AppAction, AppError, AppState, TranscriptEntry};
use clipboard::{ClipboardService, PromptAttachment, UnsupportedClipboard};
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
    let mut terminal = TerminalGuard::enter()?;
    run_loop(terminal.stdout(), &options, &mut app)
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
) -> io::Result<ExitReason> {
    let client = DaemonClient::new(options.socket_path());
    loop {
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
            match app.handle_event(terminal_event).map_err(app_error)? {
                AppAction::Quit => return Ok(ExitReason::UserQuit),
                AppAction::Interrupt => app.status = "interrupt requested".to_owned(),
                AppAction::Submit(draft) => {
                    app.status = format!(
                        "prompt queued: {} bytes, {} attachment(s)",
                        draft.text.len(),
                        draft.attachments.len()
                    );
                }
                AppAction::None | AppAction::Redraw => {}
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
) -> io::Result<ExitReason> {
    loop {
        draw_portable(stdout, options, app)?;
        if event::poll(Duration::from_millis(100))? {
            let terminal_event = event::read()?;
            if ctrl_d_on_empty(&terminal_event, app) {
                return Ok(ExitReason::InputClosed);
            }
            match app.handle_event(terminal_event).map_err(app_error)? {
                AppAction::Quit => return Ok(ExitReason::UserQuit),
                AppAction::Interrupt => app.status = "interrupt requested".to_owned(),
                AppAction::Submit(draft) => {
                    app.status = format!(
                        "prompt queued: {} bytes, {} attachment(s)",
                        draft.text.len(),
                        draft.attachments.len()
                    );
                }
                AppAction::None | AppAction::Redraw => {}
            }
        }
    }
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
    draw_common(stdout, options, app, &[], "local transport unavailable")
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
        Print("  Ctrl+V paste text or screenshot | Enter submit\r\n")
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
}
