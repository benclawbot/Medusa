mod config_command;
mod config_profiles;
mod headless_approval;
mod telegram_command;
mod update_command;

use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Command,
};

use clap::{Parser, Subcommand};
use config_command::{
    configure_interactive, doctor as doctor_config, edit as edit_config, ensure_first_run,
    ensure_selected_runtime, get as get_config, reset as reset_config, show as show_config,
    validate as validate_config,
};
use config_profiles::{
    create as create_config_profile, delete as delete_config_profile, history as config_history,
    list as list_config_profiles, reset_section as reset_config_section, rollback as rollback_config,
    set as set_config, unset as unset_config, use_profile as use_config_profile,
};
use medusa_agent::{bootstrap, inspect_effective_model_request, session_browser::list_sessions};
use medusa_config::Config;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_daemon::{DaemonClient, DaemonPaths, Request, serve, serve_with_config};
use medusa_extensions::{DesktopCommanderClient, DesktopCommanderSettings};
use medusa_hardening::{
    CURRENT_SCHEMA_VERSION, HealthComponent, HealthReport, HealthStatus, Migrator,
    ResourceBudget, ResourceSnapshot, SupportBundle, SupportProduct,
};
use headless_approval::{ApprovalMatch, HeadlessApprovalPolicy};
use medusa_protocol::frontend::{FrontendEvent, FrontendKind};
use medusa_runtime::{
    frontend::CanonicalFrontendEventStream, prompt::PromptDraft, RuntimeController, RuntimeEvent,
};
use medusa_tui::{TuiOptions, run as run_tui};
use serde::Serialize;
use walkdir::WalkDir;

use super::oauth_preflight;

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
    /// Start a fresh interactive session, ignoring all durable session state.
    #[arg(long, conflicts_with_all = ["continue", "resume_session"])]
    fresh: bool,
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
    /// Report bounded, truthful operational health without billable or destructive probes.
    Health {
        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Write a bounded, redacted local support bundle to this JSON path.
        #[arg(long, value_name = "PATH")]
        support_bundle: Option<PathBuf>,
    },
    Migrate,
    /// Configure provider, model, performance, and authentication preferences.
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
    /// Check for or install a Medusa update.
    Update {
        /// Report whether the selected update target is current without modifying this installation.
        #[arg(long)]
        check: bool,
        /// Apply an available update without an additional prompt (for managed automation).
        #[arg(long)]
        automatic: bool,
        /// Use the latest verified prebuilt release instead of the latest main-branch source.
        #[arg(long)]
        release: bool,
        /// Permit an intentional version or rollout-sequence rollback on the release path.
        #[arg(long, requires = "release")]
        allow_downgrade: bool,
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
    /// Verify and inspect one durable effective-model-request manifest.
    RequestAudit {
        session: String,
        manifest: String,
    },
    Run {
        objective: String,
        /// Answer only allowlisted approval prompts without opening the interactive terminal.
        #[arg(long, requires = "approve_allowlist")]
        non_interactive: bool,
        /// File containing one exact shell command per line.
        #[arg(long, value_name = "PATH", requires = "non_interactive")]
        approve_allowlist: Option<PathBuf>,
    },
    Resume {
        session: String,
    },
    /// Run the Telegram remote frontend over the repository daemon authority.
    Telegram {
        #[command(flatten)]
        args: Box<telegram_command::TelegramArgs>,
    },
    #[command(name = "__daemon-serve", hide = true)]
    DaemonServe,
}

