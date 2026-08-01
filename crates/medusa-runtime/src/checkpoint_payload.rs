//! Repository payloads, previews, and transactional restore for runtime checkpoints.
//!
//! Payloads contain only repository-relative UTF-8 files that were named by authoritative
//! `FileTransactionCommitted` journal events. Unsupported files are recorded as explicit risks and
//! block restore. The journal and verified execution checkpoint remain the authority.

#[cfg(unix)]
use std::fs::File;

use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};

use medusa_agent::session_browser::replay_events;
use medusa_protocol::{EventEnvelope, EventPayload};
use medusa_recovery_coordinator::{FileChangeKind, RecoveryFileChange, RecoveryPreview};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{RuntimeCheckpointRecord, RuntimeError};

const PAYLOAD_SCHEMA_VERSION: u32 = 1;
const PAYLOAD_DIRECTORY: &str = ".medusa/recovery-checkpoints";
const RESTORE_TRANSACTION_DIRECTORY: &str = ".medusa/restore-transactions";
const MAX_CHECKPOINT_FILE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointFilePayload {
    pub path: String,
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsupported_reason: Option<String>,
    pub content_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCheckpointPayload {
    pub schema_version: u32,
    pub session_id: String,
    pub checkpoint_id: String,
    pub journal_cursor: u64,
    pub repository_fingerprint: String,
    pub files: Vec<CheckpointFilePayload>,
    pub unresolved_risks: Vec<String>,
    pub payload_fingerprint: String,
}

impl RuntimeCheckpointPayload {
    pub fn verify(&self) -> Result<(), RuntimeError> {
        if self.schema_version != PAYLOAD_SCHEMA_VERSION {
            return Err(RuntimeError::agent(format!(
                "unsupported runtime checkpoint payload schema version {}",
                self.schema_version
            )));
        }
        validate_identifier(&self.session_id)?;
        validate_digest(&self.checkpoint_id, "checkpoint id")?;
        validate_digest(&self.repository_fingerprint, "repository fingerprint")?;
        validate_digest(&self.payload_fingerprint, "payload fingerprint")?;
        let mut prior = None::<&str>;
        for file in &self.files {
            safe_relative_path(&file.path)?;
            if prior.is_some_and(|value| value >= file.path.as_str()) {
                return Err(RuntimeError::agent(
                    "runtime checkpoint payload paths are not strictly sorted",
                ));
            }
            prior = Some(&file.path);
            validate_digest(&file.content_fingerprint, "file content fingerprint")?;
            if file.content.is_some() && file.unsupported_reason.is_some() {
                return Err(RuntimeError::agent(
                    "runtime checkpoint file cannot be both captured and unsupported",
                ));
            }
            if file.content_fingerprint != file_fingerprint(file)? {
                return Err(RuntimeError::agent(
                    "runtime checkpoint file fingerprint does not match its contents",
                ));
            }
        }
        if self.repository_fingerprint != repository_fingerprint_from_files(&self.files)? {
            return Err(RuntimeError::agent(
                "runtime checkpoint repository fingerprint does not match its files",
            ));
        }
        if self.payload_fingerprint != payload_fingerprint(self)? {
            return Err(RuntimeError::agent(
                "runtime checkpoint payload fingerprint does not match its contents",
            ));
        }
        Ok(())
    }
}

pub(crate) fn repository_fingerprint(
    repo: &Path,
    events: &[EventEnvelope],
) -> Result<String, RuntimeError> {
    let files = capture_files(repo, events)?;
    repository_fingerprint_from_files(&files)
}

pub(crate) fn materialize(
    repo: &Path,
    checkpoint: &RuntimeCheckpointRecord,
) -> Result<RuntimeCheckpointPayload, RuntimeError> {
    checkpoint.verify()?;
    let events = replay_events(repo, &checkpoint.session_id, 0).map_err(RuntimeError::agent)?;
    let cursor = usize::try_from(checkpoint.journal_cursor).map_err(RuntimeError::agent)?;
    if cursor > events.len() {
        return Err(RuntimeError::agent(
            "checkpoint cursor is beyond the canonical journal while creating its payload",
        ));
    }
    let files = capture_files(repo, &events[..cursor])?;
    let repository_fingerprint = repository_fingerprint_from_files(&files)?;
    if checkpoint.checkpoint.repository_snapshot_fingerprint != repository_fingerprint {
        return Err(RuntimeError::agent(
            "checkpoint repository fingerprint does not match the captured payload",
        ));
    }
    let unresolved_risks = files
        .iter()
        .filter_map(|file| {
            file.unsupported_reason
                .as_ref()
                .map(|reason| format!("{}: {reason}", file.path))
        })
        .collect::<Vec<_>>();
    let mut payload = RuntimeCheckpointPayload {
        schema_version: PAYLOAD_SCHEMA_VERSION,
        session_id: checkpoint.session_id.clone(),
        checkpoint_id: checkpoint.checkpoint.fingerprint.clone(),
        journal_cursor: checkpoint.journal_cursor,
        repository_fingerprint,
        files,
        unresolved_risks,
        payload_fingerprint: String::new(),
    };
    payload.payload_fingerprint = payload_fingerprint(&payload)?;
    payload.verify()?;
    persist(repo, &payload)?;
    Ok(payload)
}

pub(crate) fn load(
    repo: &Path,
    session_id: &str,
    checkpoint_id: &str,
) -> Result<RuntimeCheckpointPayload, RuntimeError> {
    validate_identifier(session_id)?;
    validate_digest(checkpoint_id, "checkpoint id")?;
    let path = payload_path(repo, session_id, checkpoint_id);
    let bytes = fs::read(&path).map_err(|error| {
        RuntimeError::agent(format!(
            "runtime checkpoint payload {} could not be read: {error}",
            path.display()
        ))
    })?;
    let payload: RuntimeCheckpointPayload =
        serde_json::from_slice(&bytes).map_err(RuntimeError::agent)?;
    payload.verify()?;
    if payload.session_id != session_id || payload.checkpoint_id != checkpoint_id {
        return Err(RuntimeError::agent(
            "runtime checkpoint payload identity does not match the selected checkpoint",
        ));
    }
    Ok(payload)
}

pub(crate) fn preview(
    repo: &Path,
    payload: &RuntimeCheckpointPayload,
) -> Result<RecoveryPreview, RuntimeError> {
    payload.verify()?;
    let mut changes = Vec::new();
    let mut unresolved_risks = payload.unresolved_risks.clone();
    let mut current_files = Vec::with_capacity(payload.files.len());
    for target in &payload.files {
        let path = safe_relative_path(&target.path)?;
        let current = capture_path(repo, &path, &target.path)?;
        if let Some(reason) = &current.unsupported_reason {
            unresolved_risks.push(format!("{}: current repository {reason}", target.path));
        }
        if target.unsupported_reason.is_some() || current.unsupported_reason.is_some() {
            current_files.push(current);
            continue;
        }
        let kind = match (&target.content, &current.content) {
            (Some(expected), Some(actual)) if expected != actual => Some(FileChangeKind::Modified),
            (Some(_), None) => Some(FileChangeKind::Added),
            (None, Some(_)) => Some(FileChangeKind::Deleted),
            _ => None,
        };
        if let Some(kind) = kind {
            changes.push(RecoveryFileChange {
                path: target.path.clone(),
                kind,
                would_overwrite_uncommitted_work: true,
            });
        }
        current_files.push(current);
    }
    unresolved_risks.sort();
    unresolved_risks.dedup();
    Ok(RecoveryPreview {
        checkpoint_id: payload.checkpoint_id.clone(),
        files: changes,
        unresolved_risks,
        repository_matches_checkpoint_base: repository_fingerprint_from_files(&current_files)?
            == payload.repository_fingerprint,
    })
}

pub(crate) fn current_repository_fingerprint(
    repo: &Path,
    payload: &RuntimeCheckpointPayload,
) -> Result<String, RuntimeError> {
    let mut current = Vec::with_capacity(payload.files.len());
    for file in &payload.files {
        let path = safe_relative_path(&file.path)?;
        current.push(capture_path(repo, &path, &file.path)?);
    }
    repository_fingerprint_from_files(&current)
}

pub(crate) fn restore(repo: &Path, payload: &RuntimeCheckpointPayload) -> Result<(), RuntimeError> {
    payload.verify()?;
    if !payload.unresolved_risks.is_empty()
        || payload
            .files
            .iter()
            .any(|file| file.unsupported_reason.is_some())
    {
        return Err(RuntimeError::agent(
            "runtime checkpoint restore is blocked by unsupported file payloads",
        ));
    }

    let transaction = repo
        .join(RESTORE_TRANSACTION_DIRECTORY)
        .join(&payload.payload_fingerprint);
    if transaction.exists() {
        fs::remove_dir_all(&transaction)
            .map_err(|error| checkpoint_io("remove stale restore transaction", &transaction, error))?;
    }
    let staged = transaction.join("staged");
    let backups = transaction.join("backups");
    fs::create_dir_all(&staged)
        .map_err(|error| checkpoint_io("create restore staging directory", &staged, error))?;
    fs::create_dir_all(&backups)
        .map_err(|error| checkpoint_io("create restore backup directory", &backups, error))?;

    let mut prepared = Vec::with_capacity(payload.files.len());
    for (index, file) in payload.files.iter().enumerate() {
        let relative = safe_relative_path(&file.path)?;
        reject_symlink_components(repo, &relative)?;
        let destination = repo.join(&relative);
        let backup = backups.join(index.to_string());
        let existed = destination.is_file();
        if destination.exists() && !existed {
            return Err(RuntimeError::agent(format!(
                "checkpoint restore target {} is not a regular file",
                file.path
            )));
        }
        if existed {
            fs::copy(&destination, &backup)
                .map_err(|error| checkpoint_io("back up restore target", &destination, error))?;
        }
        let staged_path = file
            .content
            .as_ref()
            .map(|content| {
                let path = staged.join(index.to_string());
                fs::write(&path, content.as_bytes())
                    .map_err(|error| checkpoint_io("write staged restore payload", &path, error))?;
                Ok::<PathBuf, RuntimeError>(path)
            })
            .transpose()?;
        prepared.push((destination, backup, existed, staged_path));
    }

    let mut applied = 0usize;
    let apply_result = (|| -> Result<(), RuntimeError> {
        for (destination, _, _, staged_path) in &prepared {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    checkpoint_io("create restore target directory", parent, error)
                })?;
            }
            match staged_path {
                Some(path) => {
                    let temporary = destination
                        .with_extension(format!("medusa-restore-{}-tmp", std::process::id()));
                    fs::copy(path, &temporary).map_err(|error| {
                        checkpoint_io("copy staged restore payload", &temporary, error)
                    })?;
                    sync_file(&temporary)?;
                    if destination.exists() {
                        fs::remove_file(destination).map_err(|error| {
                            checkpoint_io("remove prior restore target", destination, error)
                        })?;
                    }
                    fs::rename(&temporary, destination).map_err(|error| {
                        checkpoint_io("install restored file", destination, error)
                    })?;
                }
                None if destination.exists() => {
                    fs::remove_file(destination).map_err(|error| {
                        checkpoint_io("remove checkpoint-deleted file", destination, error)
                    })?;
                }
                None => {}
            }
            applied = applied.saturating_add(1);
        }
        Ok(())
    })();

    if let Err(error) = apply_result {
        for (destination, backup, existed, _) in prepared[..applied].iter().rev() {
            let rollback = if *existed {
                if destination.exists() {
                    fs::remove_file(destination)
                } else {
                    Ok(())
                }
                .and_then(|()| fs::copy(backup, destination).map(|_| ()))
            } else if destination.exists() {
                fs::remove_file(destination)
            } else {
                Ok(())
            };
            if rollback.is_err() {
                return Err(RuntimeError::agent(format!(
                    "checkpoint restore failed and rollback was incomplete: {error}"
                )));
            }
        }
        return Err(error);
    }

    fs::remove_dir_all(&transaction)
        .map_err(|error| checkpoint_io("remove completed restore transaction", &transaction, error))?;
    if let Some(parent) = transaction.parent() {
        sync_parent(parent).map_err(RuntimeError::agent)?;
    }
    Ok(())
}

