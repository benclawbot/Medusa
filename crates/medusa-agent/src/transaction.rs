use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use medusa_core::{
    ErrorCategory, ErrorCode, MedusaError, MedusaResult, hidden_command, repository_mutation,
};
#[cfg(test)]
use medusa_protocol::EventPayload;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::policy::safe_path;

#[path = "mutation_provenance.rs"]
mod mutation_provenance;
pub use mutation_provenance::{MutationContext, MutationKind, ScopeValidation};
use mutation_provenance::{build_record, load as load_provenance, persist as persist_provenance};

static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkerMutationProposal {
    pub worker_id: String,
    pub task_id: String,
    pub lease_epoch: u64,
    pub path: String,
    pub expected_fingerprint: String,
    pub content: String,
    pub priority: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileMutation {
    pub path: String,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransactionPreview {
    pub affected_files: Vec<String>,
    pub risk: String,
    pub test_plan: Vec<String>,
    pub rollback_checkpoint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransactionOutcome {
    pub affected_files: Vec<String>,
    pub rolled_back: bool,
    pub detail: String,
    #[serde(default)]
    pub mutation_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RevertPreview {
    pub mutation_id: String,
    pub path: String,
    pub start_byte: usize,
    pub remove_len: usize,
    pub restore_len: usize,
}

#[derive(Debug)]
struct Backup {
    path: PathBuf,
    content: Option<Vec<u8>>,
    permissions: Option<fs::Permissions>,
}

pub fn preview(
    mutations: &[FileMutation],
    checkpoint: &str,
    test_plan: Vec<String>,
) -> TransactionPreview {
    TransactionPreview {
        affected_files: mutations
            .iter()
            .map(|mutation| mutation.path.clone())
            .collect(),
        risk: if mutations.len() > 1 {
            "multi_file_write"
        } else {
            "single_file_write"
        }
        .to_owned(),
        test_plan,
        rollback_checkpoint: checkpoint.to_owned(),
    }
}

/// Applies repository mutations without claiming selective-revert provenance.
///
/// Callers that possess authoritative session and activity identity should use
/// `apply_atomic_with_context`. Legacy callers remain safe, but their writes are explicitly
/// unavailable for provenance-authorized selective revert.
/// One phase record in a file-transaction lifecycle sidecar. Sidecars live at
/// `.medusa/transactions/<operation_id>.jsonl` and stay queryable after crashes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileTransactionLifecycleRecord {
    pub schema_version: u16,
    pub operation_id: String,
    pub phase: String,
    pub paths: Vec<String>,
    pub message: String,
    pub recorded_unix_ms: i128,
}

/// Tracks one git-mutation lifecycle (started/progress/committed/rolled back)
/// under a single operation id shared with the journal's `FileTransaction*` events.
#[derive(Clone, Debug)]
pub struct FileTransactionTracker {
    repo: PathBuf,
    operation_id: String,
    paths: Vec<String>,
}

fn lifecycle_now_ms() -> i128 {
    time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000
}

/// Starts lifecycle tracking for a mutation batch and journals the
/// `FileTransactionStarted` intent before any file is touched.
pub fn begin_file_transaction(
    repo: &Path,
    paths: &[String],
) -> MedusaResult<FileTransactionTracker> {
    let nanos = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
    let tracker = FileTransactionTracker {
        repo: repo.to_path_buf(),
        operation_id: format!("txn-{nanos}-{}", std::process::id()),
        paths: paths.to_vec(),
    };
    tracker.record("started", String::new())?;
    Ok(tracker)
}

/// Lists transactions whose sidecar never reached a terminal phase: hung or
/// crashed mutations that `FileTransactionCommitted` alone would hide.
#[cfg(test)]
pub fn list_incomplete_transactions(
    repo: &Path,
) -> MedusaResult<Vec<FileTransactionLifecycleRecord>> {
    let dir = repo.join(".medusa").join("transactions");
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut incomplete = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry
            .path()
            .extension()
            .is_none_or(|extension| extension != "jsonl")
        {
            continue;
        }
        let body = fs::read_to_string(entry.path())?;
        let mut last: Option<FileTransactionLifecycleRecord> = None;
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            last = serde_json::from_str(line).ok();
        }
        if let Some(record) = last
            && record.phase != "committed"
            && record.phase != "rolled_back"
        {
            incomplete.push(record);
        }
    }
    incomplete.sort_by(|left, right| left.operation_id.cmp(&right.operation_id));
    Ok(incomplete)
}

impl FileTransactionTracker {
    #[must_use]
    #[cfg(test)]
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    fn record(&self, phase: &str, message: String) -> MedusaResult<()> {
        use std::io::Write as _;
        let dir = self.repo.join(".medusa").join("transactions");
        fs::create_dir_all(&dir)?;
        let record = FileTransactionLifecycleRecord {
            schema_version: 1,
            operation_id: self.operation_id.clone(),
            phase: phase.to_owned(),
            paths: self.paths.clone(),
            message,
            recorded_unix_ms: lifecycle_now_ms(),
        };
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("{}.jsonl", self.operation_id)))?;
        serde_json::to_writer(&mut file, &record).map_err(std::io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        Ok(())
    }

    pub fn progress(&self, message: impl Into<String>) -> MedusaResult<()> {
        self.record("progress", message.into())
    }

    pub fn committed(&self, rollback_ref: &str) -> MedusaResult<()> {
        self.record("committed", rollback_ref.to_owned())
    }

    pub fn rolled_back(&self, reason: &str) -> MedusaResult<()> {
        self.record("rolled_back", reason.to_owned())
    }

    #[must_use]
    #[cfg(test)]
    pub fn started_payload(&self) -> EventPayload {
        EventPayload::FileTransactionStarted {
            operation_id: self.operation_id.clone(),
            paths: self.paths.clone(),
        }
    }

    #[must_use]
    #[cfg(test)]
    pub fn progress_payload(&self, message: impl Into<String>) -> EventPayload {
        EventPayload::FileTransactionProgress {
            operation_id: self.operation_id.clone(),
            message: message.into(),
        }
    }

    #[must_use]
    #[cfg(test)]
    pub fn committed_payload(&self, paths: &[String], rollback_ref: &str) -> EventPayload {
        EventPayload::FileTransactionCommitted {
            paths: paths.to_vec(),
            rollback_ref: rollback_ref.to_owned(),
            operation_id: Some(self.operation_id.clone()),
        }
    }

    #[must_use]
    #[cfg(test)]
    pub fn rolled_back_payload(&self, reason: &str) -> EventPayload {
        EventPayload::FileTransactionRolledBack {
            operation_id: self.operation_id.clone(),
            rollback_ref: self.operation_id.clone(),
            reason: reason.to_owned(),
        }
    }
}