#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// Run the interactive first-run provider setup.
    Init,
    /// Print the non-secret provider profile.
    Show {
        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Edit the active provider profile and commit it only when the complete result is valid.
    Edit,
    /// Print one non-secret provider-profile key.
    Get {
        key: String,
        /// Emit the value as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Set one known non-secret provider-profile key.
    Set { key: String, value: String },
    /// Reset one known provider-profile key to its default.
    Unset { key: String },
    /// Reset a typed provider-profile section to defaults.
    ResetSection { section: String },
    /// Show bounded, non-secret known-good provider-profile history.
    History {
        #[arg(long)]
        json: bool,
    },
    /// Restore the previous known-good provider profile.
    Rollback,
    /// Manage named provider profiles.
    Profiles {
        #[command(subcommand)]
        action: ConfigProfileAction,
    },
    /// Validate the shared provider profile without billable provider work.
    Validate {
        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run redacted, non-billable configuration diagnostics.
    Doctor {
        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Remove the active provider profile so setup runs again.
    Reset,
}

#[derive(Subcommand, Debug)]
enum ConfigProfileAction {
    /// List named profiles and the built-in default profile.
    List {
        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Create a named profile from the current active configuration.
    Create { name: String },
    /// Select a named profile or the built-in `default` profile.
    Use { name: String },
    /// Delete an inactive named profile.
    Delete { name: String },
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
    let repo = repository_path(&cli.repo);

    let Some(command) = cli.command else {
        ensure_first_run()?;
        ensure_selected_runtime()?;
        let overrides = cli
            .overrides
            .iter()
            .cloned()
            .collect::<BTreeMap<_, _>>();
        let config = Config::load_layers(None, None, &BTreeMap::new(), &overrides)?;
        oauth_preflight::run_if_needed(&config)?;
        medusa_update::acknowledge_update_health()?;
        let mut options = TuiOptions::for_repo(repo);
        options.initial_prompt = cli.prompt;
        options.resume_session = cli.resume_session;
        options.continue_latest = cli.r#continue;
        let _ = run_tui(options)?;
        return Ok(());
    };

    if cli.prompt.is_some() || cli.fresh || cli.r#continue || cli.resume_session.is_some() {
        return Err(MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            "--prompt, --fresh, --continue, and --resume are interactive-only and cannot be combined with a subcommand",
        ));
    }

    if matches!(command, CommandKind::DaemonServe) {
        let overrides = cli
            .overrides
            .iter()
            .cloned()
            .collect::<BTreeMap<_, _>>();
        let project = repo.join(".medusa/config.toml");
        let project = project.exists().then_some(project);
        let config = Config::load_layers(
            None,
            project.as_deref(),
            &BTreeMap::new(),
            &overrides,
        )?;
        return serve_with_config(DaemonPaths::for_repo(&repo), config);
    }

    let command = match command {
        CommandKind::Telegram { args } => return telegram_command::run(&repo, *args),
        command => command,
    };

    if let CommandKind::Config { action } = command {
        return match action {
            None | Some(ConfigAction::Init) => configure_interactive(),
            Some(ConfigAction::Show { json }) => show_config(json),
            Some(ConfigAction::Edit) => edit_config(),
            Some(ConfigAction::Get { key, json }) => get_config(&key, json),
            Some(ConfigAction::Set { key, value }) => set_config(&key, &value),
            Some(ConfigAction::Unset { key }) => unset_config(&key),
            Some(ConfigAction::ResetSection { section }) => reset_config_section(&section),
            Some(ConfigAction::History { json }) => config_history(json),
            Some(ConfigAction::Rollback) => rollback_config(),
            Some(ConfigAction::Profiles { action }) => match action {
                ConfigProfileAction::List { json } => list_config_profiles(json),
                ConfigProfileAction::Create { name } => create_config_profile(&name),
                ConfigProfileAction::Use { name } => use_config_profile(&name),
                ConfigProfileAction::Delete { name } => delete_config_profile(&name),
            },
            Some(ConfigAction::Validate { json }) => validate_config(json),
            Some(ConfigAction::Doctor { json }) => doctor_config(json),
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
        CommandKind::Health {
            json,
            support_bundle,
        } => health(&repo, &config, json, support_bundle.as_deref()),
        CommandKind::Migrate => migrate(&repo),
        CommandKind::Update {
            check,
            automatic,
            release,
            allow_downgrade,
        } => update_command::run(&repo, check, automatic, release, allow_downgrade),
        CommandKind::Search { pattern } => search(&repo, &pattern),
        CommandKind::Shell { program, args } => shell(&repo, &program, &args),
        CommandKind::Checkpoint { message } => checkpoint(&repo, &message),
        CommandKind::RequestAudit { session, manifest } => {
            let audit = inspect_effective_model_request(&repo, &session, &manifest)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&audit).map_err(|error| MedusaError::new(
                    ErrorCode::InvalidEvent,
                    ErrorCategory::Internal,
                    format!("could not render request audit: {error}"),
                ))?
            );
            Ok(())
        }
        CommandKind::Run {
            objective,
            non_interactive,
            approve_allowlist,
        } => {
            ensure_selected_runtime()?;
            oauth_preflight::run_if_needed(&config)?;
            let approval_policy = HeadlessApprovalPolicy::load(
                non_interactive,
                approve_allowlist.as_deref(),
            )?;
            let runtime = RuntimeController::start_with_config(repo.clone(), config);
            runtime
                .submit(PromptDraft {
                    text: objective,
                    ..PromptDraft::default()
                })
                .map_err(runtime_error)?;
            drain_headless_runtime(&runtime, &repo, approval_policy.as_ref())
        }
        CommandKind::Resume { session } => {
            ensure_selected_runtime()?;
            oauth_preflight::run_if_needed(&config)?;
            let runtime = RuntimeController::start_resumed_with_config(repo.clone(), &session, config)
                .map_err(runtime_error)?;
            runtime
                .submit(PromptDraft {
                    text: "Continue the current task from its durable session state.".to_owned(),
                    ..PromptDraft::default()
                })
                .map_err(runtime_error)?;
            drain_headless_runtime(&runtime, &repo, None)
        }
        CommandKind::Config { .. } => unreachable!("handled before runtime config loading"),
        CommandKind::Telegram { .. } => unreachable!("handled before runtime config loading"),
        CommandKind::DaemonServe => serve(DaemonPaths::for_repo(&repo)),
    }
}

fn runtime_error(error: medusa_runtime::RuntimeError) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Execution,
        error.to_string(),
    )
}

