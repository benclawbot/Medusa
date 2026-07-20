mod config_command;

use std::{
    collections::BTreeMap,
    fs,
    io::IsTerminal,
    path::{Path, PathBuf},
    process::Command,
};

use clap::{Parser, Subcommand};
use config_command::{
    configure_interactive, ensure_first_run, reset as reset_config, show as show_config,
};
use medusa_agent::{AgentEngine, bootstrap};
use medusa_config::Config;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_daemon::{DaemonPaths, serve};
use medusa_extensions::{DesktopCommanderClient, DesktopCommanderSettings};
use medusa_hardening::{CURRENT_SCHEMA_VERSION, Migrator};
use medusa_provider::ConfiguredProvider;
use medusa_tui::{TuiOptions, run as run_tui};
use medusa_update::{
    AtomicInstaller, AttestationVerifier, GithubAttestationVerifier, GithubReleaseClient,
    InstallKind, InstallLocation, Platform, ReleaseClient, Restart, UpdateCheck, UpdatePolicy,
    verify_sha256,
};
use serde::Serialize;
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(
    name = "medusa",
    version,
    about = "Autonomous coding agent",
    after_help = "Run `medusa` without a subcommand to open the interactive terminal. Use `medusa config` to change provider preferences and `medusa run` for headless execution."
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
    /// Configure provider, model, performance, and authentication preferences.
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
    /// Check for or install a verified release from the official repository.
    Update {
        /// Report whether a verified update is available without modifying this installation.
        #[arg(long)]
        check: bool,
        /// Apply an available update without an additional prompt (for managed automation).
        #[arg(long)]
        automatic: bool,
    },
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

#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// Print the non-secret provider profile.
    Show,
    /// Remove the provider profile so setup runs again.
    Reset,
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
        ensure_first_run()?;
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

    if let CommandKind::Config { action } = command {
        return match action {
            None => configure_interactive(),
            Some(ConfigAction::Show) => show_config(),
            Some(ConfigAction::Reset) => reset_config(),
        };
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
        CommandKind::Update { check, automatic } => update(check, automatic),
        CommandKind::Search { pattern } => search(&repo, &pattern),
        CommandKind::Shell { program, args } => shell(&repo, &program, &args),
        CommandKind::Checkpoint { message } => checkpoint(&repo, &message),
        CommandKind::Run { objective } => {
            let provider = ConfiguredProvider::manager_from_config(&config, None)?;
            let engine = AgentEngine::new(provider, config);
            let mut session = engine.create_session(&repo, objective)?;
            println!("session {} created", session.id);
            engine.run_to_completion(&mut session)?;
            print_completion(&session);
            Ok(())
        }
        CommandKind::Resume { session } => {
            let provider = ConfiguredProvider::manager_from_config(&config, None)?;
            let engine = AgentEngine::new(provider, config);
            let mut session = engine.load_session(&repo, &session)?;
            println!("session {} resumed", session.id);
            engine.run_to_completion(&mut session)?;
            print_completion(&session);
            Ok(())
        }
        CommandKind::Config { .. } => unreachable!("handled before runtime config loading"),
        CommandKind::DaemonServe => serve(DaemonPaths::for_repo(&repo)),
    }
}

