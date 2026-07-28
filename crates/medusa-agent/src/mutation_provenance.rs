use std::{
    fs,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MUTATION_PROVENANCE_SCHEMA_VERSION: u32 = 1;
const PROVENANCE_PATH: &str = ".medusa/mutation-provenance.json";
const MAX_RETAINED_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MutationContext {
    pub session_id: String,
    pub task_step_id: Option<String>,
    pub activity_id: String,
    pub actor: String,
    pub sequence: u64,
    pub occurred_at_unix_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MutationKind {
    Added,
    Modified,
    Deleted,
    Renamed { previous_path: String },
    Generated,
    BinaryUnavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MutationScope {
    pub start_byte: usize,
    pub preimage_len: usize,
    pub postimage_len: usize,
    pub preimage_fingerprint: String,
    pub postimage_fingerprint: String,
    pub retained_preimage: Option<Vec<u8>>,
    pub retained_postimage: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MutationRecord {
    pub id: String,
    pub context: MutationContext,
    pub path: String,
    pub kind: MutationKind,
    pub repository_fingerprint_before: String,
    pub repository_fingerprint_after: String,
    pub scope: MutationScope,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MutationJournal {
    pub schema_version: u32,
    pub records: Vec<MutationRecord>,
}

impl Default for MutationJournal {
    fn default() -> Self {
        Self {
            schema_version: MUTATION_PROVENANCE_SCHEMA_VERSION,
            records: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ScopeValidation {
    Current,
    MissingEvidence,
    Drifted,
    DependencyConflict { later_mutation_ids: Vec<String> },
}

impl MutationJournal {
    pub fn append(&mut self, record: MutationRecord) -> MedusaResult<()> {
        validate_record(&record)?;
        if let Some(existing) = self.records.iter().find(|item| item.id == record.id) {
            if existing == &record {
                return Ok(());
            }
            return Err(provenance_error(format!(
                "conflicting mutation identity: {}",
                record.id
            )));
        }
        if self.records.iter().any(|item| {
            item.context.session_id == record.context.session_id
                && item.context.sequence == record.context.sequence
        }) {
            return Err(provenance_error(format!(
                "conflicting mutation sequence {} for session {}",
                record.context.sequence, record.context.session_id
            )));
        }
        self.records.push(record);
        self.records.sort_by(|left, right| {
            left.context
                .session_id
                .cmp(&right.context.session_id)
                .then_with(|| left.context.sequence.cmp(&right.context.sequence))
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(())
    }

    #[must_use]
    pub fn validate_scope(&self, id: &str, current: &[u8]) -> ScopeValidation {
        let Some(record) = self.records.iter().find(|item| item.id == id) else {
            return ScopeValidation::MissingEvidence;
        };
        let Some(expected) = record.scope.retained_postimage.as_deref() else {
            return ScopeValidation::MissingEvidence;
        };
        let end = record.scope.start_byte.saturating_add(expected.len());
        if current.get(record.scope.start_byte..end) != Some(expected) {
            return ScopeValidation::Drifted;
        }
        let later = self
            .records
            .iter()
            .filter(|item| {
                item.path == record.path
                    && item.context.session_id == record.context.session_id
                    && item.context.sequence > record.context.sequence
                    && ranges_overlap(&record.scope, &item.scope)
            })
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        if later.is_empty() {
            ScopeValidation::Current
        } else {
            ScopeValidation::DependencyConflict {
                later_mutation_ids: later,
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_record(
    context: MutationContext,
    path: String,
    kind: MutationKind,
    repository_fingerprint_before: String,
    repository_fingerprint_after: String,
    start_byte: usize,
    preimage: &[u8],
    postimage: &[u8],
) -> MutationRecord {
    let preimage_fingerprint = fingerprint(preimage);
    let postimage_fingerprint = fingerprint(postimage);
    let id = fingerprint(
        format!(
            "{}:{}:{}:{}:{}:{}",
            context.session_id,
            context.sequence,
            context.activity_id,
            path,
            preimage_fingerprint,
            postimage_fingerprint
        )
        .as_bytes(),
    );
    MutationRecord {
        id,
        context,
        path,
        kind,
        repository_fingerprint_before,
        repository_fingerprint_after,
        scope: MutationScope {
            start_byte,
            preimage_len: preimage.len(),
            postimage_len: postimage.len(),
            preimage_fingerprint,
            postimage_fingerprint,
            retained_preimage: retain(preimage),
            retained_postimage: retain(postimage),
        },
    }
}

pub fn load(repo: &Path) -> MedusaResult<MutationJournal> {
    let path = repo.join(PROVENANCE_PATH);
    if !path.exists() {
        return Ok(MutationJournal::default());
    }
    let bytes = fs::read(&path)?;
    let journal: MutationJournal = serde_json::from_slice(&bytes)
        .map_err(|error| provenance_error(format!("mutation provenance is corrupt: {error}")))?;
    if journal.schema_version != MUTATION_PROVENANCE_SCHEMA_VERSION {
        return Err(provenance_error(format!(
            "unsupported mutation provenance schema version: {}",
            journal.schema_version
        )));
    }
    for record in &journal.records {
        validate_record(record)?;
    }
    Ok(journal)
}

pub fn persist(repo: &Path, journal: &MutationJournal) -> MedusaResult<()> {
    if journal.schema_version != MUTATION_PROVENANCE_SCHEMA_VERSION {
        return Err(provenance_error(
            "cannot persist unsupported provenance schema",
        ));
    }
    let path = repo.join(PROVENANCE_PATH);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = temporary_path(&path);
    let bytes = serde_json::to_vec_pretty(journal).map_err(|error| {
        provenance_error(format!("could not serialize mutation provenance: {error}"))
    })?;
    fs::write(&temporary, bytes)?;
    fs::rename(&temporary, &path)?;
    Ok(())
}

fn validate_record(record: &MutationRecord) -> MedusaResult<()> {
    if record.id.trim().is_empty()
        || record.context.session_id.trim().is_empty()
        || record.context.activity_id.trim().is_empty()
        || record.context.actor.trim().is_empty()
        || record.path.trim().is_empty()
        || record.repository_fingerprint_before.trim().is_empty()
        || record.repository_fingerprint_after.trim().is_empty()
    {
        return Err(provenance_error(
            "mutation provenance contains an empty identity field",
        ));
    }
    if record.context.occurred_at_unix_ms < 0 {
        return Err(provenance_error(
            "mutation provenance timestamp is negative",
        ));
    }
    if record.scope.preimage_len == 0 && record.scope.postimage_len == 0 {
        return Err(provenance_error("mutation provenance scope is empty"));
    }
    for (bytes, expected_len, expected_fingerprint) in [
        (
            record.scope.retained_preimage.as_deref(),
            record.scope.preimage_len,
            &record.scope.preimage_fingerprint,
        ),
        (
            record.scope.retained_postimage.as_deref(),
            record.scope.postimage_len,
            &record.scope.postimage_fingerprint,
        ),
    ] {
        if let Some(bytes) = bytes {
            if bytes.len() != expected_len || fingerprint(bytes) != *expected_fingerprint {
                return Err(provenance_error(
                    "mutation provenance retained evidence is invalid",
                ));
            }
        }
    }
    Ok(())
}

fn retain(bytes: &[u8]) -> Option<Vec<u8>> {
    (bytes.len() <= MAX_RETAINED_BYTES).then(|| bytes.to_vec())
}

fn ranges_overlap(left: &MutationScope, right: &MutationScope) -> bool {
    let left_end = left
        .start_byte
        .saturating_add(left.postimage_len.max(left.preimage_len));
    let right_end = right
        .start_byte
        .saturating_add(right.postimage_len.max(right.preimage_len));
    left.start_byte < right_end && right.start_byte < left_end
}

fn fingerprint(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension("json.medusa-tmp")
}

fn provenance_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Execution,
        message.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn record(sequence: u64, start: usize, before: &[u8], after: &[u8]) -> MutationRecord {
        build_record(
            context(sequence),
            "src/lib.rs".into(),
            MutationKind::Modified,
            format!("repo-{sequence}-before"),
            format!("repo-{sequence}-after"),
            start,
            before,
            after,
        )
    }

    #[test]
    fn duplicate_replay_is_idempotent_but_conflicting_identity_fails_closed() {
        let mut journal = MutationJournal::default();
        let first = record(1, 0, b"old", b"new");
        journal.append(first.clone()).unwrap();
        journal.append(first.clone()).unwrap();
        assert_eq!(journal.records.len(), 1);

        let mut conflicting = first;
        conflicting.path = "src/main.rs".into();
        assert!(journal.append(conflicting).is_err());
    }

    #[test]
    fn current_scope_allows_non_overlapping_later_edits() {
        let mut journal = MutationJournal::default();
        let first = record(1, 0, b"abc", b"xyz");
        let id = first.id.clone();
        journal.append(first).unwrap();
        journal.append(record(2, 8, b"q", b"r")).unwrap();
        assert_eq!(
            journal.validate_scope(&id, b"xyz-----r"),
            ScopeValidation::Current
        );
    }

    #[test]
    fn overlapping_later_mutation_blocks_earlier_revert() {
        let mut journal = MutationJournal::default();
        let first = record(1, 2, b"abc", b"xyz");
        let id = first.id.clone();
        journal.append(first).unwrap();
        let later = record(2, 3, b"y", b"Y");
        let later_id = later.id.clone();
        journal.append(later).unwrap();
        assert_eq!(
            journal.validate_scope(&id, b"--xyz"),
            ScopeValidation::DependencyConflict {
                later_mutation_ids: vec![later_id]
            }
        );
    }

    #[test]
    fn drifted_user_edit_fails_closed() {
        let mut journal = MutationJournal::default();
        let first = record(1, 0, b"old", b"new");
        let id = first.id.clone();
        journal.append(first).unwrap();
        assert_eq!(
            journal.validate_scope(&id, b"NEW"),
            ScopeValidation::Drifted
        );
    }

    #[test]
    fn persistence_survives_restart_and_rejects_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let mut journal = MutationJournal::default();
        journal.append(record(1, 0, b"old", b"new")).unwrap();
        persist(directory.path(), &journal).unwrap();
        assert_eq!(load(directory.path()).unwrap(), journal);

        fs::write(directory.path().join(PROVENANCE_PATH), b"{").unwrap();
        assert!(load(directory.path()).is_err());
    }

    #[test]
    fn oversized_content_keeps_hashes_but_not_source_bytes() {
        let bytes = vec![b'x'; MAX_RETAINED_BYTES + 1];
        let item = record(1, 0, &bytes, b"small");
        assert!(item.scope.retained_preimage.is_none());
        assert!(item.scope.retained_postimage.is_some());
    }
}
