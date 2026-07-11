use std::{io, path::PathBuf, thread, time::Duration};

use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use medusa_daemon::{DaemonClient, JobRecord, Request, Response};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

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
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run(&mut terminal, DaemonClient::new(args.socket));
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    client: DaemonClient,
) -> io::Result<()> {
    loop {
        let (jobs, status) = match client.request(Request::List) {
            Ok(Response::Jobs { jobs }) => (jobs, "connected".to_owned()),
            Ok(other) => (Vec::new(), format!("unexpected response: {other:?}")),
            Err(error) => (Vec::new(), format!("disconnected: {error}")),
        };
        terminal.draw(|frame| draw(frame, &jobs, &status))?;
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

fn draw(frame: &mut ratatui::Frame<'_>, jobs: &[JobRecord], status: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1), Constraint::Length(3)])
        .split(frame.area());
    let title = Paragraph::new(Line::from("Medusa daemon"))
        .style(Style::default().add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, chunks[0]);

    let items = jobs
        .iter()
        .rev()
        .map(|job| {
            ListItem::new(format!(
                "{}  {:?}  {} {}",
                job.id,
                job.state,
                job.program,
                job.args.join(" ")
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(items).block(Block::default().title("Jobs").borders(Borders::ALL)),
        chunks[1],
    );
    frame.render_widget(
        Paragraph::new(format!("{status} — q/esc to exit"))
            .block(Block::default().borders(Borders::ALL)),
        chunks[2],
    );
}
