use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    branch_summary::BranchSummaryRecord,
    session::{self, AgentSession, NonFatalDiagnostic, journal},
};

const DISPOSITION_SCHEMA_VERSION: u32 = 1;
const JOURNAL_TOMBSTONE_PREFIX: &str = "MEDUSA_SESSION_DISPOSED_V1\n";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DispositionMarker {
    schema_version: u32,
    session_id: String,
    #[serde(with = "time::serde::rfc3339")]
    disposed_at: OffsetDateTime,
}

/// Disposes a completed session and its agent-owned derived state.
///
/// Active sessions are rejected. The canonical journal is replaced with a fail-closed tombstone
/// while holding the same in-process journal lock used by normal persistence. That tombstone keeps
/// stale `AgentSession` writers from recreating deleted content. Cleanup is idempotent so a crash
/// after tombstoning can be retried without resurrecting the session.
pub fn dispose_completed_session(repo: &Path, session_id: &str) -> MedusaResult<()> {
    let id = SessionId::parse(session_id).map_err(validation_error)?;
    if journal_is_tombstoned(repo, &id)? {
        validate_marker(repo, &id)?;
        return purge_agent_owned_state(repo, &id);
    }

    let session = session::load(repo, id.as_str())?;
    if !session.completed {
        return Err(validation_error(
            "session disposition requires a completed session",
        ));
    }

    journal::commit_snapshot_with(&session, |committed| {
        if !committed.completed {
            return Err(validation_error(
                "session became non-completed before disposition",
            ));
        }
        preflight_agent_owned_state(committed)?;
        write_marker(repo, &id)?;
        write_journal_tombstone(repo, &id)?;
        purge_agent_owned_state(repo, &id)
    })?;
    Ok(())
}

/// Returns true only after both the durable disposition marker and fail-closed journal tombstone
/// exist. A marker alone can represent an interrupted deletion and is not reported as complete.
pub fn is_session_disposed(repo: &Path, session_id: &SessionId) -> bool {
    marker_path(repo, session_id).is_file()
        && journal_is_tombstoned(repo, session_id).unwrap_or(false)
}

fn preflight_agent_owned_state(session: &AgentSession) -> MedusaResult<()> {
    if let Some(reference) = &session.world_model {
        validate_relative_path(&reference.relative_path)?;
    }
    // A corrupt branch-summary record cannot be classified safely. Fail before tombstoning rather
    // than claiming deletion while an unclassified content-addressed artifact remains.
    for path in branch_summary_paths(&session.repo)? {
        let record: BranchSummaryRecord = serde_json::from_slice(&fs::read(&path)?)?;
        record.verify()?;
    }
    Ok(())
}

fn purge_agent_owned_state(repo: &Path, id: &SessionId) -> MedusaResult<()> {
    // Keep the primary tombstone. The fallback journal is a recoverable materialized copy and must
    // disappear so it cannot be selected by tooling that bypasses normal journal resolution.
    remove_file_if_present(&fallback_journal_path(repo, id))?;
    remove_file_if_present(&primary_snapshot_path(repo, id))?;
    remove_file_if_present(&fallback_snapshot_path(repo, id))?;
    remove_file_if_present(&repo.join(".medusa/escalations").join(format!("{id}.json")))?;

    medusa_memory::delete_session_recall(repo, id.as_str())?;
    purge_world_model(repo, id)?;
    purge_branch_summaries(repo, id)?;
    purge_nonfatal_diagnostics(repo, id)?;
    Ok(())
}

fn purge_world_model(repo: &Path, id: &SessionId) -> MedusaResult<()> {
    let relative = medusa_world_model::model_relative_path(id.as_str());
    validate_relative_path(&relative)?;
    let directory = repo.join(relative).parent().map(Path::to_path_buf).ok_or_else(|| {
        persistence_error("world-model path does not have a session directory")
    })?;
    remove_dir_if_present(&directory)
}

fn purge_branch_summaries(repo: &Path, id: &SessionId) -> MedusaResult<()> {
    for path in branch_summary_paths(repo)? {
        let record: BranchSummaryRecord = serde_json::from_slice(&fs::read(&path)?)?;
        record.verify()?;
        if record.session_id == id.as_str() {
            remove_file_if_present(&path)?;
        }
    }
    Ok(())
}