/// Resolves the repository argument to an absolute path.
///
/// On Windows `canonicalize` returns a `\\?\` verbatim path. That prefix leaks
/// into diagnostics and daemon endpoint paths, and several Win32 consumers
/// reject it, so it is removed while keeping the resolved location.
fn repository_path(requested: &Path) -> PathBuf {
    let resolved = requested
        .canonicalize()
        .unwrap_or_else(|_| requested.to_path_buf());
    let text = resolved.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => resolved,
    }
}

fn drain_headless_runtime(
    runtime: &RuntimeController,
    repo: &Path,
    approval_policy: Option<&HeadlessApprovalPolicy>,
) -> MedusaResult<()> {
    let mut stream = CanonicalFrontendEventStream::new(repo.to_path_buf(), FrontendKind::Headless);
    let mut automatically_answered_question = false;
    loop {
        let runtime_event = runtime.try_event().map_err(runtime_error)?;
        match runtime_event {
            Some(RuntimeEvent::Question(question)) => {
                let Some(policy) = approval_policy else {
                    continue;
                };
                match policy.matches(&question) {
                    ApprovalMatch::Approved(command) => {
                        println!("approved allowlisted command: {command}");
                        runtime
                            .submit(PromptDraft {
                                text: "approve".to_owned(),
                                ..PromptDraft::default()
                            })
                            .map_err(runtime_error)?;
                        automatically_answered_question = true;
                    }
                    ApprovalMatch::Missing(command) => {
                        return Err(MedusaError::new(
                            ErrorCode::PolicyDenied,
                            ErrorCategory::Policy,
                            format!(
                                "headless approval denied for `{command}` because it is not listed in {}. Add the exact command and rerun with `medusa run --non-interactive --approve-allowlist {} <objective>`.",
                                policy.source().display(),
                                policy.source().display()
                            ),
                        ));
                    }
                    ApprovalMatch::NotApproval => {}
                }
            }
            Some(RuntimeEvent::Failed(error))
                if runtime.active_session_id().is_none()
                    || is_unjournaled_runtime_failure(&error) =>
            {
                return Err(MedusaError::new(
                    ErrorCode::DependencyUnavailable,
                    ErrorCategory::Execution,
                    error,
                ));
            }
            _ => {}
        }

        let Some(session_id) = runtime.active_session_id() else {
            std::thread::yield_now();
            continue;
        };
        let mut emitted = false;
        while let Some(envelope) = stream.try_event(&session_id).map_err(runtime_error)? {
            emitted = true;
            match envelope.event {
                FrontendEvent::Started if automatically_answered_question => {
                    automatically_answered_question = false;
                }
                FrontendEvent::AssistantTextDelta { text }
                | FrontendEvent::AssistantInterim { text } => println!("{text}"),
                FrontendEvent::Activity(activity) => {
                    println!("{}: {}", activity.title, activity.details.join("; "));
                }
                FrontendEvent::Notice { title, details, .. } => {
                    println!("{title}: {}", details.join("; "));
                }
                FrontendEvent::Question(question) => {
                    if automatically_answered_question {
                        continue;
                    }
                    return Err(MedusaError::new(
                        ErrorCode::DependencyUnavailable,
                        ErrorCategory::Execution,
                        format!(
                            "agent is waiting for user input, which headless execution cannot provide: {}. For an approval prompt, create an allowlist and rerun with `medusa run --non-interactive --approve-allowlist .medusa/approve.txt <objective>`; otherwise use the interactive terminal.",
                            question.prompt
                        ),
                    ));
                }
                FrontendEvent::ApprovalRequired(approval) => {
                    if automatically_answered_question {
                        continue;
                    }
                    return Err(MedusaError::new(
                        ErrorCode::DependencyUnavailable,
                        ErrorCategory::Execution,
                        format!(
                            "agent requires approval for {} ({}) and headless execution has no matching allowlist decision",
                            approval.action, approval.scope
                        ),
                    ));
                }
                FrontendEvent::Completed { summary } => {
                    if let Some(summary) = summary {
                        println!("session {session_id} completed: {summary}");
                    } else {
                        println!("session {session_id} completed");
                    }
                    return Ok(());
                }
                FrontendEvent::TurnFinished => return Ok(()),
                FrontendEvent::Cancelled { .. } => {
                    return Err(MedusaError::new(
                        ErrorCode::DependencyUnavailable,
                        ErrorCategory::Execution,
                        "agent execution was cancelled",
                    ));
                }
                FrontendEvent::Failed { message, .. } => {
                    return Err(MedusaError::new(
                        ErrorCode::DependencyUnavailable,
                        ErrorCategory::Execution,
                        message,
                    ));
                }
                _ => {}
            }
        }
        if !emitted {
            std::thread::yield_now();
        }
    }
}