fn capture_files(
    repo: &Path,
    events: &[EventEnvelope],
) -> Result<Vec<CheckpointFilePayload>, RuntimeError> {
    let mut paths = BTreeSet::new();
    for event in events {
        if let EventPayload::FileTransactionCommitted { paths: changed, .. } = &event.payload {
            for value in changed {
                let relative = safe_relative_path(value)?;
                paths.insert(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    paths
        .into_iter()
        .map(|value| {
            let relative = safe_relative_path(&value)?;
            capture_path(repo, &relative, &value)
        })
        .collect()
}

fn capture_path(
    repo: &Path,
    relative: &Path,
    display: &str,
) -> Result<CheckpointFilePayload, RuntimeError> {
    reject_symlink_components(repo, relative)?;
    let path = repo.join(relative);
    let (content, unsupported_reason) = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            (None, Some("symlinks are not captured".to_owned()))
        }
        Ok(metadata) if !metadata.is_file() => {
            (None, Some("non-file paths are not captured".to_owned()))
        }
        Ok(metadata) if metadata.len() > MAX_CHECKPOINT_FILE_BYTES => (
            None,
            Some(format!(
                "file is {} bytes; limit is {MAX_CHECKPOINT_FILE_BYTES}",
                metadata.len()
            )),
        ),
        Ok(_) => match fs::read(&path).map_err(RuntimeError::agent)? {
            bytes if std::str::from_utf8(&bytes).is_ok() => (
                Some(String::from_utf8(bytes).map_err(RuntimeError::agent)?),
                None,
            ),
            _ => (None, Some("non-UTF-8 files are not captured".to_owned())),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, None),
        Err(error) => return Err(RuntimeError::agent(error)),
    };
    let mut file = CheckpointFilePayload {
        path: display.to_owned(),
        content,
        unsupported_reason,
        content_fingerprint: String::new(),
    };
    file.content_fingerprint = file_fingerprint(&file)?;
    Ok(file)
}

fn persist(repo: &Path, payload: &RuntimeCheckpointPayload) -> Result<(), RuntimeError> {
    payload.verify()?;
    let destination = payload_path(repo, &payload.session_id, &payload.checkpoint_id);
    if destination.is_file() {
        let existing: RuntimeCheckpointPayload =
            serde_json::from_slice(&fs::read(&destination).map_err(RuntimeError::agent)?)
                .map_err(RuntimeError::agent)?;
        existing.verify()?;
        if existing == *payload {
            return Ok(());
        }
        return Err(RuntimeError::agent(
            "checkpoint payload fingerprint is already bound to conflicting content",
        ));
    }
    let directory = destination
        .parent()
        .ok_or_else(|| RuntimeError::agent("checkpoint payload path has no parent"))?;
    fs::create_dir_all(directory).map_err(RuntimeError::agent)?;
    let temporary = directory.join(format!(
        ".{}.{}.tmp",
        payload.checkpoint_id,
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    ));
    let bytes = serde_json::to_vec_pretty(payload).map_err(RuntimeError::agent)?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(RuntimeError::agent)?;
    if let Err(error) = (|| -> std::io::Result<()> {
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &destination)?;
        sync_parent(directory)?;
        Ok(())
    })() {
        let _ = fs::remove_file(&temporary);
        return Err(RuntimeError::agent(error));
    }
    Ok(())
}

fn payload_path(repo: &Path, session_id: &str, checkpoint_id: &str) -> PathBuf {
    repo.join(PAYLOAD_DIRECTORY)
        .join(session_id)
        .join(format!("{checkpoint_id}.json"))
}

fn payload_fingerprint(payload: &RuntimeCheckpointPayload) -> Result<String, RuntimeError> {
    digest(&(
        payload.schema_version,
        &payload.session_id,
        &payload.checkpoint_id,
        payload.journal_cursor,
        &payload.repository_fingerprint,
        &payload.files,
        &payload.unresolved_risks,
    ))
}

fn repository_fingerprint_from_files(
    files: &[CheckpointFilePayload],
) -> Result<String, RuntimeError> {
    digest(files)
}

fn file_fingerprint(file: &CheckpointFilePayload) -> Result<String, RuntimeError> {
    digest(&(&file.path, &file.content, &file.unsupported_reason))
}

fn digest<T: Serialize + ?Sized>(value: &T) -> Result<String, RuntimeError> {
    let bytes = serde_json::to_vec(value).map_err(RuntimeError::agent)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn validate_identifier(value: &str) -> Result<(), RuntimeError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(RuntimeError::agent(
            "checkpoint payload identifier is not path-safe",
        ));
    }
    Ok(())
}