fn note_progress(tracker: Option<&FileTransactionTracker>, message: String) {
    if let Some(tracker) = tracker {
        let _ = tracker.progress(message);
    }
}

fn note_rollback(tracker: Option<&FileTransactionTracker>, reason: &str) {
    if let Some(tracker) = tracker {
        let _ = tracker.rolled_back(reason);
    }
}

/// Records the terminal phase of a tracker so `list_incomplete_transactions`
/// does not flag a successful batch as hung.
fn finalize_tracker(
    tracker: &FileTransactionTracker,
    outcome: &MedusaResult<TransactionOutcome>,
    paths: &[String],
) {
    match outcome {
        Ok(result) => {
            let rollback_ref = result
                .mutation_ids
                .first()
                .cloned()
                .unwrap_or_else(|| "provenance-unavailable".to_owned());
            let _ = tracker.committed(&rollback_ref);
            let _ = paths; // committed event payload is built by callers with a session
        }
        Err(error) => {
            let reason: String = error.to_string().chars().take(512).collect();
            let _ = tracker.rolled_back(&reason);
        }
    }
}

pub fn apply_atomic(repo: &Path, mutations: &[FileMutation]) -> MedusaResult<TransactionOutcome> {
    let paths = mutations.iter().map(|m| m.path.clone()).collect::<Vec<_>>();
    let tracker = begin_file_transaction(repo, &paths)?;
    let outcome = apply_atomic_with_tracker(repo, mutations, Some(&tracker));
    finalize_tracker(&tracker, &outcome, &paths);
    outcome
}

/// Session-free entry point for journaled mutation flows: applies mutations with
/// lifecycle tracking but without emitting session events. The session-aware
/// `apply_atomic_with_events` lives in `evidence`, next to the journal writer.
pub(crate) fn apply_atomic_with_tracker(
    repo: &Path,
    mutations: &[FileMutation],
    tracker: Option<&FileTransactionTracker>,
) -> MedusaResult<TransactionOutcome> {
    apply_atomic_inner(repo, mutations, None, true, tracker)
}

/// Applies every repository mutation through the rollback-capable boundary and atomically records
/// authoritative mutation provenance. If provenance persistence fails, all committed file writes
/// are rolled back before returning an error.
pub fn apply_atomic_with_context(
    repo: &Path,
    mutations: &[FileMutation],
    context: &MutationContext,
) -> MedusaResult<TransactionOutcome> {
    let paths = mutations.iter().map(|m| m.path.clone()).collect::<Vec<_>>();
    let tracker = begin_file_transaction(repo, &paths)?;
    let outcome = apply_atomic_inner(repo, mutations, Some(context), true, Some(&tracker));
    finalize_tracker(&tracker, &outcome, &paths);
    outcome
}

