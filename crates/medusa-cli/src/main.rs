use std::{collections::BTreeMap, fs, path::PathBuf, process::Command};

use clap::{Parser, Subcommand};
use medusa_agent::{AgentEngine, bootstrap};
use medusa_config::Config;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_provider::MiniMaxProvider;
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(name = "medusa", version, about = "Autonomous coding agent")]
struct Cli {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long = "set", value_parser = parse_key_value)]
    overrides: Vec<(String, String)>,
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand, Debug)]
enum CommandKind {
    Bootstrap,
    Search { pattern: String },
    Shell { program: String, args: Vec<String> },
    Checkpoint { message: String },
    Run { objective: String },
    Resume { session: String },
}

fn parse_key_value(raw: &str) -> Result<(String, String), String> {
    raw.split_once('=')
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .ok_or_else(|| "expected key=value".to_owned())
}

fn main() {
    if let Err(error) = run() {
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&error).unwrap_or_else(|_| error.to_string())
        );
        std::process::exit(1);
    }
}

fn run() -> MedusaResult<()> {
    let cli = Cli::parse();
    let overrides = cli.overrides.into_iter().collect::<BTreeMap<_, _>>();
    let config = Config::load_layers(None, None, &BTreeMap::new(), &overrides)?;
    let repo = cli.repo.canonicalize().unwrap_or(cli.repo);

    match cli.command {
        CommandKind::Bootstrap => {
            bootstrap(&repo)?;
            println!("bootstrapped {}", repo.display());
            Ok(())
        }
        CommandKind::Search { pattern } => search(&repo, &pattern),
        CommandKind::Shell { program, args } => shell(&repo, &program, &args),
        CommandKind::Checkpoint { message } => checkpoint(&repo, &message),
        CommandKind::Run { objective } => {
            let provider = MiniMaxProvider::from_config(&config)?;
            let engine = AgentEngine::new(provider, config);
            let mut session = engine.create_session(&repo, objective)?;
            println!("session {} created", session.id);
            engine.run_to_completion(&mut session)?;
            print_completion(&session);
            Ok(())
        }
        CommandKind::Resume { session } => {
            let provider = MiniMaxProvider::from_config(&config)?;
            let engine = AgentEngine::new(provider, config);
            let mut session = engine.load_session(&repo, &session)?;
            println!("session {} resumed", session.id);
            engine.run_to_completion(&mut session)?;
            print_completion(&session);
            Ok(())
        }
    }
}

fn print_completion(session: &medusa_agent::AgentSession) {
    println!("session {} completed", session.id);
    for item in &session.evidence {
        println!("evidence: {item}");
    }
}

fn search(repo: &std::path::Path, pattern: &str) -> MedusaResult<()> {
    for entry in WalkDir::new(repo).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file()
            || entry
                .path()
                .components()
                .any(|part| part.as_os_str() == ".git")
        {
            continue;
        }
        if let Ok(text) = fs::read_to_string(entry.path()) {
            for (index, line) in text.lines().enumerate() {
                if line.contains(pattern) {
                    println!("{}:{}:{}", entry.path().display(), index + 1, line.trim());
                }
            }
        }
    }
    Ok(())
}

fn shell(repo: &std::path::Path, program: &str, args: &[String]) -> MedusaResult<()> {
    if matches!(program, "rm" | "sudo" | "shutdown" | "reboot" | "mkfs") {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!("hard-denied command: {program}"),
        ));
    }
    let status = Command::new(program)
        .args(args)
        .current_dir(repo)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!("command exited with {status}"),
        ))
    }
}

fn checkpoint(repo: &std::path::Path, message: &str) -> MedusaResult<()> {
    run_git(repo, &["add", "-A"])?;
    run_git(repo, &["commit", "-m", message])
}

fn run_git(repo: &std::path::Path, args: &[&str]) -> MedusaResult<()> {
    let status = Command::new("git").args(args).current_dir(repo).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!("git {} failed with {status}", args.join(" ")),
        ))
    }
}