fn validate_digest(value: &str, label: &str) -> Result<(), RuntimeError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(RuntimeError::agent(format!(
            "checkpoint payload {label} is not a lowercase SHA-256 digest"
        )));
    }
    Ok(())
}

fn safe_relative_path(value: &str) -> Result<PathBuf, RuntimeError> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(RuntimeError::agent(format!(
            "unsafe checkpoint repository path: {value}"
        )));
    }
    Ok(path.to_path_buf())
}

fn reject_symlink_components(repo: &Path, relative: &Path) -> Result<(), RuntimeError> {
    let mut current = repo.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            continue;
        };
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(RuntimeError::agent(format!(
                    "checkpoint path traverses symlink {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(RuntimeError::agent(error)),
        }
    }
    Ok(())
}

fn sync_file(path: &Path) -> Result<(), RuntimeError> {
    OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| checkpoint_io("sync restored file", path, error))
}

fn checkpoint_io(operation: &str, path: &Path, error: std::io::Error) -> RuntimeError {
    RuntimeError::agent(format!("{operation} at {}: {error}", path.display()))
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
    use medusa_agent::{AgentEngine, record_session_event};
    use medusa_config::Config;
    use medusa_core::{CorrelationId, MedusaResult, SessionId};
    use medusa_protocol::Actor;
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};
    use tempfile::tempdir;

    use super::*;

    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("session creation does not call the provider")
        }
    }

    #[test]
    fn captures_previews_and_restores_tracked_files() {
        let repository = tempdir().expect("repository");
        fs::create_dir_all(repository.path().join("src")).expect("src");
        fs::write(repository.path().join("src/lib.rs"), "checkpoint").expect("write");
        let engine = AgentEngine::new(UnusedProvider, Config::default());
        let mut session = engine
            .create_session(repository.path(), "Capture checkpoint payload".to_owned())
            .expect("session");
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::FileTransactionCommitted {
                paths: vec!["src/lib.rs".to_owned()],
                rollback_ref: "rollback".to_owned(),
            },
        )
        .expect("file event");
        let checkpoint =
            crate::checkpoint_store::materialize(repository.path(), session.id.as_str())
                .expect("checkpoint");
        let payload = materialize(repository.path(), &checkpoint).expect("payload");
        fs::write(repository.path().join("src/lib.rs"), "changed").expect("change");

        let preview = preview(repository.path(), &payload).expect("preview");
        assert_eq!(preview.files.len(), 1);
        assert_eq!(preview.files[0].kind, FileChangeKind::Modified);
        assert!(preview.files[0].would_overwrite_uncommitted_work);
        assert!(!preview.repository_matches_checkpoint_base);

        restore(repository.path(), &payload).expect("restore");
        assert_eq!(
            fs::read_to_string(repository.path().join("src/lib.rs")).expect("read"),
            "checkpoint"
        );
    }

    #[test]
    fn unsupported_binary_payload_fails_closed() {
        let repository = tempdir().expect("repository");
        fs::write(repository.path().join("binary.bin"), [0xff, 0xfe]).expect("binary");
        let event = medusa_protocol::EventEnvelope::new(
            0,
            SessionId::parse("ses-01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("session"),
            Actor::Coordinator,
            CorrelationId::parse("cor-01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("correlation"),
            EventPayload::FileTransactionCommitted {
                paths: vec!["binary.bin".to_owned()],
                rollback_ref: "rollback".to_owned(),
            },
            None,
            time::OffsetDateTime::UNIX_EPOCH,
        )
        .expect("event");
        let files = capture_files(repository.path(), &[event]).expect("capture");
        assert!(files[0].unsupported_reason.is_some());
        let repository_fingerprint = repository_fingerprint_from_files(&files).unwrap();
        let mut payload = RuntimeCheckpointPayload {
            schema_version: PAYLOAD_SCHEMA_VERSION,
            session_id: "session-1".to_owned(),
            checkpoint_id: "a".repeat(64),
            journal_cursor: 1,
            repository_fingerprint,
            unresolved_risks: vec!["binary.bin: non-UTF-8 files are not captured".to_owned()],
            files,
            payload_fingerprint: String::new(),
        };
        payload.payload_fingerprint = payload_fingerprint(&payload).unwrap();
        assert!(restore(repository.path(), &payload).is_err());
    }

    #[test]
    fn traversal_and_symlink_escape_are_rejected() {
        assert!(safe_relative_path("../escape").is_err());
        assert!(safe_relative_path("/absolute").is_err());
    }
}