fn apply_atomic_inner(
    repo: &Path,
    mutations: &[FileMutation],
    context: Option<&MutationContext>,
    acquire_repository_lock: bool,
    tracker: Option<&FileTransactionTracker>,
) -> MedusaResult<TransactionOutcome> {
    if mutations.is_empty() {
        return Err(MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            "transaction must contain at least one file mutation",
        ));
    }

    let _repository_guard = acquire_repository_lock.then(|| repository_mutation::lock(repo));

    let repository_before = if context.is_some() {
        Some(repository_fingerprint(repo)?)
    } else {
        None
    };
    let mut resolved = Vec::with_capacity(mutations.len());
    let mut unique_targets = BTreeSet::new();
    for mutation in mutations {
        let target = safe_path(repo, &mutation.path)?;
        if !unique_targets.insert(target.clone()) {
            return Err(MedusaError::new(
                ErrorCode::InvalidConfiguration,
                ErrorCategory::Validation,
                format!("transaction contains duplicate target: {}", mutation.path),
            ));
        }
        resolved.push((mutation, target));
    }
    note_progress(
        tracker,
        format!("resolved {} mutation targets", resolved.len()),
    );

    let mut backups = Vec::with_capacity(mutations.len());
    let mut staged = Vec::with_capacity(mutations.len());

    for (index, (mutation, target)) in resolved.iter().enumerate() {
        let metadata = fs::metadata(target).ok();
        let original = if metadata.is_some() {
            Some(fs::read(target)?)
        } else {
            None
        };
        let permissions = metadata.map(|metadata| metadata.permissions());
        backups.push(Backup {
            path: target.clone(),
            content: original,
            permissions: permissions.clone(),
        });

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = unique_staging_path(target, index);
        if let Err(error) = fs::write(&temporary, mutation.content.as_bytes()) {
            cleanup_staged(&staged);
            return Err(error.into());
        }
        if let Some(permissions) = permissions {
            if let Err(error) = fs::set_permissions(&temporary, permissions) {
                let _ = fs::remove_file(&temporary);
                cleanup_staged(&staged);
                return Err(error.into());
            }
        }
        staged.push((target.clone(), temporary));
    }
    note_progress(tracker, format!("staged {} writes", staged.len()));

    for (index, (target, temporary)) in staged.iter().enumerate() {
        if !matches_backup(target, &backups[index])? {
            note_rollback(tracker, "target changed before commit");
            let rollback = rollback(&backups[..index]);
            cleanup_staged(&staged[index..]);
            return Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Execution,
                format!(
                    "transaction target changed before commit: {}; rollback={rollback}",
                    target.display()
                ),
            ));
        }
        if let Err(error) = fs::rename(temporary, target) {
            note_rollback(tracker, "commit rename failed");
            let rollback = rollback(&backups[..index]);
            cleanup_staged(&staged[index..]);
            return Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Execution,
                format!("transaction commit failed: {error}; rollback={rollback}"),
            ));
        }
    }

    note_progress(tracker, format!("committed {} files", staged.len()));

    let mut mutation_ids = Vec::new();
    if let Some(context) = context {
        let repository_before = repository_before.ok_or_else(|| {
            provenance_boundary_error("authoritative mutation fingerprint is unavailable")
        })?;
        let repository_after = repository_fingerprint(repo)?;
        let mut journal = match load_provenance(repo) {
            Ok(journal) => journal,
            Err(error) => {
                note_rollback(tracker, "provenance unavailable after write");
                let rollback = rollback(&backups);
                return Err(MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Execution,
                    format!(
                        "mutation provenance unavailable after write; rollback={rollback}: {error}"
                    ),
                ));
            }
        };
        for (index, ((mutation, _), backup)) in resolved.iter().zip(&backups).enumerate() {
            let before = backup.content.as_deref().unwrap_or_default();
            let after = mutation.content.as_bytes();
            let (start_byte, preimage, postimage) = minimal_scope(before, after);
            let mut item_context = context.clone();
            item_context.sequence = item_context
                .sequence
                .checked_add(index as u64)
                .ok_or_else(|| provenance_boundary_error("mutation sequence overflow"))?;
            let kind = if backup.content.is_none() {
                MutationKind::Added
            } else {
                MutationKind::Modified
            };
            let record = build_record(
                item_context,
                mutation.path.clone(),
                kind,
                repository_before.clone(),
                repository_after.clone(),
                start_byte,
                preimage,
                postimage,
            );
            mutation_ids.push(record.id.clone());
            if let Err(error) = journal.append(record) {
                note_rollback(tracker, "provenance conflict");
                let rollback = rollback(&backups);
                return Err(MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Execution,
                    format!("mutation provenance conflict; rollback={rollback}: {error}"),
                ));
            }
        }
        if let Err(error) = persist_provenance(repo, &journal) {
            note_rollback(tracker, "provenance persistence failed");
            let rollback = rollback(&backups);
            return Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Execution,
                format!("mutation provenance persistence failed; rollback={rollback}: {error}"),
            ));
        }
    }

    Ok(TransactionOutcome {
        affected_files: mutations
            .iter()
            .map(|mutation| mutation.path.clone())
            .collect(),
        rolled_back: false,
        detail: if context.is_some() {
            "all file mutations committed with authoritative provenance"
        } else {
            "all file mutations committed; selective revert provenance unavailable"
        }
        .to_owned(),
        mutation_ids,
    })
}

