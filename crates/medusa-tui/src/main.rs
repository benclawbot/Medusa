use std::{io, path::PathBuf};

use clap::Parser;
use medusa_tui::{TuiOptions, run};

#[derive(Parser, Debug)]
#[command(
    name = "medusa-tui",
    about = "Compatibility launcher for the Medusa interactive terminal",
    after_help = "`medusa-tui` is retained for compatibility. Prefer `medusa`."
)]
struct Args {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long)]
    socket: Option<PathBuf>,
    #[arg(long)]
    prompt: Option<String>,
    #[arg(long)]
    resume: Option<String>,
    #[arg(long)]
    r#continue: bool,
}

fn main() -> io::Result<()> {
    let args = Args::parse();
    eprintln!("warning: `medusa-tui` is deprecated; use `medusa` instead");
    let mut options = TuiOptions::for_repo(args.repo);
    options.socket = args.socket;
    options.initial_prompt = args.prompt;
    options.resume_session = args.resume;
    options.continue_latest = args.r#continue;
    let _ = run(options)?;
    Ok(())
}
