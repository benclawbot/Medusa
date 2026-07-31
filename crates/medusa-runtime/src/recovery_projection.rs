//! Recovery-coordinator projection for verified runtime checkpoints.
//!
//! This module keeps `.medusa/recovery` as a disposable, rebuildable view over the canonical
//! session journal and verified checkpoint artifacts. It never owns execution state.

#[cfg(unix)]
use std::fs::File;

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use medusa_agent::session_browser::replay_events;
use medusa_recovery_coordinator::{
    CheckpointPresentation, RecoveryPreview, RecoveryView, VerificationState,
};
use serde::Serialize;

use crate::{
    RuntimeController, RuntimeError, checkpoint_payload, checkpoint_store, execution_history,
};

const RECOVERY_DIRECTORY: &str = ".medusa/recovery";

#[derive(Debug, Serialize)]
struct PersistedRecoveryRecord {
    session_id: String,
    last_durable_step: String,
    interrupted_operation: Option<String>,
    current_repository_fingerprint: String,
    verification: VerificationState,
    approvals_must_be_reestablished: bool,
    containment_must_be_reestablished: bool,
    checkpoints: Vec<CheckpointPresentation>,
    selected_preview: Option<RecoveryPreview>,
}

impl RuntimeController {
    /// Returns the current recovery views derived from verified runtime state.
    #[must_use]
    pub fn recovery_views(&self) -> Vec<RecoveryView> {
        crate::recovery::discover(&self.repo)
    }