pub(crate) fn next_mutation_sequence(repo: &Path, session_id: &str) -> MedusaResult<u64> {
    let journal = load_provenance(repo)?;
    journal
        .records
        .iter()
        .filter(|record| record.context.session_id == session_id)
        .map(|record| record.context.sequence)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| provenance_boundary_error("mutation sequence overflow"))
}

pub fn preview_session_selective_revert(
    repo: &Path,
    session_id: &str,
    mutation_id: &str,
) -> MedusaResult<RevertPreview> {
    let journal = load_provenance(repo)?;
    let record = journal
        .records
        .iter()
        .find(|record| record.id == mutation_id)
        .ok_or_else(|| provenance_boundary_error("mutation provenance record is missing"))?;
    if record.context.session_id != session_id {
        return Err(provenance_boundary_error(
            "mutation provenance does not belong to the requested session",
        ));
    }
    preview_selective_revert(repo, mutation_id)
}

pub fn apply_session_selective_revert(
    repo: &Path,
    session_id: &str,
    mutation_id: &str,
    activity_id: &str,
    actor: &str,
) -> MedusaResult<TransactionOutcome> {
    preview_session_selective_revert(repo, session_id, mutation_id)?;
    let sequence = next_mutation_sequence(repo, session_id)?;
    let occurred_at_unix_ms =
        i64::try_from(time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000)
            .map_err(|_| provenance_boundary_error("mutation timestamp overflow"))?;
    let context = MutationContext {
        session_id: session_id.to_owned(),
        task_step_id: None,
        activity_id: activity_id.to_owned(),
        actor: actor.to_owned(),
        sequence,
        occurred_at_unix_ms,
    };
    let outcome = apply_selective_revert(repo, mutation_id, &context)?;
    #[cfg(not(test))]
    {
        let mut session = crate::session::load(repo, session_id)?;
        session.evidence.clear();
        session.updated_at = time::OffsetDateTime::now_utc();
        crate::session::persist(&session)?;
    }
    Ok(outcome)
}

pub fn preview_selective_revert(repo: &Path, mutation_id: &str) -> MedusaResult<RevertPreview> {
    let journal = load_provenance(repo)?;
    let record = journal
        .records
        .iter()
        .find(|record| record.id == mutation_id)
        .ok_or_else(|| provenance_boundary_error("mutation provenance record is missing"))?;
    let path = safe_path(repo, &record.path)?;
    let current = match &record.kind {
        MutationKind::Added | MutationKind::Modified => fs::read(&path)?,
        MutationKind::Deleted => {
            if path.exists() {
                return Err(provenance_boundary_error(
                    "selective revert rejected because a deleted path was recreated",
                ));
            }
            Vec::new()
        }
        MutationKind::Renamed { .. } => {
            return Err(provenance_boundary_error(
                "selective revert is unavailable for rename provenance",
            ));
        }
        MutationKind::Generated => {
            return Err(provenance_boundary_error(
                "selective revert is unavailable for generated-file provenance",
            ));
        }
        MutationKind::BinaryUnavailable => {
            return Err(provenance_boundary_error(
                "selective revert is unavailable when binary inverse evidence is unavailable",
            ));
        }
    };
    match journal.validate_scope(mutation_id, &current) {
        ScopeValidation::Current => {}
        ScopeValidation::MissingEvidence => {
            return Err(provenance_boundary_error(
                "selective revert requires retained inverse evidence",
            ));
        }
        ScopeValidation::Drifted => {
            return Err(provenance_boundary_error(
                "selective revert rejected because the authored scope drifted",
            ));
        }
        ScopeValidation::DependencyConflict { later_mutation_ids } => {
            return Err(provenance_boundary_error(format!(
                "selective revert rejected because later mutations overlap: {}",
                later_mutation_ids.join(", ")
            )));
        }
    }
    let restore_len = record.scope.retained_preimage.as_ref().map_or(0, Vec::len);
    Ok(RevertPreview {
        mutation_id: record.id.clone(),
        path: record.path.clone(),
        start_byte: record.scope.start_byte,
        remove_len: record.scope.postimage_len,
        restore_len,
    })
}