fn is_unjournaled_runtime_failure(message: &str) -> bool {
    message.starts_with("runtime event was not published because")
}

fn request_daemon_shutdown(repo: &Path) {
    let paths = DaemonPaths::for_repo(repo);
    let _ = DaemonClient::new(&paths.socket).request(Request::ShutdownNow);
}

fn health(
    repo: &Path,
    config: &Config,
    json: bool,
    support_bundle: Option<&Path>,
) -> MedusaResult<()> {
    let mut components = Vec::new();
    components.push(HealthComponent::new(
        "repository",
        if repo.is_dir() {
            HealthStatus::HealthyReady
        } else {
            HealthStatus::BlockedUserAction
        },
        if repo.is_dir() {
            "repository path is readable"
        } else {
            "repository path does not exist or is not a directory"
        },
        (!repo.is_dir()).then_some("choose an existing repository with --repo".to_owned()),
    )?);

    let state_root = repo.join(".medusa");
    let writable = writable_directory(&state_root);
    components.push(HealthComponent::new(
        "state_store",
        if writable {
            HealthStatus::HealthyReady
        } else {
            HealthStatus::BlockedUserAction
        },
        if writable {
            "Medusa state directory is writable"
        } else {
            "Medusa state directory is missing or not writable"
        },
        (!writable).then_some("run `medusa bootstrap` or repair the .medusa directory permissions".to_owned()),
    )?);

    let schema_path = state_root.clone();
    let schema = match Migrator::new(&schema_path).schema_version() {
        Ok(version) if version <= CURRENT_SCHEMA_VERSION => HealthComponent::new(
            "schema",
            HealthStatus::HealthyReady,
            format!("state schema {version} is supported (maximum {CURRENT_SCHEMA_VERSION})"),
            None,
        )?,
        Ok(version) => HealthComponent::new(
            "schema",
            HealthStatus::UnsafeQuarantine,
            format!("state schema {version} is newer than supported schema {CURRENT_SCHEMA_VERSION}"),
            Some("use a compatible Medusa build before writing authoritative state".to_owned()),
        )?,
        Err(error) => HealthComponent::new(
            "schema",
            HealthStatus::UnhealthyRecoveryRequired,
            format!("state schema could not be read: {error}"),
            Some("run `medusa migrate` after preserving the .medusa directory".to_owned()),
        )?,
    };
    components.push(schema);

    for check in bounded_runtime_health_checks(repo) {
        let status = if check.ok {
            HealthStatus::HealthyReady
        } else if check.name.contains("checkpoint") || check.name.contains("journal") {
            HealthStatus::UnhealthyRecoveryRequired
        } else {
            HealthStatus::BlockedUserAction
        };
        components.push(HealthComponent::new(
            format!("runtime.{}", check.name),
            status,
            check.detail,
            (!check.ok).then_some(
                "inspect the durable session evidence and use the recovery command before resuming"
                    .to_owned(),
            ),
        )?);
    }

    let (bytes, entries, truncated) = measure_state_tree(&state_root);
    let budget = ResourceBudget::new("disk_state", 64 * 1024 * 1024, 10_000)?;
    let resource = ResourceSnapshot::from_usage(budget, bytes, entries);
    components.push(resource.health_component()?);
    if truncated {
        components.push(HealthComponent::new(
            "disk_scan",
            HealthStatus::DegradedSafe,
            "state scan reached its bounded inspection limit",
            Some("inspect state usage with the support bundle before increasing workload".to_owned()),
        )?);
    }

    let provider_status = if config.model.provider.trim().is_empty() {
        HealthStatus::BlockedUserAction
    } else {
        HealthStatus::OptionalUnavailable
    };
    components.push(HealthComponent::new(
        "provider_route",
        provider_status,
        if config.model.provider.trim().is_empty() {
            "no provider route is configured; no billable probe was attempted"
        } else {
            "provider route is configured but live readiness was not probed"
        },
        Some("use `medusa config doctor` for redacted configuration checks; live provider work requires an explicit run".to_owned()),
    )?);

    for (id, summary) in [
        ("daemon", "daemon readiness is owned by the daemon lifecycle and was not inferred from a file"),
        ("process_containment", "containment readiness was not probed because that would launch a process"),
        ("analysis_workspace", "analysis workspace readiness is optional and was not started"),
        ("frontend_gateway", "frontend attachment readiness is session-scoped and was not inferred"),
    ] {
        components.push(HealthComponent::new(
            id,
            HealthStatus::OptionalUnavailable,
            summary,
            None,
        )?);
    }

    let report = HealthReport::new(components)?;
    let events = recent_operational_events(repo);
    if let Some(path) = support_bundle {
        let bundle = SupportBundle::new(
            SupportProduct {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                commit: option_env!("MEDUSA_GIT_COMMIT").map(str::to_owned),
                platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            },
            BTreeMap::from([
                ("config_schema_version".to_owned(), config.version.to_string()),
                ("agent_mode".to_owned(), format!("{:?}", config.agent.mode)),
                ("provider".to_owned(), config.model.provider.clone()),
                ("model".to_owned(), config.model.name.clone()),
                ("protocol".to_owned(), config.model.protocol.clone()),
                ("auth_class".to_owned(), config.model.auth.clone()),
                ("tool_calling".to_owned(), config.model.tool_calling.to_string()),
                ("streaming".to_owned(), config.model.streaming.to_string()),
                (
                    "fallback_route_count".to_owned(),
                    config.model.fallback_providers.len().to_string(),
                ),
                (
                    "role_route_count".to_owned(),
                    config.model.role_routes.len().to_string(),
                ),
            ]),
            report.clone(),
            vec![resource],
            &events,
        )?;
        let manifest = bundle.write_to(path)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        } else {
            println!(
                "wrote redacted support bundle {} ({} bytes, {} events)",
                path.display(), manifest.bytes, manifest.event_count
            );
        }
    } else if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("operational status: {:?}", report.status);
        for component in &report.components {
            println!("{:?}: {} — {}", component.status, component.id, component.summary);
        }
    }

    if report.safe_to_continue {
        Ok(())
    } else {
        Err(MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Environment,
            format!("operational health requires attention: {:?}", report.status),
        ))
    }
}

