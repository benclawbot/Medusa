use std::{fs, path::{Path, PathBuf}};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use medusa_protocol::EventEnvelope;
use medusa_provider::Message;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::evidence::verify_chain;

/// Durable state for one single-agent session.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentSession {
    pub id: SessionId,
    pub objective: String,
    pub repo: PathBuf,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub completed: bool,
    pub turn: u32,
    pub messages: Vec<Message>,
    pub events: Vec<EventEnvelope>,
    pub evidence: Vec<String>,
}

/// Creates the on-disk Medusa layout and repository map.
pub fn bootstrap(repo: &Path) -> MedusaResult<()> {
    fs::create_dir_all(repo.join(".medusa/sessions"))?;
    let map = repo.join("REPOSITORY_MAP.md");
    if !map.exists() {
        fs::write(
            map,
            "# Repository Map\n\n## Overview\n\n## Languages and Frameworks\n\n## Entry Points\n\n## Build and Run Commands\n\n## Test Commands\n\n## Critical Invariants\n",
        )?;
    }
    Ok(())
}

pub(crate) fn load(repo: &Path, session: &str) -> MedusaResult<AgentSession> {
    let id = SessionId::parse(session).map_err(|message| {
        MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            message,
        )
    })?;
    let session: AgentSession = serde_json::from_slice(&fs::read(session_path(repo, &id))?)?;
    verify_chain(&session.events)?;
    Ok(session)
}

pub(crate) fn persist(session: &AgentSession) -> MedusaResult<()> {
    let path = session_path(&session.repo, &session.id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(session)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn session_path(repo: &Path, id: &SessionId) -> PathBuf {
    repo.join(".medusa/sessions").join(format!("{id}.json"))
}
