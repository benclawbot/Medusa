use std::{error::Error, path::PathBuf};

use clap::{Parser, Subcommand};
use medusa_daemon::{DaemonPaths, serve};
use medusa_tui::{TuiOptions, run};

#[derive(Parser, Debug)]
#[command(
    name = "medusa-tui",
    about = "Compatibility launcher for the Medusa interactive terminal",
    after_help = "`medusa-tui` is retained for compatibility. Prefer `medusa`."
)]
struct Args {
    #[arg(long, default_value = ".", global = true)]
    repo: PathBuf,
    #[arg(long)]
    socket: Option<PathBuf>,
    #[arg(long)]
    prompt: Option<String>,
    #[arg(long)]
    resume: Option<String>,
    #[arg(long)]
    r#continue: bool,
    #[command(subcommand)]
    command: Option<CommandKind>,
}

#[derive(Subcommand, Debug)]
enum CommandKind {
    #[command(name = "__daemon-serve", hide = true)]
    DaemonServe,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let repo = args.repo.canonicalize().unwrap_or(args.repo);
    if matches!(args.command, Some(CommandKind::DaemonServe)) {
        serve(DaemonPaths::for_repo(&repo))?;
        return Ok(());
    }

    eprintln!("warning: `medusa-tui` is deprecated; use `medusa` instead");
    let mut options = TuiOptions::for_repo(repo);
    options.socket = args.socket;
    options.initial_prompt = args.prompt;
    options.resume_session = args.resume;
    options.continue_latest = args.r#continue;
    let _ = run(options)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_definition_is_valid() {
        Args::command().debug_assert();
    }

    #[test]
    fn hidden_daemon_host_accepts_repository_after_subcommand() {
        let args = Args::try_parse_from(["medusa-tui", "__daemon-serve", "--repo", "."])
            .expect("parse daemon host");
        assert!(matches!(args.command, Some(CommandKind::DaemonServe)));
    }
}