fn measure_state_tree(root: &Path) -> (u64, u64, bool) {
    const MAX_ENTRIES: u64 = 10_000;
    const MAX_BYTES: u64 = 64 * 1024 * 1024;
    if !root.is_dir() {
        return (0, 0, false);
    }
    let mut bytes = 0_u64;
    let mut entries = 0_u64;
    let mut truncated = false;
    for entry in WalkDir::new(root).follow_links(false) {
        if entries >= MAX_ENTRIES || bytes >= MAX_BYTES {
            truncated = true;
            break;
        }
        let Ok(entry) = entry else {
            truncated = true;
            continue;
        };
        entries = entries.saturating_add(1);
        if entry.file_type().is_file() {
            bytes = bytes.saturating_add(entry.metadata().map(|metadata| metadata.len()).unwrap_or(0));
        }
    }
    (bytes, entries, truncated)
}

fn bounded_runtime_health_checks(repo: &Path) -> Vec<DoctorCheck> {
    const MAX_SESSIONS: usize = 16;
    let sessions = match list_sessions(repo) {
        Ok(sessions) => sessions,
        Err(error) => {
            return vec![DoctorCheck {
                name: "execution_journal",
                ok: false,
                detail: format!("durable session discovery failed: {error}"),
            }];
        }
    };
    let total = sessions.len();
    let mut errors = Vec::new();
    for session in sessions.into_iter().take(MAX_SESSIONS) {
        match medusa_runtime::execution_history::inspect(repo, &session.id) {
            Ok(health) if health.replay.equivalent => {}
            Ok(_) => errors.push(format!("session {} replay diverges", session.id)),
            Err(error) => errors.push(format!("session {}: {error}", session.id)),
        }
    }
    let sampled = total.min(MAX_SESSIONS);
    vec![DoctorCheck {
        name: "execution_journal",
        ok: errors.is_empty(),
        detail: if errors.is_empty() {
            if total > sampled {
                format!("verified {sampled} of {total} session journals (bounded sample)")
            } else {
                format!("verified {sampled} session journal(s)")
            }
        } else {
            errors.join("; ")
        },
    }]
}