    /// Generates and persists an authoritative preview for a selected checkpoint restore.
    pub fn preview_checkpoint_restore(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<RecoveryPreview, RuntimeError> {
        select_preview(&self.repo, session_id, checkpoint_id)
    }
}

pub(crate) fn refresh(repo: &Path, session_id: &str) -> Result<(), RuntimeError> {
    let destination = recovery_path(repo, session_id);
    match build_record(repo, session_id, None)? {
        Some(record) => persist_projection(&destination, &record),
        None => remove_projection(&destination),
    }
}

pub(crate) fn select_preview(
    repo: &Path,
    session_id: &str,
    checkpoint_id: &str,
) -> Result<RecoveryPreview, RuntimeError> {
    let destination = recovery_path(repo, session_id);
    let record = build_record(repo, session_id, Some(checkpoint_id))?
        .ok_or_else(|| RuntimeError::agent("completed sessions do not have restore previews"))?;
    let preview = record
        .selected_preview
        .clone()
        .ok_or_else(|| RuntimeError::agent("selected checkpoint preview was not generated"))?;
    persist_projection(&destination, &record)?;
    Ok(preview)
}

fn build_record(
    repo: &Path,
    session_id: &str,
    selected_checkpoint: Option<&str>,
) -> Result<Option<PersistedRecoveryRecord>, RuntimeError> {
    let checkpoints = checkpoint_store::list(repo, session_id)?;
    if checkpoints.is_empty() {
        return Ok(None);
    }

    let journal = replay_events(repo, session_id, 0).map_err(RuntimeError::agent)?;
    let mut presentations = Vec::with_capacity(checkpoints.len());
    let mut latest_step = String::new();
    let mut latest_verification = VerificationState::Incomplete;

    for record in &checkpoints {
        let historical = execution_history::historical(repo, session_id, record.journal_cursor)?;
        let task_step = historical
            .values
            .get("last_event_kind")
            .cloned()
            .unwrap_or_else(|| "unknown_durable_boundary".to_owned());
        let verification = verification_state(&historical.values);
        let created_at_unix_ms = journal
            .get(
                usize::try_from(record.journal_cursor.saturating_sub(1))
                    .map_err(RuntimeError::agent)?,
            )
            .and_then(|event| {
                i64::try_from(event.timestamp.unix_timestamp_nanos() / 1_000_000).ok()
            })
            .unwrap_or_default();
        presentations.push(CheckpointPresentation {
            id: record.checkpoint.fingerprint.clone(),
            sequence: record.journal_cursor,
            created_at_unix_ms,
            task_step: task_step.clone(),
            reason: format!("durable {task_step} boundary"),
            repository_fingerprint: record.checkpoint.repository_snapshot_fingerprint.clone(),
            verification,
            provenance: format!("runtime-checkpoint/v1:{}", record.record_fingerprint),
            integrity_verified: true,
        });
        latest_step = task_step;
        latest_verification = verification;
    }

    if matches!(
        latest_step.as_str(),
        "session_completed" | "cancellation_completed"
    ) {
        return Ok(None);
    }

    let latest = checkpoints
        .last()
        .ok_or_else(|| RuntimeError::agent("verified checkpoint list became empty"))?;
    let selected_id = selected_checkpoint.unwrap_or(&latest.checkpoint.fingerprint);
    if !checkpoints
        .iter()
        .any(|record| record.checkpoint.fingerprint == selected_id)
    {
        return Err(RuntimeError::InvalidCommand(format!(
            "checkpoint {selected_id} does not belong to session {session_id}"
        )));
    }
    let selected_payload = checkpoint_payload::load(repo, session_id, selected_id)?;
    let selected_preview = checkpoint_payload::preview(repo, &selected_payload)?;
    let latest_payload =
        checkpoint_payload::load(repo, session_id, &latest.checkpoint.fingerprint)?;
    let current_repository_fingerprint =
        checkpoint_payload::current_repository_fingerprint(repo, &latest_payload)?;
    let interrupted_operation = matches!(latest_step.as_str(), "runtime_failed" | "session_failed")
        .then(|| latest_step.replace('_', " "));

    Ok(Some(PersistedRecoveryRecord {
        session_id: session_id.to_owned(),
        last_durable_step: latest_step,
        interrupted_operation,
        current_repository_fingerprint,
        verification: latest_verification,
        approvals_must_be_reestablished: true,
        containment_must_be_reestablished: true,
        checkpoints: presentations,
        selected_preview: Some(selected_preview),
    }))
}

fn verification_state(values: &std::collections::BTreeMap<String, String>) -> VerificationState {
    match values.get("verification_passed").map(String::as_str) {
        Some("true") => VerificationState::Verified,
        Some("false") => VerificationState::Failed,
        Some(_) => VerificationState::Unknown,
        None => VerificationState::Incomplete,
    }
}

fn recovery_path(repo: &Path, session_id: &str) -> PathBuf {
    repo.join(RECOVERY_DIRECTORY)
        .join(format!("{session_id}.json"))
}

fn persist_projection(
    destination: &Path,
    record: &PersistedRecoveryRecord,
) -> Result<(), RuntimeError> {
    let bytes = serde_json::to_vec_pretty(record).map_err(RuntimeError::agent)?;
    if destination.is_file() && fs::read(destination).map_err(RuntimeError::agent)? == bytes {
        return Ok(());
    }
    let directory = destination
        .parent()
        .ok_or_else(|| RuntimeError::agent("recovery projection path has no parent"))?;
    fs::create_dir_all(directory).map_err(RuntimeError::agent)?;
    let temporary = directory.join(format!(
        ".{}.{}.{}.tmp",
        destination
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("recovery"),
        std::process::id(),
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(RuntimeError::agent)?;
    if let Err(error) = (|| -> std::io::Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, destination)?;
        sync_parent(directory)?;
        Ok(())
    })() {
        let _ = fs::remove_file(&temporary);
        return Err(RuntimeError::agent(error));
    }
    Ok(())
}

fn remove_projection(path: &Path) -> Result<(), RuntimeError> {
    if !path.exists() {
        return Ok(());
    }
    fs::remove_file(path).map_err(RuntimeError::agent)?;
    if let Some(parent) = path.parent() {
        sync_parent(parent).map_err(RuntimeError::agent)?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent(directory: &Path) -> std::io::Result<()> {
    File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use medusa_agent::{AgentEngine, record_session_event};
    use medusa_config::Config;
    use medusa_core::MedusaResult;
    use medusa_protocol::{Actor, EventPayload};
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};
    use tempfile::tempdir;

    use super::*;

    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("session creation does not call the provider")
        }
    }

    fn session(repo: &Path) -> medusa_agent::AgentSession {
        AgentEngine::new(UnusedProvider, Config::default())
            .create_session(repo, "Recover durable work".to_owned())
            .expect("session")
    }

    fn record_file_boundary(repo: &Path, session: &mut medusa_agent::AgentSession) {
        fs::create_dir_all(repo.join("src")).expect("src");
        fs::write(repo.join("src/lib.rs"), "checkpoint").expect("file");
        record_session_event(
            session,
            Actor::Coordinator,
            EventPayload::FileTransactionCommitted {
                paths: vec!["src/lib.rs".to_owned()],
                rollback_ref: "rollback".to_owned(),
            },
        )
        .expect("file event");
    }

