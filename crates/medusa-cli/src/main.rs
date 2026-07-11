use std::{collections::BTreeMap, fs, path::PathBuf, process::Command};

use clap::{Parser, Subcommand};
use medusa_config::Config;
use medusa_core::{CorrelationId, MedusaResult, SessionId};
use medusa_protocol::{Actor, EventEnvelope, EventPayload};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
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

#[derive(Debug, Serialize, Deserialize)]
struct SessionRecord {
    id: SessionId,
    objective: String,
    repo: PathBuf,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    completed: bool,
    events: Vec<EventEnvelope>,
}

fn parse_key_value(raw: &str) -> Result<(String, String), String> {
    raw.split_once('=')
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .ok_or_else(|| "expected key=value".to_owned())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{}", serde_json::to_string_pretty(&error).unwrap_or_else(|_| error.to_string()));
        std::process::exit(1);
    }
}

fn run() -> MedusaResult<()> {
    let cli = Cli::parse();
    let overrides = cli.overrides.into_iter().collect::<BTreeMap<_, _>>();
    let config = Config::load_layers(None, None, &BTreeMap::new(), &overrides)?;
    let repo = cli.repo.canonicalize().unwrap_or(cli.repo);

    match cli.command {
        CommandKind::Bootstrap => bootstrap(&repo),
        CommandKind::Search { pattern } => search(&repo, &pattern),
        CommandKind::Shell { program, args } => shell(&repo, &program, &args),
        CommandKind::Checkpoint { message } => checkpoint(&repo, &message),
        CommandKind::Run { objective } => run_session(&repo, objective, &config),
        CommandKind::Resume { session } => resume_session(&repo, &session, &config),
    }
}

fn medusa_dir(repo: &PathBuf) -> PathBuf {
    repo.join(".medusa")
}

fn bootstrap(repo: &PathBuf) -> MedusaResult<()> {
    fs::create_dir_all(medusa_dir(repo).join("sessions"))?;
    let map = repo.join("REPOSITORY_MAP.md");
    if !map.exists() {
        fs::write(
            &map,
            "# Repository Map\n\n## Overview\n\n## Languages and Frameworks\n\n## Entry Points\n\n## Build and Run Commands\n\n## Test Commands\n\n## Critical Invariants\n",
        )?;
    }
    println!("bootstrapped {}", repo.display());
    Ok(())
}

fn search(repo: &PathBuf, pattern: &str) -> MedusaResult<()> {
    for entry in WalkDir::new(repo).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() || entry.path().components().any(|part| part.as_os_str() == ".git") {
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

fn shell(repo: &PathBuf, program: &str, args: &[String]) -> MedusaResult<()> {
    let status = Command::new(program).args(args).current_dir(repo).status()?;
    if !status.success() {
        return Err(medusa_core::MedusaError::new(
            medusa_core::ErrorCode::ToolExecutionFailed,
            medusa_core::ErrorCategory::Execution,
            format!("command exited with {status}"),
        ));
    }
    Ok(())
}

fn checkpoint(repo: &PathBuf, message: &str) -> MedusaResult<()> {
    let status = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .status()?;
    if !status.success() {
        return Err(tool_error("git add failed"));
    }
    let status = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(repo)
        .status()?;
    if !status.success() {
        return Err(tool_error("git commit failed"));
    }
    Ok(())
}

fn run_session(repo: &PathBuf, objective: String, _config: &Config) -> MedusaResult<()> {
    bootstrap(repo)?;
    let now = OffsetDateTime::now_utc();
    let session_id = SessionId::new();
    let correlation = CorrelationId::new();
    let event = EventEnvelope::new(
        session_id,
        correlation,
        Actor::User,
        now,
        None,
        EventPayload::SessionCreated { objective: objective.clone() },
    )?;
    let record = SessionRecord {
        id: session_id,
        objective,
        repo: repo.clone(),
        created_at: now,
        updated_at: now,
        completed: false,
        events: vec![event],
    };
    persist_session(repo, &record)?;
    println!("session {} created", record.id);
    Ok(())
}

fn resume_session(repo: &PathBuf, session: &str, _config: &Config) -> MedusaResult<()> {
    let path = medusa_dir(repo).join("sessions").join(format!("{session}.json"));
    let mut record: SessionRecord = serde_json::from_slice(&fs::read(&path)?)?;
    verify_chain(&record.events)?;
    record.updated_at = OffsetDateTime::now_utc();
    persist_session(repo, &record)?;
    println!("session {} resumed: {}", record.id, record.objective);
    Ok(())
}

fn persist_session(repo: &PathBuf, record: &SessionRecord) -> MedusaResult<()> {
    let directory = medusa_dir(repo).join("sessions");
    fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{}.json", record.id));
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(record)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn verify_chain(events: &[EventEnvelope]) -> MedusaResult<()> {
    let mut previous = None;
    for event in events {
        event.verify(previous.as_deref())?;
        previous = Some(event.checksum.clone());
    }
    Ok(())
}

fn tool_error(message: &str) -> medusa_core::MedusaError {
    let digest = hex::encode(Sha256::digest(message.as_bytes()));
    medusa_core::MedusaError::new(
        medusa_core::ErrorCode::ToolExecutionFailed,
        medusa_core::ErrorCategory::Execution,
        format!("{message} ({digest})"),
    )
}