pub fn apply_selective_revert(
    repo: &Path,
    mutation_id: &str,
    context: &MutationContext,
) -> MedusaResult<TransactionOutcome> {
    let _repository_guard = repository_mutation::lock(repo);
    let preview = preview_selective_revert(repo, mutation_id)?;
    let journal = load_provenance(repo)?;
    let record = journal
        .records
        .iter()
        .find(|record| record.id == mutation_id)
        .ok_or_else(|| provenance_boundary_error("mutation provenance record disappeared"))?;
    let path = safe_path(repo, &preview.path)?;

    if matches!(record.kind, MutationKind::Added) {
        let expected = record.scope.retained_postimage.as_deref().ok_or_else(|| {
            provenance_boundary_error("selective revert postimage is unavailable")
        })?;
        return apply_delete_with_context(repo, &preview.path, expected, context);
    }

    if matches!(record.kind, MutationKind::Deleted) {
        if path.exists() {
            return Err(provenance_boundary_error(
                "selective revert scope changed during authorization",
            ));
        }
        let restore =
            record.scope.retained_preimage.as_deref().ok_or_else(|| {
                provenance_boundary_error("selective revert preimage is unavailable")
            })?;
        let content = String::from_utf8(restore.to_vec()).map_err(|_| {
            provenance_boundary_error("selective revert of non-UTF-8 content is unavailable")
        })?;
        return apply_atomic_inner(
            repo,
            &[FileMutation {
                path: preview.path,
                content,
            }],
            Some(context),
            false,
            None,
        );
    }

    let current = fs::read(&path)?;
    let end = preview
        .start_byte
        .checked_add(preview.remove_len)
        .ok_or_else(|| provenance_boundary_error("selective revert scope overflow"))?;
    let expected =
        record.scope.retained_postimage.as_deref().ok_or_else(|| {
            provenance_boundary_error("selective revert postimage is unavailable")
        })?;
    if current.get(preview.start_byte..end) != Some(expected) {
        return Err(provenance_boundary_error(
            "selective revert scope changed during authorization",
        ));
    }
    let restore = record
        .scope
        .retained_preimage
        .as_deref()
        .ok_or_else(|| provenance_boundary_error("selective revert preimage is unavailable"))?;
    let mut reverted = Vec::with_capacity(current.len() - preview.remove_len + restore.len());
    reverted.extend_from_slice(&current[..preview.start_byte]);
    reverted.extend_from_slice(restore);
    reverted.extend_from_slice(&current[end..]);
    let content = String::from_utf8(reverted).map_err(|_| {
        provenance_boundary_error("selective revert of non-UTF-8 content is unavailable")
    })?;
    apply_atomic_inner(
        repo,
        &[FileMutation {
            path: preview.path,
            content,
        }],
        Some(context),
        false,
        None,
    )
}

fn apply_delete_with_context(
    repo: &Path,
    relative: &str,
    expected_current: &[u8],
    context: &MutationContext,
) -> MedusaResult<TransactionOutcome> {
    let path = safe_path(repo, relative)?;
    let metadata = fs::metadata(&path)?;
    let current = fs::read(&path)?;
    if current != expected_current {
        return Err(provenance_boundary_error(
            "selective revert scope changed during authorization",
        ));
    }

    let repository_before = repository_fingerprint(repo)?;
    let mut journal = load_provenance(repo)?;
    let backup = Backup {
        path: path.clone(),
        content: Some(current.clone()),
        permissions: Some(metadata.permissions()),
    };
    fs::remove_file(&path)?;

    let repository_after = match repository_fingerprint(repo) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            let rollback = rollback(std::slice::from_ref(&backup));
            return Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Execution,
                format!("mutation fingerprint failed after delete; rollback={rollback}: {error}"),
            ));
        }
    };
    let record = build_record(
        context.clone(),
        relative.to_owned(),
        MutationKind::Deleted,
        repository_before,
        repository_after,
        0,
        &current,
        &[],
    );
    let mutation_id = record.id.clone();
    if let Err(error) = journal.append(record) {
        let rollback = rollback(std::slice::from_ref(&backup));
        return Err(MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Execution,
            format!("delete provenance conflict; rollback={rollback}: {error}"),
        ));
    }
    if let Err(error) = persist_provenance(repo, &journal) {
        let rollback = rollback(std::slice::from_ref(&backup));
        return Err(MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Execution,
            format!("delete provenance persistence failed; rollback={rollback}: {error}"),
        ));
    }

    Ok(TransactionOutcome {
        affected_files: vec![relative.to_owned()],
        rolled_back: false,
        detail: "file removed by selective revert with authoritative provenance".to_owned(),
        mutation_ids: vec![mutation_id],
    })
}

