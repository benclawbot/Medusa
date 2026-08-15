use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
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

mod browser_assisted_escalation;
mod completed_learning;
mod escalation_state;
#[path = "journal.rs"]
pub(crate) mod journal;
mod lessons;
mod manual_escalation;
mod recall;
mod skill_drafts;
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
pub(crate) use usage::record_turn_usage;
pub use usage::{SessionUsage, TurnUsage, UsageProvenance, session_usage};

pub(crate) fn record_loaded_skills(session: &AgentSession) -> MedusaResult<()> {
    if !completed_learning::telemetry_allowed(&session.repo)? {
        return Ok(());
    }
    skill_outcomes::record_loaded_skills(session)
}

pub(crate) fn record_terminal_skill_outcome(
    session: &AgentSession,
    error: &MedusaError,
    decision: &medusa_failure::FailureDecision,
    reason: &str,
) -> MedusaResult<Option<PathBuf>> {
    if !completed_learning::telemetry_allowed(&session.repo)? {
        return Ok(None);
    }
    skill_outcomes::record_terminal_skill_outcome(session, error, decision, reason)
}

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
static SNAPSHOT_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
    let snapshot = if path.is_file() {
        let session: AgentSession = serde_json::from_slice(&fs::read(&path)?)?;
        verify_chain(&session.events)?;
        Some(session)
    } else {
        None
    };
    let outcome = journal::load_or_migrate(repo, &id, snapshot)?;
    if outcome.repair_snapshot {
        persist_compatibility_snapshot(&outcome.session)?;
    }
    Ok(outcome.session)
}

pub(crate) fn persist(session: &AgentSession) -> MedusaResult<()> {
    let committed = journal::commit_snapshot_with(session, persist_compatibility_snapshot)?;
    if committed.events.last().is_some_and(|event| {
        matches!(
            &event.payload,
            medusa_protocol::EventPayload::ModelRequestStarted { .. }
        )
    }) {
        if let Err(error) = record_loaded_skills(&committed) {
            record_nonfatal(
                &session.repo,
                Some(&session.id),
                "learning",
                "record_loaded_skills",
                &error.to_string(),
            );
        }
    }
    if let Err(error) = recall::persist_completed_session(&committed) {
        record_nonfatal(
            &session.repo,
            Some(&session.id),
            "memory",
            "persist_completed_session",
            &error.to_string(),
        );
    }
    if let Err(error) = completed_learning::process(&committed) {
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

fn persist_compatibility_snapshot(session: &AgentSession) -> MedusaResult<()> {
    let primary = session_path(&session.repo, &session.id);
    match persist_at(&primary, session) {
        Ok(()) => Ok(()),
        Err(_) => persist_at(&fallback_session_path(&session.repo, &session.id), session),
    }
}

fn persist_at(path: &Path, session: &AgentSession) -> MedusaResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = unique_snapshot_temporary(path);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(session)?)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        sync_parent(path);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn unique_snapshot_temporary(path: &Path) -> PathBuf {
    let sequence = SNAPSHOT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .map_or_else(|| "session.json".into(), |name| name.to_string_lossy());
    path.with_file_name(format!(".{file_name}.tmp.{}.{}", process::id(), sequence))
}

fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
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
    root.join("Medusa")
        .join(category)
        .join(repository_key(repo))
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

    fn concurrent_test_session(repo: &Path) -> AgentSession {
        AgentSession {
            id: SessionId::new(),
            objective: "concurrent persistence test".to_owned(),
            repo: repo.to_path_buf(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            completed: false,
            turn: 0,
            plan: Vec::new(),
            pending_question: None,
            messages: Vec::new(),
            events: Vec::new(),
            evidence: Vec::new(),
            tool_artifacts: Vec::new(),
            world_model: None,
            approval_grants: Vec::new(),
            approval_receipts: Vec::new(),
            rollback_receipts: Vec::new(),
        }
    }

    #[test]
    fn concurrent_full_persists_share_one_atomic_publication_boundary() {
        use std::{
            sync::{Arc, Barrier},
            thread,
        };

        use medusa_protocol::{Actor, EventPayload};

        let repository = tempfile::tempdir().expect("repository");
        bootstrap(repository.path()).expect("bootstrap");
        let mut session = concurrent_test_session(repository.path());
        let objective = session.objective.clone();
        journal::append_payload_committed(
            &mut session,
            Actor::Coordinator,
            EventPayload::SessionCreated { objective },
        )
        .expect("initial durable event");
        persist(&session).expect("initial full persist");

        let workers = 8;
        let iterations = 24;
        let barrier = Arc::new(Barrier::new(workers));
        let session = Arc::new(session);
        let handles = (0..workers)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let session = Arc::clone(&session);
                thread::spawn(move || {
                    for _ in 0..iterations {
                        barrier.wait();
                        persist(&session).expect("concurrent full persist");
                    }
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().expect("persistence worker");
        }

        let loaded =
            load(repository.path(), &session.id.to_string()).expect("load committed session");
        assert_eq!(loaded.events, session.events);
        let temporary_files = fs::read_dir(repository.path().join(".medusa/sessions"))
            .expect("session directory")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
            .count();
        assert_eq!(temporary_files, 0);
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
