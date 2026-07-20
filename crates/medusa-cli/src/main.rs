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
use medusa_daemon::{DaemonPaths, serve};
use medusa_extensions::{DesktopCommanderClient, DesktopCommanderSettings};
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
    #[arg(long)]
    prompt: Option<String>,
    #[arg(long, conflicts_with = "resume_session")]
    r#continue: bool,
    #[arg(long = "resume", value_name = "SESSION", conflicts_with = "continue")]
    resume_session: Option<String>,
    #[command(subcommand)]
    command: Option<CommandKind>,
}

#[derive(Subcommand, Debug)]
enum CommandKind {
    Bootstrap,
    Doctor,
    Migrate,
    /// Install the latest Medusa CLI from the official repository.
    Update,
    Search {
        pattern: String,
    },
    Shell {
        program: String,
        args: Vec<String>,
    },
    Checkpoint {
        message: String,
    },
    Run {
        objective: String,
    },
    Resume {
        session: String,
    },
    #[command(name = "__daemon-serve", hide = true)]
    DaemonServe,
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

    let Some(command) = cli.command else {
        let mut options = TuiOptions::for_repo(repo);
        options.initial_prompt = cli.prompt;
        options.resume_session = cli.resume_session;
        options.continue_latest = cli.r#continue;
        let _ = run_tui(options)?;
        return Ok(());
    };

    if cli.prompt.is_some() || cli.r#continue || cli.resume_session.is_some() {
        return Err(MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            "--prompt, --continue, and --resume are interactive-only and cannot be combined with a subcommand",
        ));
    }

    if matches!(command, CommandKind::DaemonServe) {
        return serve(DaemonPaths::for_repo(&repo));
    }

    let overrides = cli.overrides.into_iter().collect::<BTreeMap<_, _>>();
    let config = Config::load_layers(None, None, &BTreeMap::new(), &overrides)?;

    match command {
        CommandKind::Bootstrap => {
            bootstrap(&repo)?;
            println!("bootstrapped {}", repo.display());
            Ok(())
        }
        CommandKind::Doctor => doctor(&repo, &config),
        CommandKind::Migrate => migrate(&repo),
        CommandKind::Update => update(),
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
        CommandKind::DaemonServe => serve(DaemonPaths::for_repo(&repo)),
    }
}

fn update() -> MedusaResult<()> {
    println!("Updating Medusa from https://github.com/benclawbot/Medusa ...");
    let status = Command::new("cargo")
        .args([
            "install",
            "--git",
            "https://github.com/benclawbot/Medusa.git",
            "--locked",
            "--force",
            "medusa-cli",
        ])
        .status()
        .map_err(|error| {
            MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Environment,
                format!("could not start Cargo updater: {error}"),
            )
        })?;
    if !status.success() {
        return Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!("Cargo updater exited with {status}"),
        ));
    }
    println!("Medusa is up to date. Restart any open Medusa sessions to use the new version.");
    Ok(())
}

fn doctor(repo: &Path, config: &Config) -> MedusaResult<()> {
    let mut checks = vec![
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
    checks.push(desktop_commander_check(
        repo,
        &DesktopCommanderSettings::from_env(),
    ));
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

fn desktop_commander_check(repo: &Path, settings: &DesktopCommanderSettings) -> DoctorCheck {
    if !settings.requested() {
        return DoctorCheck {
            name: "desktop_commander_mcp",
            ok: true,
            detail: "disabled; set MEDUSA_DESKTOP_COMMANDER_ENABLED=true to opt in".to_owned(),
        };
    }
    if let Some(error) = settings.configuration_error() {
        return DoctorCheck {
            name: "desktop_commander_mcp",
            ok: false,
            detail: error.to_owned(),
        };
    }
    if !executable_available(settings.command()) {
        return DoctorCheck {
            name: "desktop_commander_mcp",
            ok: false,
            detail: format!("{} was not found on PATH", settings.command().display()),
        };
    }
    match DesktopCommanderClient::connect(repo, settings.clone()) {
        Ok(_) => DoctorCheck {
            name: "desktop_commander_mcp",
            ok: true,
            detail: format!(
                "MCP handshake ready: {} via {}",
                settings.package_label(),
                settings.command().display()
            ),
        },
        Err(error) => DoctorCheck {
            name: "desktop_commander_mcp",
            ok: false,
            detail: format!("MCP handshake failed: {error}"),
        },
    }
}

fn executable_available(program: &Path) -> bool {
    if program.components().count() > 1 {
        return program.is_file();
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|directory| {
        let candidate = directory.join(program);
        candidate.is_file()
            || (cfg!(windows)
                && ["exe", "cmd", "bat"]
                    .iter()
                    .any(|extension| candidate.with_extension(extension).is_file()))
    })
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
    fn update_command_is_available_without_extra_arguments() {
        let cli = Cli::try_parse_from(["medusa", "update"]).expect("parse update command");
        assert!(matches!(cli.command, Some(CommandKind::Update)));
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

    #[test]
    fn interactive_flags_parse_with_subcommand_for_runtime_validation() {
        let cli = Cli::try_parse_from(["medusa", "--prompt", "hello", "doctor"])
            .expect("parse before semantic validation");
        assert!(cli.command.is_some());
        assert_eq!(cli.prompt.as_deref(), Some("hello"));
    }

    #[test]
    fn hidden_daemon_host_accepts_repository_after_subcommand() {
        let cli = Cli::try_parse_from(["medusa", "__daemon-serve", "--repo", "."])
            .expect("parse daemon host");
        assert!(matches!(cli.command, Some(CommandKind::DaemonServe)));
    }
}
