use std::{
    collections::BTreeMap,
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
    journal,
};

mod browser_assisted_escalation;
mod completed_learning;
mod escalation_state;
mod lessons;
mod manual_escalation;
mod recall;
mod skill_drafts;
#[allow(dead_code)]
mod skill_outcomes;
mod skill_probation;
#[path = "usage.rs"]
mod usage;

pub use browser_assisted_escalation::{
    BrowserAssistedLaunch, launch_browser_assisted_escalation, render_chatgpt_prompt,
};
pub use escalation_state::{
    EscalationJournal, EscalationStatus, SessionEscalation, load_escalation_journal,
    persist_escalation_journal,
};
pub use manual_escalation::{export_manual_escalation, import_manual_advice};
pub(crate) use skill_outcomes::{record_loaded_skills, record_terminal_skill_outcome};
pub(crate) use usage::record_turn_usage;
#[allow(unused_imports)]
pub use usage::{SessionUsage, TurnUsage, UsageProvenance, session_usage};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NonFatalDiagnostic {
    pub repository: String,
    pub session_id: Option<String>,
    pub stage: String,
    pub operation: String,
    pub error: String,
    pub occurrence_count: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen_at: OffsetDateTime,
}

const MAX_DIAGNOSTICS: usize = 128;

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
    #[serde(default)]
    pub applied_journal_cursor: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_journal_checksum: Option<String>,
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

