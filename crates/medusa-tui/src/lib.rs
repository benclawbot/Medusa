use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
    thread,
    time::Duration,
};

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind},
    execute, queue,
    style::{Attribute, Print, SetAttribute},
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode, size,
    },
};

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

    let mut terminal = TerminalGuard::enter()?;
    run_loop(terminal.stdout(), options)
}

struct TerminalGuard {
    stdout: io::Stdout,
    active: bool,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, Hide) {
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
        let _ = execute!(self.stdout, Show, LeaveAlternateScreen);
        self.active = false;
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(unix)]
fn run_loop(stdout: &mut io::Stdout, options: TuiOptions) -> io::Result<ExitReason> {
    let client = DaemonClient::new(options.socket_path());
    loop {
        let (jobs, status) = match client.request(Request::List) {
            Ok(Response::Jobs { jobs }) => (jobs, "connected".to_owned()),
            Ok(other) => (Vec::new(), format!("unexpected response: {other:?}")),
            Err(error) => (Vec::new(), format!("disconnected: {error}")),
        };
        draw(stdout, &options, &jobs, &status)?;
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(ExitReason::UserQuit),
                KeyCode::Char('d') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                    return Ok(ExitReason::InputClosed);
                }
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(not(unix))]
fn run_loop(stdout: &mut io::Stdout, options: TuiOptions) -> io::Result<ExitReason> {
    loop {
        draw_unsupported(stdout, &options)?;
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
        {
            return Ok(ExitReason::UserQuit);
        }
    }
}

#[cfg(unix)]
fn draw(
    stdout: &mut io::Stdout,
    options: &TuiOptions,
    jobs: &[JobRecord],
    status: &str,
) -> io::Result<()> {
    let (width, height) = size()?;
    queue!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
    queue!(
        stdout,
        SetAttribute(Attribute::Bold),
        Print("Medusa interactive\r\n"),
        SetAttribute(Attribute::Reset),
        Print(format!("repo: {}\r\n", options.repo.display())),
        Print(format!("daemon: {status}\r\n")),
        Print("─".repeat(width as usize)),
        Print("\r\n")
    )?;

    let available_rows = height.saturating_sub(7) as usize;
    if let Some(prompt) = &options.initial_prompt {
        queue!(stdout, Print(format!("initial objective: {prompt}\r\n")))?;
    }
    if jobs.is_empty() {
        queue!(stdout, Print("No daemon jobs\r\n"))?;
    } else {
        for job in jobs.iter().rev().take(available_rows) {
            let mut line = format!(
                "{}  {:?}  {} {}",
                job.id,
                job.state,
                job.program,
                job.args.join(" ")
            );
            if line.len() > width as usize {
                line.truncate(width as usize);
            }
            queue!(stdout, Print(line), Print("\r\n"))?;
        }
    }
    queue!(
        stdout,
        MoveTo(0, height.saturating_sub(1)),
        Print("q/esc to exit")
    )?;
    stdout.flush()
}

#[cfg(not(unix))]
fn draw_unsupported(stdout: &mut io::Stdout, options: &TuiOptions) -> io::Result<()> {
    let (_, height) = size()?;
    queue!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
    queue!(
        stdout,
        SetAttribute(Attribute::Bold),
        Print("Medusa interactive\r\n"),
        SetAttribute(Attribute::Reset),
        Print(format!("repo: {}\r\n\r\n", options.repo.display())),
        Print("The Windows local transport is not active in this build.\r\n"),
        Print("The integration branch will replace this with named-pipe transport.\r\n"),
        MoveTo(0, height.saturating_sub(1)),
        Print("q/esc to exit")
    )?;
    stdout.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