fn recent_operational_events(repo: &Path) -> Vec<medusa_hardening::OperationalEvent> {
    let path = repo.join(".medusa/diagnostics/events.jsonl");
    let Ok(mut file) = fs::File::open(path) else {
        return Vec::new();
    };
    if let Ok(metadata) = file.metadata() {
        let start = metadata.len().saturating_sub(512 * 1024);
        let _ = file.seek(SeekFrom::Start(start));
    }
    let mut bytes = Vec::new();
    let _ = file
        .take(512 * 1024)
        .read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes)
        .lines()
        .rev()
        .take(64)
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
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
    checks.extend(runtime_durability_checks(repo));
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

fn runtime_durability_checks(repo: &Path) -> Vec<DoctorCheck> {
    let sessions = match list_sessions(repo) {
        Ok(sessions) => sessions,
        Err(error) => {
            let detail = format!("durable session discovery failed: {error}");
            return vec![
                DoctorCheck {
                    name: "execution_journal",
                    ok: false,
                    detail: detail.clone(),
                },
                DoctorCheck {
                    name: "runtime_checkpoints",
                    ok: false,
                    detail,
                },
            ];
        }
    };

    let mut journal_cursor = 0_u64;
    let mut journal_errors = Vec::new();
    let mut replayed_sessions = 0_usize;
    for session in &sessions {
        match medusa_runtime::execution_history::inspect(repo, &session.id) {
            Ok(health) if health.replay.equivalent => {
                replayed_sessions = replayed_sessions.saturating_add(1);
                journal_cursor = journal_cursor.saturating_add(health.journal_cursor);
            }
            Ok(_) => journal_errors.push(format!(
                "session {} replay diverges from its materialized state",
                session.id
            )),
            Err(error) => journal_errors.push(format!("session {}: {error}", session.id)),
        }
    }
    let journal = DoctorCheck {
        name: "execution_journal",
        ok: journal_errors.is_empty(),
        detail: if journal_errors.is_empty() {
            if sessions.is_empty() {
                "no durable sessions found; journal storage is ready".to_owned()
            } else {
                format!(
                    "verified {replayed_sessions} session journal(s) through {journal_cursor} total event cursor(s)"
                )
            }
        } else {
            format!(
                "{} journal verification failure(s): {}",
                journal_errors.len(),
                journal_errors.join("; ")
            )
        },
    };

    let mut checkpoint_count = 0_usize;
    let mut sessions_without_checkpoint = 0_usize;
    let mut latest_cursor = None::<u64>;
    let mut checkpoint_errors = Vec::new();
    for session in &sessions {
        match medusa_runtime::checkpoint_store::list(repo, &session.id) {
            Ok(records) => {
                if records.is_empty() {
                    sessions_without_checkpoint = sessions_without_checkpoint.saturating_add(1);
                }
                checkpoint_count = checkpoint_count.saturating_add(records.len());
                if let Some(cursor) = records.last().map(|record| record.journal_cursor) {
                    latest_cursor = Some(latest_cursor.map_or(cursor, |current| current.max(cursor)));
                }
            }
            Err(error) => checkpoint_errors.push(format!("session {}: {error}", session.id)),
        }
    }
    let checkpoints = DoctorCheck {
        name: "runtime_checkpoints",
        ok: checkpoint_errors.is_empty(),
        detail: if checkpoint_errors.is_empty() {
            if sessions.is_empty() {
                "no sessions require checkpoint recovery".to_owned()
            } else if checkpoint_count == 0 {
                format!(
                    "{sessions_without_checkpoint} session(s) are journal-resumable; no safe-boundary checkpoint exists yet"
                )
            } else {
                format!(
                    "verified {checkpoint_count} checkpoint(s); latest recoverable cursor {}; {sessions_without_checkpoint} session(s) currently rely on journal resume",
                    latest_cursor.unwrap_or_default()
                )
            }
        } else {
            format!(
                "{} checkpoint verification failure(s): {}",
                checkpoint_errors.len(),
                checkpoint_errors.join("; ")
            )
        },
    };

    vec![journal, checkpoints]
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
    ensure_git_root(repo)?;
    run_git(repo, &["add", "-A"])?;
    run_git(repo, &["commit", "-m", message])
}