    #[test]
    fn durable_turn_creates_verified_recovery_view() {
        let repository = tempdir().expect("repository");
        let session = session(repository.path());
        crate::record_controller_event(
            repository.path(),
            session.id.as_str(),
            Actor::Coordinator,
            EventPayload::RuntimeTurnFinished,
        )
        .expect("turn boundary");

        let views = crate::recovery::discover(repository.path());
        assert_eq!(views.len(), 1);
        let view = &views[0];
        assert_eq!(view.session_id, session.id.as_str());
        assert_eq!(view.last_durable_step, "runtime_turn_finished");
        assert_eq!(view.checkpoints.len(), 1);
        assert!(view.checkpoints[0].integrity_verified);
        assert!(view.selected_preview.is_some());
        assert!(view.approvals_must_be_reestablished);
        assert!(view.containment_must_be_reestablished);
    }

    #[test]
    fn selected_checkpoint_preview_reports_exact_changes() {
        let repository = tempdir().expect("repository");
        let mut session = session(repository.path());
        record_file_boundary(repository.path(), &mut session);
        crate::record_controller_event(
            repository.path(),
            session.id.as_str(),
            Actor::Coordinator,
            EventPayload::RuntimeTurnFinished,
        )
        .expect("turn boundary");
        let checkpoint = checkpoint_store::latest(repository.path(), session.id.as_str())
            .expect("latest")
            .expect("checkpoint");
        fs::write(repository.path().join("src/lib.rs"), "changed").expect("change");

        let preview = select_preview(
            repository.path(),
            session.id.as_str(),
            &checkpoint.checkpoint.fingerprint,
        )
        .expect("preview");
        assert_eq!(preview.files.len(), 1);
        assert_eq!(preview.files[0].path, "src/lib.rs");
        assert!(preview.files[0].would_overwrite_uncommitted_work);
        assert!(!preview.repository_matches_checkpoint_base);
    }

    #[test]
    fn clean_completion_removes_recovery_projection() {
        let repository = tempdir().expect("repository");
        let session = session(repository.path());
        crate::record_controller_event(
            repository.path(),
            session.id.as_str(),
            Actor::Coordinator,
            EventPayload::RuntimeTurnFinished,
        )
        .expect("turn boundary");
        assert_eq!(crate::recovery::discover(repository.path()).len(), 1);

        crate::record_controller_event(
            repository.path(),
            session.id.as_str(),
            Actor::Coordinator,
            EventPayload::SessionCompleted {
                report_ref: "report".to_owned(),
            },
        )
        .expect("completion");
        assert!(crate::recovery::discover(repository.path()).is_empty());
    }

    #[test]
    fn corrupt_checkpoint_does_not_overwrite_last_valid_projection() {
        let repository = tempdir().expect("repository");
        let session = session(repository.path());
        crate::record_controller_event(
            repository.path(),
            session.id.as_str(),
            Actor::Coordinator,
            EventPayload::RuntimeTurnFinished,
        )
        .expect("turn boundary");
        let path = recovery_path(repository.path(), session.id.as_str());
        let original = fs::read(&path).expect("projection");
        let checkpoint = checkpoint_store::latest(repository.path(), session.id.as_str())
            .expect("latest")
            .expect("checkpoint");
        let checkpoint_path = repository
            .path()
            .join(".medusa/checkpoints")
            .join(session.id.as_str())
            .join(format!("{}.json", checkpoint.checkpoint.fingerprint));
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&checkpoint_path).expect("checkpoint artifact"))
                .expect("checkpoint json");
        value["journal_cursor"] = serde_json::json!(99);
        fs::write(
            checkpoint_path,
            serde_json::to_vec_pretty(&value).expect("tampered json"),
        )
        .expect("tamper checkpoint");

        assert!(refresh(repository.path(), session.id.as_str()).is_err());
        assert_eq!(fs::read(path).expect("projection"), original);
    }

    #[test]
    fn refresh_is_byte_stable_without_new_durable_state() {
        let repository = tempdir().expect("repository");
        let session = session(repository.path());
        crate::record_controller_event(
            repository.path(),
            session.id.as_str(),
            Actor::Coordinator,
            EventPayload::RuntimeTurnFinished,
        )
        .expect("turn boundary");
        let path = recovery_path(repository.path(), session.id.as_str());
        let before = fs::read(&path).expect("projection");
        refresh(repository.path(), session.id.as_str()).expect("refresh");
        assert_eq!(fs::read(path).expect("projection"), before);
    }
}
