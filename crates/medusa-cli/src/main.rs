use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use clap::{Parser, Subcommand};
use medusa_agent::{AgentEngine, bootstrap};
use medusa_config::Config;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_hardening::{CURRENT_SCHEMA_VERSION, Migrator};
use medusa_provider::MiniMaxProvider;
use medusa_tui::{TuiOptions, run as run_tui};
use serde::Serialize;
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(
    name = "medusa",
    version,
    about = "Autonomous coding agent",
    after_help = "Run `medusa` without a subcommand to open the interactive terminal. Use `medusa run` for headless execution."
)]
struct Cli {
    #[arg(long, default_value = ".", global = true)]
    repo: PathBuf,
    #[arg(long = "set", value_parser = parse_key_value, global = true)]
    overrides: Vec<(String, String)>,
    #[arg(long, conflicts_with = "command")]
    prompt: Option<String>,
    #[arg(long, conflicts_with_all = ["command", "resume_session"])]
    r#continue: bool,
    #[arg(long = "resume", value_name = "SESSION", conflicts_with_all = ["command", "continue"])]
    resume_session: Option<String>,
    #[command(subcommand)]
    command: Option<CommandKind>,
}

#[derive(Subcommand, Debug)]
enum CommandKind {
    Bootstrap,
    Doctor,
    Migrate,
    Search { pattern: String },
    Shell { program: String, args: Vec<String> },
    Checkpoint { message: String },
    Run { objective: String },
    Resume { session: String },
}

#[derive(Debug, Serialize)]
struct DoctorCheck {
    name: &'static str,
    ok: bool,
    detail: String,
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
    let repo = cli.repo.canonicalize().unwrap_or(cli.repo);

    if cli.command.is_none() {
        let mut options = TuiOptions::for_repo(repo);
        options.initial_prompt = cli.prompt;
        options.resume_session = cli.resume_session;
        options.continue_latest = cli.r#continue;
        let _ = run_tui(options)?;
        return Ok(());
    }

    let overrides = cli.overrides.into_iter().collect::<BTreeMap<_, _>>();
    let config = Config::load_layers(None, None, &BTreeMap::new(), &overrides)?;

    match cli.command.expect("checked above") {
        CommandKind::Bootstrap => {
            bootstrap(&repo)?;
            println!("bootstrapped {}", repo.display());
            Ok(())
        }
        CommandKind::Doctor => doctor(&repo, &config),
        CommandKind::Migrate => migrate(&repo),
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

fn doctor(repo: &Path, config: &Config) -> MedusaResult<()> {
    let checks = vec![
        command_check("git", "git", &["--version"]),
        command_check("node", "node", &["--version"]),
        command_check("cargo", "cargo", &["--version"]),
        DoctorCheck {
            name: "repository",
            ok: repo.is_dir(),
            detail: repo.display().to_string(),
        },
        DoctorCheck {
            name: "provider_credential",
            ok: std::env::var("MINIMAX_API_KEY").is_ok(),
            detail: if std::env::var("MINIMAX_API_KEY").is_ok() {
                "MINIMAX_API_KEY is present".into()
            } else {
                "MINIMAX_API_KEY is absent; live model runs are unavailable".into()
            },
        },
        DoctorCheck {
            name: "model",
            ok: !config.model.name.trim().is_empty(),
            detail: config.model.name.clone(),
        },
        DoctorCheck {
            name: "state_permissions",
            ok: writable_directory(&repo.join(".medusa")),
            detail: repo.join(".medusa").display().to_string(),
        },
        DoctorCheck {
            name: "schema",
            ok: Migrator::new(repo.join(".medusa"))
                .schema_version()
                .unwrap_or_default()
                <= CURRENT_SCHEMA_VERSION,
            detail: format!("supported schema <= {CURRENT_SCHEMA_VERSION}"),
        },
    ];
    println!("{}", serde_json::to_string_pretty(&checks)?);
    if checks.iter().all(|check| check.ok) {
        Ok(())
    } else {
        Err(MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Environment,
            "one or more doctor checks failed",
        ))
    }
}

fn migrate(repo: &Path) -> MedusaResult<()> {
    let migrator = Migrator::new(repo.join(".medusa"));
    let receipts = migrator.upgrade_to_current()?;
    println!("{}", serde_json::to_string_pretty(&receipts)?);
    Ok(())
}

fn command_check(name: &'static str, program: &str, args: &[&str]) -> DoctorCheck {
    match Command::new(program).args(args).output() {
        Ok(output) => DoctorCheck {
            name,
            ok: output.status.success(),
            detail: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        },
        Err(error) => DoctorCheck {
            name,
            ok: false,
            detail: error.to_string(),
        },
    }
}

fn writable_directory(path: &Path) -> bool {
    if fs::create_dir_all(path).is_err() {
        return false;
    }
    let probe = path.join(format!("doctor-{}.tmp", std::process::id()));
    let written = fs::write(&probe, b"probe").is_ok();
    let _ = fs::remove_file(probe);
    written
}

fn print_completion(session: &medusa_agent::AgentSession) {
    println!("session {} completed", session.id);
    for item in &session.evidence {
        println!("evidence: {item}");
    }
}

fn search(repo: &Path, pattern: &str) -> MedusaResult<()> {
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

fn shell(repo: &Path, program: &str, args: &[String]) -> MedusaResult<()> {
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

fn checkpoint(repo: &Path, message: &str) -> MedusaResult<()> {
    run_git(repo, &["add", "-A"])?;
    run_git(repo, &["commit", "-m", message])
}

fn run_git(repo: &Path, args: &[&str]) -> MedusaResult<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn bare_medusa_selects_interactive_mode() {
        let cli = Cli::try_parse_from(["medusa"]).expect("parse bare invocation");
        assert!(cli.command.is_none());
        assert!(cli.prompt.is_none());
        assert!(!cli.r#continue);
    }

    #[test]
    fn headless_run_remains_available() {
        let cli = Cli::try_parse_from(["medusa", "run", "fix tests"]).expect("parse headless run");
        assert!(matches!(
            cli.command,
            Some(CommandKind::Run { objective }) if objective == "fix tests"
        ));
    }

    #[test]
    fn interactive_resume_flags_are_parsed() {
        let cli = Cli::try_parse_from(["medusa", "--resume", "session-123"])
            .expect("parse interactive resume");
        assert_eq!(cli.resume_session.as_deref(), Some("session-123"));
        assert!(cli.command.is_none());
    }
}