fn ensure_git_root(repo: &Path) -> MedusaResult<()> {
    let expected = repo.canonicalize().map_err(|error| {
        MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!("git add -A failed to resolve repository path {}: {error}", repo.display()),
        )
    })?;
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(repo)
        .output()?;
    if !output.status.success() {
        return Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!("git add -A failed with {}", output.status),
        ));
    }
    let discovered = String::from_utf8_lossy(&output.stdout);
    let discovered = Path::new(discovered.trim()).canonicalize().map_err(|error| {
        MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!("git add -A failed to resolve discovered repository root: {error}"),
        )
    })?;
    if discovered != expected {
        return Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!(
                "git add -A failed: repository root {} does not match requested path {}",
                discovered.display(),
                expected.display()
            ),
        ));
    }
    Ok(())
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
    use medusa_agent::AgentEngine;
    use medusa_config::Config;
    use medusa_core::MedusaResult;
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};

    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("session creation does not call the provider")
        }
    }

    #[test]
    fn clap_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn repository_path_preserves_existing_and_missing_paths() {
        let directory = tempfile::tempdir().expect("tempdir");
        let existing = directory.path().join("nested");
        std::fs::create_dir(&existing).expect("nested directory");
        assert_eq!(
            repository_path(&existing).canonicalize().expect("resolved path"),
            existing.canonicalize().expect("existing path")
        );

        let missing = directory.path().join("does-not-exist");
        assert_eq!(repository_path(&missing), missing);
    }

    #[test]
    fn executable_and_writable_directory_checks_cover_path_edges() {
        let directory = tempfile::tempdir().expect("tempdir");
        let executable = directory.path().join("fixture.exe");
        std::fs::write(&executable, b"fixture").expect("fixture");
        assert!(executable_available(&executable));
        assert!(!executable_available(&directory.path().join("missing.exe")));
        assert!(executable_available(Path::new("git")));
        assert!(!executable_available(Path::new("medusa-command-that-does-not-exist")));

        assert!(writable_directory(&directory.path().join("writable")));
        let file_path = directory.path().join("not-a-directory");
        std::fs::write(&file_path, b"file").expect("file");
        assert!(!writable_directory(&file_path));
    }

    fn init_test_repository(path: &Path) {
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(path)
            .status()
            .expect("git init");
        assert!(status.success());
        for (key, value) in [("user.email", "medusa-tests@example.invalid"), ("user.name", "Medusa Tests")] {
            let status = Command::new("git")
                .args(["config", key, value])
                .current_dir(path)
                .status()
                .expect("git config");
            assert!(status.success());
        }
    }

    #[test]
    fn git_root_validation_accepts_exact_root_and_rejects_enclosing_root() {
        let repository = tempfile::tempdir().expect("repository");
        init_test_repository(repository.path());
        ensure_git_root(repository.path()).expect("exact git root");

        let child = repository.path().join("child");
        std::fs::create_dir(&child).expect("child");
        let error = ensure_git_root(&child).expect_err("nested path must not be treated as root");
        assert!(error.to_string().contains("does not match requested path"));
    }

    #[test]
    fn git_root_validation_reports_non_git_and_missing_paths() {
        let directory = tempfile::tempdir().expect("directory");
        let error = ensure_git_root(directory.path()).expect_err("non-git path");
        assert_eq!(error.code, ErrorCode::ToolExecutionFailed);
        assert!(error.to_string().contains("git add -A failed"));

        let missing = directory.path().join("missing");
        let error = ensure_git_root(&missing).expect_err("missing path");
        assert!(error.to_string().contains("failed to resolve repository path"));
    }

    #[test]
    fn checkpoint_commits_only_after_root_validation() {
        let repository = tempfile::tempdir().expect("repository");
        init_test_repository(repository.path());
        std::fs::write(repository.path().join("value.txt"), b"checkpoint").expect("value");

        checkpoint(repository.path(), "test checkpoint").expect("checkpoint");
        let output = Command::new("git")
            .args(["log", "-1", "--pretty=%s"])
            .current_dir(repository.path())
            .output()
            .expect("git log");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "test checkpoint");
    }

    #[test]
    fn run_git_reports_nonzero_status() {
        let directory = tempfile::tempdir().expect("directory");
        let error = run_git(directory.path(), &["status", "--definitely-invalid"])
            .expect_err("invalid git argument");
        assert!(error.to_string().contains("git status --definitely-invalid failed"));
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
                action: Some(ConfigAction::Show { json: false })
            })
        ));
    }

    #[test]
    fn config_set_is_available() {
        let cli = Cli::try_parse_from(["medusa", "config", "set", "model", "gpt-5"])
            .expect("parse config set");
        assert!(matches!(
            cli.command,
            Some(CommandKind::Config {
                action: Some(ConfigAction::Set { key, value })
            }) if key == "model" && value == "gpt-5"
        ));
    }

    #[test]
    fn config_profiles_use_is_available() {
        let cli = Cli::try_parse_from([
            "medusa", "config", "profiles", "use", "work"
        ])
        .expect("parse profile selection");
        assert!(matches!(
            cli.command,
            Some(CommandKind::Config {
                action: Some(ConfigAction::Profiles {
                    action: ConfigProfileAction::Use { name }
                })
            }) if name == "work"
        ));
    }

    #[test]
    fn update_command_defaults_to_main() {
        let cli =
            Cli::try_parse_from(["medusa", "update", "--check"]).expect("parse update command");
        assert!(matches!(
            cli.command,
            Some(CommandKind::Update {
                check: true,
                automatic: false,
                release: false,
                allow_downgrade: false,
            })
        ));
    }

    #[test]
    fn update_command_accepts_explicit_release() {
        let cli = Cli::try_parse_from(["medusa", "update", "--release", "--check"])
            .expect("parse release update command");
        assert!(matches!(
            cli.command,
            Some(CommandKind::Update {
                check: true,
                automatic: false,
                release: true,
                allow_downgrade: false,
            })
        ));
    }

    #[test]
    fn update_downgrade_requires_release() {
        assert!(Cli::try_parse_from(["medusa", "update", "--allow-downgrade"]).is_err());
        let cli = Cli::try_parse_from([
            "medusa",
            "update",
            "--release",
            "--allow-downgrade",
        ])
        .expect("release downgrade is explicit");
        assert!(matches!(
            cli.command,
            Some(CommandKind::Update {
                release: true,
                allow_downgrade: true,
                ..
            })
        ));
    }

    #[test]
    fn legacy_update_channel_selector_is_rejected() {
        assert!(Cli::try_parse_from(["medusa", "update", "--channel", "source"]).is_err());
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
            matches!(cli.command, Some(CommandKind::Run { objective, .. }) if objective == "fix tests")
        );
    }

    #[test]
    fn headless_run_accepts_explicit_approval_allowlist() {
        let cli = Cli::try_parse_from([
            "medusa",
            "run",
            "--non-interactive",
            "--approve-allowlist",
            ".medusa/approve.txt",
            "fix tests",
        ])
        .expect("parse allowlisted headless run");
        assert!(matches!(
            cli.command,
            Some(CommandKind::Run {
                objective,
                non_interactive: true,
                approve_allowlist: Some(path),
            }) if objective == "fix tests" && path == PathBuf::from(".medusa/approve.txt")
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
    fn runtime_doctor_accepts_empty_repository_state() {
        let repository = tempfile::tempdir().expect("repository");
        let checks = runtime_durability_checks(repository.path());
        assert_eq!(checks.len(), 2);
        assert!(checks.iter().all(|check| check.ok));
    }

    #[test]
    fn runtime_doctor_verifies_a_canonical_session_journal() {
        let repository = tempfile::tempdir().expect("repository");
        AgentEngine::new(UnusedProvider, Config::default())
            .create_session(repository.path(), "Verify durable journal".to_owned())
            .expect("session");
        let checks = runtime_durability_checks(repository.path());
        assert!(
            checks
                .iter()
                .find(|check| check.name == "execution_journal")
                .is_some_and(|check| check.ok && check.detail.contains("verified 1 session"))
        );
    }

    #[test]
    fn runtime_doctor_reports_corrupt_checkpoint_artifacts() {
        let repository = tempfile::tempdir().expect("repository");
        let session = AgentEngine::new(UnusedProvider, Config::default())
            .create_session(repository.path(), "Detect corrupt checkpoint".to_owned())
            .expect("session");
        let directory = repository
            .path()
            .join(".medusa/checkpoints")
            .join(session.id.as_str());
        fs::create_dir_all(&directory).expect("checkpoint directory");
        fs::write(directory.join("corrupt.json"), b"not-json").expect("corrupt checkpoint");

        let checks = runtime_durability_checks(repository.path());
        let checkpoint = checks
            .iter()
            .find(|check| check.name == "runtime_checkpoints")
            .expect("checkpoint check");
        assert!(!checkpoint.ok);
        assert!(checkpoint.detail.contains("checkpoint verification failure"));
    }

    #[test]
    fn hidden_daemon_host_accepts_repository_after_subcommand() {
        let cli = Cli::try_parse_from(["medusa", "__daemon-serve", "--repo", "."])
            .expect("parse daemon host");
        assert!(matches!(cli.command, Some(CommandKind::DaemonServe)));
    }

    #[test]
    fn headless_runtime_fails_closed_when_terminal_publication_is_not_journaled() {
        assert!(is_unjournaled_runtime_failure(
            "runtime event was not published because its durable record failed: disk full"
        ));
        assert!(is_unjournaled_runtime_failure(
            "runtime event was not published because durable serialization failed: invalid value"
        ));
        assert!(!is_unjournaled_runtime_failure(
            "provider returned an ordinary session-bound failure"
        ));
    }
}