fn minimal_scope<'a>(before: &'a [u8], after: &'a [u8]) -> (usize, &'a [u8], &'a [u8]) {
    let prefix = before
        .iter()
        .zip(after)
        .take_while(|(left, right)| left == right)
        .count();
    let remaining_before = &before[prefix..];
    let remaining_after = &after[prefix..];
    let suffix = remaining_before
        .iter()
        .rev()
        .zip(remaining_after.iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let before_end = before.len().saturating_sub(suffix);
    let after_end = after.len().saturating_sub(suffix);
    (
        prefix,
        &before[prefix..before_end],
        &after[prefix..after_end],
    )
}

fn repository_fingerprint(repo: &Path) -> MedusaResult<String> {
    let output = hidden_command("git")
        .args(["diff", "--binary", "--no-ext-diff", "--", "."])
        .current_dir(repo)
        .output()?;
    if !output.status.success() {
        return Err(provenance_boundary_error(
            "could not fingerprint repository working tree",
        ));
    }
    Ok(hex::encode(Sha256::digest(&output.stdout)))
}

fn unique_staging_path(target: &Path, index: usize) -> PathBuf {
    let nonce = STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
    target.with_extension(format!(
        "medusa-txn-{}-{nonce}-{index}.tmp",
        std::process::id()
    ))
}

fn matches_backup(target: &Path, backup: &Backup) -> MedusaResult<bool> {
    match &backup.content {
        Some(expected) => Ok(target.exists() && fs::read(target)? == *expected),
        None => Ok(!target.exists()),
    }
}

fn provenance_boundary_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Execution,
        message.into(),
    )
}

fn rollback(backups: &[Backup]) -> &'static str {
    for backup in backups.iter().rev() {
        let result = match &backup.content {
            Some(content) => fs::write(&backup.path, content).and_then(|()| {
                if let Some(permissions) = &backup.permissions {
                    fs::set_permissions(&backup.path, permissions.clone())
                } else {
                    Ok(())
                }
            }),
            None => {
                if backup.path.exists() {
                    fs::remove_file(&backup.path)
                } else {
                    Ok(())
                }
            }
        };
        if result.is_err() {
            return "failed";
        }
    }
    "completed"
}

