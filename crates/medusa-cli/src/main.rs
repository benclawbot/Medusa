use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use clap::{Parser, Subcommand};
use medusa_config::Config;
use medusa_core::{CorrelationId, ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
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
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
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
        CommandKind::Bootstrap => bootstrap(&repo),
        CommandKind::Search { pattern } => search(&repo, &pattern),
        CommandKind::Shell { program, args } => shell(&repo, &program, &args),
        CommandKind::Checkpoint { message } => checkpoint(&repo, &message),
        CommandKind::Run { objective } => run_session(&repo, objective, &config),
        CommandKind::Resume { session } => resume_session(&repo, &session, &config),
    }
}

fn medusa_dir(repo: &Path) -> PathBuf {
    repo.join(".medusa")
}

fn bootstrap(repo: &Path) -> MedusaResult<()> {
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
    if matches!(program, "rm" | "sudo" | "shutdown" | "reboot") {
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
    if !status.success() {
        return Err(tool_error(format!("command exited with {status}")));
    }
    Ok(())
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
        Err(tool_error(format!(
            "git {} failed with {status}",
            args.join(" ")
        )))
    }
}

fn run_session(repo: &Path, objective: String, _config: &Config) -> MedusaResult<()> {
    bootstrap(repo)?;
    let now = OffsetDateTime::now_utc();
    let session_id = SessionId::new();
    let event = EventEnvelope::new(
        1,
        session_id.clone(),
        Actor::User,
        CorrelationId::new(),
        EventPayload::SessionCreated {
            objective: objective.clone(),
        },
        None,
        now,
    )?;
    let record = SessionRecord {
        id: session_id,
        objective,
        repo: repo.to_path_buf(),
        created_at: now,
        updated_at: now,
        completed: false,
        events: vec![event],
    };
    persist_session(repo, &record)?;
    println!("session {} created", record.id);
    Ok(())
}

fn resume_session(repo: &Path, session: &str, _config: &Config) -> MedusaResult<()> {
    let session_id = SessionId::parse(session).map_err(|message| {
        MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            message,
        )
    })?;
    let path = medusa_dir(repo)
        .join("sessions")
        .join(format!("{session_id}.json"));
    let mut record: SessionRecord = serde_json::from_slice(&fs::read(&path)?)?;
    verify_chain(&record.events)?;
    record.updated_at = OffsetDateTime::now_utc();
    persist_session(repo, &record)?;
    println!("session {} resumed: {}", record.id, record.objective);
    Ok(())
}

fn persist_session(repo: &Path, record: &SessionRecord) -> MedusaResult<()> {
    let directory = medusa_dir(repo).join("sessions");
    fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{}.json", record.id));
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(record)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn verify_chain(events: &[EventEnvelope]) -> MedusaResult<()> {
    let mut previous: Option<&str> = None;
    for event in events {
        event.validate()?;
        if event.previous_hash.as_deref() != previous {
            return Err(MedusaError::new(
                ErrorCode::ChecksumMismatch,
                ErrorCategory::Persistence,
                "event chain previous hash mismatch",
            ));
        }
        previous = Some(&event.checksum);
    }
    Ok(())
}

fn tool_error(message: String) -> MedusaError {
    let digest = hex::encode(Sha256::digest(message.as_bytes()));
    MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        format!("{message} ({digest})"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_survives_restart() {
        let directory = tempfile::tempdir().expect("tempdir");
        run_session(directory.path(), "fix fixture".into(), &Config::default())
            .expect("create session");
        let sessions = fs::read_dir(medusa_dir(directory.path()).join("sessions"))
            .expect("sessions")
            .collect::<Result<Vec<_>, _>>()
            .expect("entries");
        assert_eq!(sessions.len(), 1);
        let name = sessions[0]
            .path()
            .file_stem()
            .expect("stem")
            .to_string_lossy()
            .into_owned();
        resume_session(directory.path(), &name, &Config::default()).expect("resume session");
    }
}