/// Creates the on-disk Medusa runtime layout.
pub fn bootstrap(repo: &Path) -> MedusaResult<()> {
    if fs::create_dir_all(repo.join(".medusa/sessions")).is_err() {
        fs::create_dir_all(fallback_session_root(repo))?;
    }
    for (stage, operation, path) in [
        (
            "bootstrap",
            "create_world_model_directory",
            repo.join(".medusa/world-models"),
        ),
        (
            "bootstrap",
            "create_escalation_directory",
            repo.join(".medusa/escalations"),
        ),
    ] {
        if let Err(error) = fs::create_dir_all(&path) {
            record_nonfatal(repo, None, stage, operation, &error.to_string());
        }
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
    let mut session: AgentSession = serde_json::from_slice(&fs::read(&path)?)?;
    verify_chain(&session.events)?;
    let reconciliation = journal::reconcile(&mut session)?;
    if reconciliation.snapshot_changed {
        persist_at(&path, &session)?;
    }
    Ok(session)
}

pub(crate) fn persist(session: &AgentSession) -> MedusaResult<()> {
    journal::validate_snapshot_binding(session)?;
    let primary = session_path(&session.repo, &session.id);
    let persisted = match persist_at(&primary, session) {
        Ok(()) => Ok(()),
        Err(_) => persist_at(&fallback_session_path(&session.repo, &session.id), session),
    };
    persisted?;
    if session.events.last().is_some_and(|event| {
        matches!(
            &event.payload,
            medusa_protocol::EventPayload::ModelRequestStarted { .. }
        )
    }) {
        if let Err(error) = record_loaded_skills(session) {
            record_nonfatal(
                &session.repo,
                Some(&session.id),
                "learning",
                "record_loaded_skills",
                &error.to_string(),
            );
        }
    }
    if let Err(error) = recall::persist_completed_session(session) {
        record_nonfatal(
            &session.repo,
            Some(&session.id),
            "memory",
            "persist_completed_session",
            &error.to_string(),
        );
    }
    if let Err(error) = completed_learning::process(session) {
        record_nonfatal(
            &session.repo,
            Some(&session.id),
            "learning",
            "process_completed_session",
            &error.to_string(),
        );
    }
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
    fallback_storage_root(repo, "sessions")
}

pub(crate) fn fallback_storage_root(repo: &Path, category: &str) -> PathBuf {
    let root = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    root.join("Medusa").join(category).join(repository_key(repo))
}

fn repository_key(repo: &Path) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in repo.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

pub fn load_nonfatal_diagnostics(repo: &Path) -> Vec<NonFatalDiagnostic> {
    fs::read(diagnostic_path(repo))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn record_nonfatal(
    repo: &Path,
    session_id: Option<&SessionId>,
    stage: &str,
    operation: &str,
    error: &str,
) {
    let mut diagnostics = load_nonfatal_diagnostics(repo);
    let error = redact_error(error);
    let fingerprint = format!("{stage}\0{operation}\0{error}");
    let mut index = BTreeMap::new();
    for (position, item) in diagnostics.iter().enumerate() {
        index.insert(
            format!("{}\0{}\0{}", item.stage, item.operation, item.error),
            position,
        );
    }
    if let Some(position) = index.get(&fingerprint).copied() {
        let item = &mut diagnostics[position];
        item.occurrence_count = item.occurrence_count.saturating_add(1);
        item.last_seen_at = OffsetDateTime::now_utc();
    } else {
        diagnostics.push(NonFatalDiagnostic {
            repository: repo.display().to_string(),
            session_id: session_id.map(ToString::to_string),
            stage: stage.to_owned(),
            operation: operation.to_owned(),
            error,
            occurrence_count: 1,
            last_seen_at: OffsetDateTime::now_utc(),
        });
        if diagnostics.len() > MAX_DIAGNOSTICS {
            diagnostics.drain(..diagnostics.len() - MAX_DIAGNOSTICS);
        }
    }
    let path = diagnostic_path(repo);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(&diagnostics) {
        let temporary = path.with_extension("json.tmp");
        if fs::write(&temporary, bytes).is_ok() {
            let _ = fs::rename(temporary, path);
        }
    }
}

fn diagnostic_path(repo: &Path) -> PathBuf {
    repo.join(".medusa/diagnostics/nonfatal.json")
}

fn redact_error(error: &str) -> String {
    error
        .split_whitespace()
        .map(|token| {
            let lower = token.to_ascii_lowercase();
            if lower.contains("api_key")
                || lower.contains("apikey")
                || lower.contains("token=")
                || lower.starts_with("sk-")
            {
                "[REDACTED]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal_test_session(repo: &Path) -> AgentSession {
        let id = SessionId::new();
        let now = OffsetDateTime::UNIX_EPOCH;
        let event = medusa_protocol::EventEnvelope::new(
            1,
            id.clone(),
            medusa_protocol::Actor::Coordinator,
            medusa_core::CorrelationId::new(),
            medusa_protocol::EventPayload::SessionCreated {
                objective: "recover journal".to_owned(),
            },
            None,
            now,
        )
        .expect("event");
        AgentSession {
            id,
            objective: "recover journal".to_owned(),
            repo: repo.to_path_buf(),
            created_at: now,
            updated_at: now,
            completed: false,
            turn: 0,
            plan: Vec::new(),
            pending_question: None,
            messages: Vec::new(),
            events: vec![event],
            applied_journal_cursor: 0,
            applied_journal_checksum: None,
            evidence: Vec::new(),
            tool_artifacts: Vec::new(),
            world_model: None,
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
        }
    }

    #[test]
    fn legacy_snapshot_is_migrated_to_the_append_only_journal() {
        let repository = tempfile::tempdir().expect("repository");
        let session = journal_test_session(repository.path());
        let path = session_path(repository.path(), &session.id);
        persist_at(&path, &session).expect("legacy snapshot");

        let loaded = load(repository.path(), session.id.as_str()).expect("migrated session");

        assert_eq!(loaded.applied_journal_cursor, 1);
        assert_eq!(
            loaded.applied_journal_checksum,
            loaded.events.last().map(|event| event.checksum.clone())
        );
        assert!(
            repository
                .path()
                .join(".medusa/journals")
                .join(format!("{}.events", loaded.id))
                .is_file()
        );
    }

    #[test]
    fn journal_tail_is_replayed_when_snapshot_update_was_interrupted() {
        let repository = tempfile::tempdir().expect("repository");
        let mut session = journal_test_session(repository.path());
        crate::journal::reconcile(&mut session).expect("journal migration");
        let path = session_path(repository.path(), &session.id);
        persist_at(&path, &session).expect("snapshot");

        let tail = medusa_protocol::EventEnvelope::new(
            2,
            session.id.clone(),
            medusa_protocol::Actor::Coordinator,
            medusa_core::CorrelationId::new(),
            medusa_protocol::EventPayload::SessionResumed,
            session.events.last().map(|event| event.checksum.clone()),
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("tail event");
        assert_eq!(
            crate::journal::append_record(&session, &tail).expect("durable tail"),
            crate::journal::AppendDisposition::Appended
        );

        let loaded = load(repository.path(), session.id.as_str()).expect("replayed session");

        assert_eq!(loaded.events.len(), 2);
        assert_eq!(loaded.events[1], tail);
        assert_eq!(loaded.applied_journal_cursor, 2);
        let persisted: AgentSession =
            serde_json::from_slice(&fs::read(path).expect("materialized snapshot"))
                .expect("snapshot json");
        assert_eq!(persisted.applied_journal_cursor, 2);
        assert_eq!(persisted.events.len(), 2);
    }

    #[test]
    fn bootstrap_keeps_runtime_state_under_medusa_directory() {
        let repository = tempfile::tempdir().expect("repository");

        bootstrap(repository.path()).expect("bootstrap");

        assert!(repository.path().join(".medusa/sessions").is_dir());
        let mut top_level = fs::read_dir(repository.path())
            .expect("repository entries")
            .map(|entry| entry.expect("repository entry").file_name())
            .collect::<Vec<_>>();
        top_level.sort();
        assert_eq!(top_level, vec![std::ffi::OsString::from(".medusa")]);
    }
}
