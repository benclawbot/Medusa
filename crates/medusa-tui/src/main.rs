use std::{
    io::{self, Write},
    path::PathBuf,
    thread,
    time::Duration,
};

use clap::Parser;
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode},
    execute, queue,
    style::{Attribute, Print, SetAttribute},
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode, size,
    },
};
use medusa_daemon::{DaemonClient, JobRecord, Request, Response};

#[derive(Parser, Debug)]
#[command(name = "medusa-tui", about = "Medusa daemon dashboard")]
struct Args {
    #[arg(long, default_value = ".medusa/daemon/medusa.sock")]
    socket: PathBuf,
}

fn main() -> io::Result<()> {
    let args = Args::parse();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, Hide)?;
    let result = run(&mut stdout, DaemonClient::new(args.socket));
    disable_raw_mode()?;
    execute!(stdout, Show, LeaveAlternateScreen)?;
    result
}

fn run(stdout: &mut io::Stdout, client: DaemonClient) -> io::Result<()> {
    loop {
        let (jobs, status) = match client.request(Request::List) {
            Ok(Response::Jobs { jobs }) => (jobs, "connected".to_owned()),
            Ok(other) => (Vec::new(), format!("unexpected response: {other:?}")),
            Err(error) => (Vec::new(), format!("disconnected: {error}")),
        };
        draw(stdout, &jobs, &status)?;
        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
            && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
        {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn draw(stdout: &mut io::Stdout, jobs: &[JobRecord], status: &str) -> io::Result<()> {
    let (width, height) = size()?;
    queue!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
    queue!(
        stdout,
        SetAttribute(Attribute::Bold),
        Print("Medusa daemon\r\n"),
        SetAttribute(Attribute::Reset),
        Print(format!("status: {status}\r\n")),
        Print("─".repeat(width as usize)),
        Print("\r\n")
    )?;

    let available_rows = height.saturating_sub(5) as usize;
    if jobs.is_empty() {
        queue!(stdout, Print("No jobs\r\n"))?;
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