fn update(check_only: bool, automatic: bool) -> MedusaResult<()> {
    let policy = UpdatePolicy::from_environment();
    let check_only = check_only || policy == UpdatePolicy::Check;
    let automatic = automatic || policy == UpdatePolicy::Automatic;
    let client = GithubReleaseClient::public()?;
    let Some(release) = client.latest()? else {
        println!("No published Medusa release is available yet; this installation is unchanged.");
        return Ok(());
    };
    match UpdateCheck::compare(env!("CARGO_PKG_VERSION"), release.version.clone()) {
        UpdateCheck::UpToDate { current } => {
            println!("Medusa {current} is up to date.");
            return Ok(());
        }
        UpdateCheck::Available { current, latest } => {
            println!("Medusa update available: {current} -> {latest}")
        }
        UpdateCheck::CurrentBuildUnparseable { current, latest } => {
            println!("Medusa build {current} can be updated to verified release {latest}");
        }
    }
    if check_only {
        return Ok(());
    }
    if !automatic && !std::io::stdin().is_terminal() {
        return Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            "refusing unattended replacement; use medusa update --automatic",
        ));
    }
    let location = InstallLocation::current()?;
    if let InstallKind::PackageManaged { manager, command } = location.kind {
        println!("This Medusa binary is managed by {manager}. Update it with: {command}");
        return Ok(());
    }
    let temporary = tempfile::tempdir()?;
    let manifest_path = temporary.path().join("medusa-release-manifest.json");
    client.download(&release.manifest, &manifest_path, |_, _| {})?;
    GithubAttestationVerifier.verify_manifest(&manifest_path, &release.repository)?;
    let artifact = release.artifact_for(&Platform::current())?;
    let archive = temporary.path().join(&artifact.name);
    println!("Downloading {}...", artifact.name);
    client.download(artifact, &archive, |written, total| match total {
        Some(total) => eprint!("\r{written}/{total} bytes"),
        None => eprint!("\r{written} bytes"),
    })?;
    eprintln!();
    verify_sha256(&archive, &artifact.sha256)?;
    let installer = AtomicInstaller::new(location.executable);
    let extracted = installer.extract_archive(&archive, &temporary.path().join("payload"))?;
    match installer.replace(&extracted, &Restart::default())? {
        Some(backup) => println!(
            "Medusa updated and restarted. Rollback binary: {}",
            backup.display()
        ),
        None => println!("Medusa replacement is scheduled after this process exits."),
    }
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
            ok: config.model.auth != "api-key" || provider_credential_present(config),
            detail: provider_credential_detail(config),
        },
        DoctorCheck {
            name: "provider_profile",
            ok: config_command::load_profile().is_ok_and(|profile| profile.configured),
            detail: config_command::config_path()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|error| error.to_string()),
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

fn provider_credential_present(config: &Config) -> bool {
    if config.model.auth != "api-key" {
        return true;
    }
    let prefix = config
        .model
        .provider
        .trim()
        .to_ascii_uppercase()
        .replace('-', "_");
    std::env::var(format!("{prefix}_API_KEY")).is_ok()
        || std::env::var("OPENAI_API_KEY").is_ok()
        || std::env::var("MEDUSA_API_KEY").is_ok()
        || std::env::var("MINIMAX_API_KEY").is_ok()
        || std::env::var("ANTHROPIC_API_KEY").is_ok()
}

fn provider_credential_detail(config: &Config) -> String {
    if config.model.auth != "api-key" {
        return format!("authentication mode: {}", config.model.auth);
    }
    if provider_credential_present(config) {
        "provider credential is present".to_owned()
    } else {
        "provider credential is absent; configure the provider-specific API key environment variable".to_owned()
    }
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
    #[cfg(windows)]
    let status = if program.eq_ignore_ascii_case("true") {
        Command::new("cmd")
            .args(["/C", "exit", "0"])
            .current_dir(repo)
            .status()?
    } else {
        Command::new(program)
            .args(args)
            .current_dir(repo)
            .status()?
    };
    #[cfg(not(windows))]
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
    fn config_command_is_available() {
        let cli = Cli::try_parse_from(["medusa", "config"]).expect("parse config command");
        assert!(matches!(
            cli.command,
            Some(CommandKind::Config { action: None })
        ));
    }

    #[test]
    fn config_show_is_available() {
        let cli = Cli::try_parse_from(["medusa", "config", "show"]).expect("parse config show");
        assert!(matches!(
            cli.command,
            Some(CommandKind::Config {
                action: Some(ConfigAction::Show)
            })
        ));
    }

    #[test]
    fn update_command_is_available_without_extra_arguments() {
        let cli =
            Cli::try_parse_from(["medusa", "update", "--check"]).expect("parse update command");
        assert!(matches!(
            cli.command,
            Some(CommandKind::Update {
                check: true,
                automatic: false
            })
        ));
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
        assert!(
            matches!(cli.command, Some(CommandKind::Run { objective }) if objective == "fix tests")
        );
    }

    #[test]
    fn interactive_resume_flags_are_parsed() {
        let cli = Cli::try_parse_from(["medusa", "--resume", "session-123"])
            .expect("parse interactive resume");
        assert_eq!(cli.resume_session.as_deref(), Some("session-123"));
        assert!(cli.command.is_none());
    }

    #[test]
    fn hidden_daemon_host_accepts_repository_after_subcommand() {
        let cli = Cli::try_parse_from(["medusa", "__daemon-serve", "--repo", "."])
            .expect("parse daemon host");
        assert!(matches!(cli.command, Some(CommandKind::DaemonServe)));
    }
}