fn branch_summary_paths(repo: &Path) -> MedusaResult<Vec<PathBuf>> {
    let directory = repo.join(".medusa/artifacts/branch-summary-v1");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry.path().extension().and_then(|value| value.to_str()) == Some("json")
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn purge_nonfatal_diagnostics(repo: &Path, id: &SessionId) -> MedusaResult<()> {
    let path = repo.join(".medusa/diagnostics/nonfatal.json");
    let raw = match fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut diagnostics: Vec<NonFatalDiagnostic> = serde_json::from_slice(&raw)?;
    let before = diagnostics.len();
    diagnostics.retain(|item| item.session_id.as_deref() != Some(id.as_str()));
    if diagnostics.len() == before {
        return Ok(());
    }
    atomic_write_json(&path, &diagnostics)
}

fn write_marker(repo: &Path, id: &SessionId) -> MedusaResult<()> {
    let marker = DispositionMarker {
        schema_version: DISPOSITION_SCHEMA_VERSION,
        session_id: id.to_string(),
        disposed_at: OffsetDateTime::now_utc(),
    };
    atomic_write_json(&marker_path(repo, id), &marker)
}

fn validate_marker(repo: &Path, id: &SessionId) -> MedusaResult<()> {
    let path = marker_path(repo, id);
    let marker: DispositionMarker = serde_json::from_slice(&fs::read(path)?)?;
    if marker.schema_version != DISPOSITION_SCHEMA_VERSION || marker.session_id != id.as_str() {
        return Err(persistence_error(
            "session disposition marker identity is invalid",
        ));
    }
    Ok(())
}

fn write_journal_tombstone(repo: &Path, id: &SessionId) -> MedusaResult<()> {
    let path = primary_journal_path(repo, id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Deliberately not a valid journal header. Existing journal readers fail closed, and because
    // the path remains a regular file `ensure_initialized` cannot silently create a new journal.
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)?;
    file.write_all(format!("{JOURNAL_TOMBSTONE_PREFIX}{id}\n").as_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn journal_is_tombstoned(repo: &Path, id: &SessionId) -> MedusaResult<bool> {
    let path = primary_journal_path(repo, id);
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let prefix = format!("{JOURNAL_TOMBSTONE_PREFIX}{id}\n");
    Ok(raw.starts_with(prefix.as_bytes()))
}

fn marker_path(repo: &Path, id: &SessionId) -> PathBuf {
    repo.join(".medusa/session-dispositions")
        .join(format!("{id}.json"))
}

fn primary_journal_path(repo: &Path, id: &SessionId) -> PathBuf {
    repo.join(".medusa/journals").join(format!("{id}.events"))
}

fn fallback_journal_path(repo: &Path, id: &SessionId) -> PathBuf {
    session::fallback_storage_root(repo, "journals").join(format!("{id}.events"))
}

fn primary_snapshot_path(repo: &Path, id: &SessionId) -> PathBuf {
    repo.join(".medusa/sessions").join(format!("{id}.json"))
}

fn fallback_snapshot_path(repo: &Path, id: &SessionId) -> PathBuf {
    session::fallback_storage_root(repo, "sessions").join(format!("{id}.json"))
}

fn validate_relative_path(path: &Path) -> MedusaResult<()> {
    if path.is_absolute()
        || path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(persistence_error(
            "session-owned relative path escapes its repository scope",
        ));
    }
    Ok(())
}

fn remove_file_if_present(path: &Path) -> MedusaResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn remove_dir_if_present(path: &Path) -> MedusaResult<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> MedusaResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let result = (|| -> MedusaResult<()> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(value)?)?;
        file.sync_all()?;
        #[cfg(windows)]
        if path.exists() {
            fs::remove_file(path)?;
        }
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn validation_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn persistence_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Persistence,
        message,
    )
}

#[cfg(test)]
mod tests {
    use medusa_config::Config;
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};

    use super::*;
    use crate::{AgentEngine, persist_session, session_browser::list_sessions};

    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("session creation does not call the provider")
        }
    }

    #[test]
    fn active_session_cannot_be_disposed() {
        let repository = tempfile::tempdir().expect("repository");
        let engine = AgentEngine::new(UnusedProvider, Config::default());
        let session = engine
            .create_session(repository.path(), "active private objective".to_owned())
            .expect("session");

        assert!(dispose_completed_session(repository.path(), session.id.as_str()).is_err());
        assert!(!is_session_disposed(repository.path(), &session.id));
        assert!(session::load(repository.path(), session.id.as_str()).is_ok());
    }

    #[test]
    fn completed_disposition_blocks_stale_persistence_and_removes_agent_copies() {
        let repository = tempfile::tempdir().expect("repository");
        let engine = AgentEngine::new(UnusedProvider, Config::default());
        let mut session = engine
            .create_session(repository.path(), "PRIVATE_SESSION_DELETE_MARKER".to_owned())
            .expect("session");
        session.completed = true;
        persist_session(&session).expect("persist completed session");
        let stale = session.clone();
        let id = session.id.clone();

        dispose_completed_session(repository.path(), id.as_str()).expect("dispose");
        assert!(is_session_disposed(repository.path(), &id));
        assert!(!primary_snapshot_path(repository.path(), &id).exists());
        assert!(!fallback_snapshot_path(repository.path(), &id).exists());
        assert!(!fallback_journal_path(repository.path(), &id).exists());
        assert!(session::load(repository.path(), id.as_str()).is_err());
        assert!(persist_session(&stale).is_err());
        assert!(list_sessions(repository.path()).is_ok());

        dispose_completed_session(repository.path(), id.as_str()).expect("idempotent retry");
    }
}
