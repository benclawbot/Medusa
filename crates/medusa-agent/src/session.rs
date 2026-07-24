use std::{
    fs,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use medusa_protocol::EventEnvelope;
use medusa_provider::Message;
use medusa_world_model::WorldModelRef;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    approval::{ApprovalGrant, ApprovalReceipt, RollbackReceipt},
    evidence::verify_chain,
};

mod lessons;
mod recall;
mod skill_drafts;
mod skill_outcomes;
mod skill_probation;
#[path = "usage.rs"]
mod usage;

pub(crate) use usage::record_turn_usage;
#[allow(unused_imports)]
pub use usage::{SessionUsage, TurnUsage, UsageProvenance, session_usage};

/// A durable model-authored task plan step.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentPlanStep {
    pub title: String,
    pub status: AgentPlanStepStatus,
}

/// The current execution state of a task plan step.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPlanStepStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// One selectable option inside a model-authored question.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentQuestionOption {
    pub label: String,
    pub description: String,
}

/// One question inside a model-authored question set.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentQuestionItem {
    pub header: String,
    pub question: String,
    pub options: Vec<AgentQuestionOption>,
    pub multi_select: bool,
}

/// A model-authored question set that blocks the session until the user confirms every answer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentQuestion {
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub questions: Vec<AgentQuestionItem>,
    #[serde(default, rename = "question", skip_serializing)]
    pub(crate) legacy_question: Option<String>,
    #[serde(default, rename = "options", skip_serializing)]
    pub(crate) legacy_options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) approval: Option<PendingToolApproval>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct PendingToolApproval {
    pub tool_use_id: String,
    pub tool: String,
    pub input: serde_json::Value,
    pub grant: ApprovalGrant,
}

impl AgentQuestion {
    #[must_use]
    pub fn prompts(&self) -> Vec<AgentQuestionItem> {
        if !self.questions.is_empty() {
            return self.questions.clone();
        }
        self.legacy_question
            .as_deref()
            .filter(|question| !question.trim().is_empty())
            .map(|question| AgentQuestionItem {
                header: "Question".to_owned(),
                question: question.to_owned(),
                options: self
                    .legacy_options
                    .iter()
                    .map(|label| AgentQuestionOption {
                        label: label.clone(),
                        description: String::new(),
                    })
                    .collect(),
                multi_select: false,
            })
            .into_iter()
            .collect()
    }
}

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
    #[serde(default)]
    pub plan: Vec<AgentPlanStep>,
    #[serde(default)]
    pub pending_question: Option<AgentQuestion>,
    pub messages: Vec<Message>,
    pub events: Vec<EventEnvelope>,
    pub evidence: Vec<String>,
    #[serde(default)]
    pub tool_artifacts: Vec<PathBuf>,
    #[serde(default)]
    pub world_model: Option<WorldModelRef>,
    #[serde(default)]
    pub approval_grants: Vec<ApprovalGrant>,
    #[serde(default)]
    pub approval_receipts: Vec<ApprovalReceipt>,
    #[serde(default)]
    pub rollback_receipts: Vec<RollbackReceipt>,
}

/// Creates the on-disk Medusa layout and repository map.
pub fn bootstrap(repo: &Path) -> MedusaResult<()> {
    if fs::create_dir_all(repo.join(".medusa/sessions")).is_err() {
        fs::create_dir_all(fallback_session_root(repo))?;
    }
    let _ = fs::create_dir_all(repo.join(".medusa/world-models"));
    let map = repo.join("REPOSITORY_MAP.md");
    if !map.exists() {
        let _ = fs::write(
            map,
            "# Repository Map\n\n## Overview\n\n## Languages and Frameworks\n\n## Entry Points\n\n## Build and Run Commands\n\n## Test Commands\n\n## Critical Invariants\n",
        );
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
    let primary = session_path(repo, &id);
    let path = if primary.is_file() {
        primary
    } else {
        fallback_session_path(repo, &id)
    };
    let session: AgentSession = serde_json::from_slice(&fs::read(path)?)?;
    verify_chain(&session.events)?;
    Ok(session)
}

pub(crate) fn persist(session: &AgentSession) -> MedusaResult<()> {
    let primary = session_path(&session.repo, &session.id);
    let persisted = match persist_at(&primary, session) {
        Ok(()) => Ok(()),
        Err(_) => persist_at(&fallback_session_path(&session.repo, &session.id), session),
    };
    persisted?;
    let _recall_result = recall::persist_completed_session(session);
    if let Ok(Some(lesson_path)) = lessons::extract_completed_session(session) {
        let _skill_result = skill_drafts::create_from_lesson(&lesson_path);
    }
    let _skill_outcome_result = skill_outcomes::record_completed_session(session);
    let _skill_probation_result = skill_probation::refresh(&session.repo);
    Ok(())
}

fn persist_at(path: &Path, session: &AgentSession) -> MedusaResult<()> {
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

fn fallback_session_path(repo: &Path, id: &SessionId) -> PathBuf {
    fallback_session_root(repo).join(format!("{id}.json"))
}

fn fallback_session_root(repo: &Path) -> PathBuf {
    let root = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    root.join("Medusa/sessions").join(repository_key(repo))
}

fn repository_key(repo: &Path) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in repo.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}