fn cleanup_staged(staged: &[(PathBuf, PathBuf)]) {
    for (_, temporary) in staged {
        let _ = fs::remove_file(temporary);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    use super::*;
    use medusa_protocol::EventPayload;

    #[test]
    fn file_transaction_lifecycle_links_commit_to_operation() {
        let directory = tempfile::tempdir().expect("tempdir");
        let paths = vec!["src/new.rs".to_owned()];
        let tracker = begin_file_transaction(directory.path(), &paths).expect("begin");
        tracker.progress("staged 1 write").expect("progress");
        tracker.committed("rollback-1").expect("committed");
        assert!(
            list_incomplete_transactions(directory.path())
                .expect("list")
                .is_empty()
        );
        let committed = tracker.committed_payload(&paths, "rollback-1");
        match committed {
            EventPayload::FileTransactionCommitted { operation_id, .. } => {
                assert_eq!(operation_id.as_deref(), Some(tracker.operation_id()));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn lifecycle_payloads_link_every_phase_to_one_operation() {
        let directory = tempfile::tempdir().expect("tempdir");
        // Payload constructors link every phase to the same operation id.
        let tracker =
            begin_file_transaction(directory.path(), &["src/x.rs".to_owned()]).expect("begin");
        match tracker.progress_payload("staged") {
            EventPayload::FileTransactionProgress { operation_id, .. } => {
                assert_eq!(operation_id, tracker.operation_id());
            }
            other => panic!("unexpected payload: {other:?}"),
        }
        match tracker.rolled_back_payload("reason") {
            EventPayload::FileTransactionRolledBack { operation_id, .. } => {
                assert_eq!(operation_id, tracker.operation_id());
            }
            other => panic!("unexpected payload: {other:?}"),
        }
        match tracker.started_payload() {
            EventPayload::FileTransactionStarted { operation_id, .. } => {
                assert_eq!(operation_id, tracker.operation_id());
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn crashed_transaction_stays_visible_until_rolled_back() {
        let directory = tempfile::tempdir().expect("tempdir");
        let paths = vec!["src/hung.rs".to_owned()];
        let hung = begin_file_transaction(directory.path(), &paths).expect("begin hung");
        hung.progress("resolved 1 mutation targets")
            .expect("progress");
        let incomplete = list_incomplete_transactions(directory.path()).expect("list hung");
        assert!(
            incomplete
                .iter()
                .any(|record| record.operation_id == hung.operation_id())
        );
        hung.rolled_back("test rollback").expect("rollback");
        assert!(
            list_incomplete_transactions(directory.path())
                .expect("list")
                .is_empty()
        );
    }

    fn context(sequence: u64) -> MutationContext {
        MutationContext {
            session_id: "session-1".into(),
            task_step_id: Some("step-1".into()),
            activity_id: format!("tool-{sequence}"),
            actor: "medusa".into(),
            sequence,
            occurred_at_unix_ms: 10,
        }
    }

    #[test]
    fn commits_multiple_files() {
        let directory = tempfile::tempdir().expect("tempdir");
        hidden_command("git")
            .args(["init", "-q"])
            .current_dir(directory.path())
            .status()
            .unwrap();
        let outcome = apply_atomic(
            directory.path(),
            &[
                FileMutation {
                    path: "a.txt".into(),
                    content: "a".into(),
                },
                FileMutation {
                    path: "nested/b.txt".into(),
                    content: "b".into(),
                },
            ],
        )
        .expect("transaction");
        assert!(!outcome.rolled_back);
        assert!(outcome.mutation_ids.is_empty());
        assert_eq!(
            fs::read_to_string(directory.path().join("a.txt")).unwrap(),
            "a"
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("nested/b.txt")).unwrap(),
            "b"
        );
    }

    #[test]
    fn records_minimal_scope_and_preserves_non_overlapping_user_edits_on_revert() {
        let directory = tempfile::tempdir().expect("tempdir");
        hidden_command("git")
            .args(["init", "-q"])
            .current_dir(directory.path())
            .status()
            .unwrap();
        fs::write(
            directory.path().join("value.txt"),
            "user-before\nold\nuser-after\n",
        )
        .unwrap();
        let outcome = apply_atomic_with_context(
            directory.path(),
            &[FileMutation {
                path: "value.txt".into(),
                content: "user-before\nnew\nuser-after\n".into(),
            }],
            &context(1),
        )
        .unwrap();
        fs::write(
            directory.path().join("value.txt"),
            "USER-BEFORE\nnew\nuser-after\n",
        )
        .unwrap();
        let reverted =
            apply_selective_revert(directory.path(), &outcome.mutation_ids[0], &context(2))
                .unwrap();
        assert_eq!(reverted.mutation_ids.len(), 1);
        assert_eq!(
            fs::read_to_string(directory.path().join("value.txt")).unwrap(),
            "USER-BEFORE\nold\nuser-after\n"
        );
    }

    #[test]
    fn added_file_revert_removes_file_and_deleted_revert_restores_it() {
        let directory = tempfile::tempdir().expect("tempdir");
        hidden_command("git")
            .args(["init", "-q"])
            .current_dir(directory.path())
            .status()
            .unwrap();
        let added = apply_atomic_with_context(
            directory.path(),
            &[FileMutation {
                path: "new.txt".into(),
                content: "created\n".into(),
            }],
            &context(1),
        )
        .unwrap();
        assert!(directory.path().join("new.txt").exists());

        let deleted =
            apply_selective_revert(directory.path(), &added.mutation_ids[0], &context(2)).unwrap();
        assert!(!directory.path().join("new.txt").exists());
        assert_eq!(deleted.mutation_ids.len(), 1);

        apply_selective_revert(directory.path(), &deleted.mutation_ids[0], &context(3)).unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("new.txt")).unwrap(),
            "created\n"
        );
    }

    #[test]
    fn line_ending_change_round_trips_exactly() {
        let directory = tempfile::tempdir().expect("tempdir");
        hidden_command("git")
            .args(["init", "-q"])
            .current_dir(directory.path())
            .status()
            .unwrap();
        fs::write(directory.path().join("value.txt"), b"alpha\r\nbeta\r\n").unwrap();
        let outcome = apply_atomic_with_context(
            directory.path(),
            &[FileMutation {
                path: "value.txt".into(),
                content: "alpha\nbeta\n".into(),
            }],
            &context(1),
        )
        .unwrap();
        apply_selective_revert(directory.path(), &outcome.mutation_ids[0], &context(2)).unwrap();
        assert_eq!(
            fs::read(directory.path().join("value.txt")).unwrap(),
            b"alpha\r\nbeta\r\n"
        );
    }

    #[test]
    fn unsupported_provenance_kinds_fail_closed() {
        for kind in [
            MutationKind::Renamed {
                previous_path: "old.txt".into(),
            },
            MutationKind::Generated,
            MutationKind::BinaryUnavailable,
        ] {
            let directory = tempfile::tempdir().expect("tempdir");
            hidden_command("git")
                .args(["init", "-q"])
                .current_dir(directory.path())
                .status()
                .unwrap();
            fs::write(directory.path().join("value.txt"), "after").unwrap();
            let mut journal = load_provenance(directory.path()).unwrap();
            let record = build_record(
                context(1),
                "value.txt".into(),
                kind,
                "before".into(),
                "after".into(),
                0,
                b"before",
                b"after",
            );
            let mutation_id = record.id.clone();
            journal.append(record).unwrap();
            persist_provenance(directory.path(), &journal).unwrap();
            assert!(preview_selective_revert(directory.path(), &mutation_id).is_err());
        }
    }

    #[test]
    fn overlapping_user_edit_rejects_selective_revert() {
        let directory = tempfile::tempdir().expect("tempdir");
        hidden_command("git")
            .args(["init", "-q"])
            .current_dir(directory.path())
            .status()
            .unwrap();
        fs::write(directory.path().join("value.txt"), "old").unwrap();
        let outcome = apply_atomic_with_context(
            directory.path(),
            &[FileMutation {
                path: "value.txt".into(),
                content: "new".into(),
            }],
            &context(1),
        )
        .unwrap();
        fs::write(directory.path().join("value.txt"), "NEW").unwrap();
        assert!(
            apply_selective_revert(directory.path(), &outcome.mutation_ids[0], &context(2))
                .is_err()
        );
    }

    #[test]
    fn rejects_escape_before_any_write() {
        let directory = tempfile::tempdir().expect("tempdir");
        let result = apply_atomic(
            directory.path(),
            &[
                FileMutation {
                    path: "safe.txt".into(),
                    content: "safe".into(),
                },
                FileMutation {
                    path: "../escape.txt".into(),
                    content: "bad".into(),
                },
            ],
        );
        assert!(result.is_err());
        assert!(!directory.path().join("safe.txt").exists());
    }

    #[test]
    fn rejects_duplicate_targets_before_staging() {
        let directory = tempfile::tempdir().expect("tempdir");
        let result = apply_atomic(
            directory.path(),
            &[
                FileMutation {
                    path: "same.txt".into(),
                    content: "first".into(),
                },
                FileMutation {
                    path: "same.txt".into(),
                    content: "second".into(),
                },
            ],
        );
        assert!(result.is_err());
        assert!(!directory.path().join("same.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_traversal_before_staging() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside");
        symlink(outside.path(), directory.path().join("linked")).expect("symlink");
        let result = apply_atomic(
            directory.path(),
            &[FileMutation {
                path: "linked/escape.txt".into(),
                content: "bad".into(),
            }],
        );
        assert!(result.is_err());
        assert!(!outside.path().join("escape.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn preserves_existing_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("script.sh");
        fs::write(&path, "old").expect("fixture");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).expect("permissions");
        apply_atomic(
            directory.path(),
            &[FileMutation {
                path: "script.sh".into(),
                content: "new".into(),
            }],
        )
        .expect("transaction");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }

    #[test]
    fn staging_paths_are_unique() {
        let target = Path::new("src/lib.rs");
        assert_ne!(
            unique_staging_path(target, 0),
            unique_staging_path(target, 0)
        );
    }

    #[test]
    fn repository_lock_serializes_overlapping_transactions() {
        let directory = tempfile::tempdir().expect("tempdir");
        let repo = Arc::new(directory.path().to_path_buf());
        let barrier = Arc::new(Barrier::new(3));
        let handles = ["first", "second"].map(|content| {
            let repo = Arc::clone(&repo);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                apply_atomic(
                    &repo,
                    &[FileMutation {
                        path: "same.txt".into(),
                        content: content.into(),
                    }],
                )
            })
        });
        barrier.wait();
        for handle in handles {
            handle.join().expect("worker").expect("transaction");
        }
        let final_content = fs::read_to_string(directory.path().join("same.txt")).unwrap();
        assert!(matches!(final_content.as_str(), "first" | "second"));
        assert!(
            fs::read_dir(directory.path())
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().contains("medusa-txn"))
        );
    }
}
