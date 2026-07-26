//! Deterministic repository rollback journals and crash recovery plans.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum FileState {
    Absent,
    /// Legacy fingerprint-only state. This remains readable for journal compatibility but is not
    /// accepted as rollback evidence because it cannot reconstruct file bytes.
    Present {
        content_fingerprint: String,
        byte_len: u64,
    },
    CapturedFile {
        content_fingerprint: String,
        content: Vec<u8>,
        #[serde(default)]
        unix_mode: Option<u32>,
    },
    Directory {
        #[serde(default)]
        unix_mode: Option<u32>,
    },
    Symlink {
        target: String,
    },
}

impl FileState {
    #[must_use]
    pub fn captured_file(content: impl Into<Vec<u8>>, unix_mode: Option<u32>) -> Self {
        let content = content.into();
        Self::CapturedFile {
            content_fingerprint: fingerprint_bytes(&content),
            content,
            unix_mode,
        }
    }

    #[must_use]
    pub fn directory(unix_mode: Option<u32>) -> Self {
        Self::Directory { unix_mode }
    }

    #[must_use]
    pub fn symlink(target: impl Into<String>) -> Self {
        Self::Symlink {
            target: target.into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RollbackEntry {
    pub path: String,
    pub before: FileState,
    pub after: FileState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum JournalState {
    Prepared,
    Applying,
    Applied,
    RollingBack,
    RolledBack,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveryJournal {
    pub journal_id: String,
    pub base_snapshot: String,
    pub target_snapshot: String,
    pub barrier_fingerprint: String,
    pub entries: Vec<RollbackEntry>,
    pub applied_paths: Vec<String>,
    pub state: JournalState,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RecoveryAction {
    Restore { path: String, state: FileState },
    VerifySnapshot { expected_fingerprint: String },
}

impl RecoveryJournal {
    pub fn prepare(
        journal_id: impl Into<String>,
        base_snapshot: impl Into<String>,
        target_snapshot: impl Into<String>,
        barrier_fingerprint: impl Into<String>,
        entries: Vec<RollbackEntry>,
    ) -> Result<Self, &'static str> {
        let journal_id = journal_id.into();
        if journal_id.trim().is_empty() {
            return Err("journal identifier cannot be empty");
        }
        let base_snapshot = base_snapshot.into();
        let target_snapshot = target_snapshot.into();
        let barrier_fingerprint = barrier_fingerprint.into();
        validate_digest(&base_snapshot)?;
        validate_digest(&target_snapshot)?;
        validate_digest(&barrier_fingerprint)?;
        let entries = canonical_entries(entries)?;
        if entries.is_empty() {
            return Err("rollback journal requires at least one entry");
        }
        let mut journal = Self {
            journal_id,
            base_snapshot,
            target_snapshot,
            barrier_fingerprint,
            entries,
            applied_paths: Vec::new(),
            state: JournalState::Prepared,
            fingerprint: String::new(),
        };
        journal.refresh_fingerprint();
        Ok(journal)
    }

    pub fn begin_apply(&mut self) -> Result<(), &'static str> {
        self.validate()?;
        if self.state != JournalState::Prepared {
            return Err("journal is not prepared");
        }
        self.state = JournalState::Applying;
        self.refresh_fingerprint();
        Ok(())
    }

    pub fn record_applied(&mut self, path: &str) -> Result<(), &'static str> {
        self.validate()?;
        if self.state != JournalState::Applying {
            return Err("journal is not applying");
        }
        if !self.entries.iter().any(|entry| entry.path == path) {
            return Err("path is not part of the journal");
        }
        if self
            .applied_paths
            .binary_search_by(|value| value.as_str().cmp(path))
            .is_ok()
        {
            return Err("path was already applied");
        }
        self.applied_paths.push(path.to_owned());
        self.applied_paths.sort();
        self.refresh_fingerprint();
        Ok(())
    }

    pub fn finish_apply(&mut self) -> Result<(), &'static str> {
        self.validate()?;
        if self.state != JournalState::Applying || self.applied_paths.len() != self.entries.len() {
            return Err("all journal entries must be applied before completion");
        }
        self.state = JournalState::Applied;
        self.refresh_fingerprint();
        Ok(())
    }

    pub fn begin_rollback(&mut self) -> Result<Vec<RecoveryAction>, &'static str> {
        self.validate()?;
        if !matches!(self.state, JournalState::Applying | JournalState::Applied) {
            return Err("journal cannot enter rollback from its current state");
        }
        self.state = JournalState::RollingBack;
        let actions = self.rollback_actions()?;
        self.refresh_fingerprint();
        Ok(actions)
    }

    pub fn finish_rollback(&mut self) -> Result<(), &'static str> {
        self.validate()?;
        if self.state != JournalState::RollingBack {
            return Err("journal is not rolling back");
        }
        self.state = JournalState::RolledBack;
        self.applied_paths.clear();
        self.refresh_fingerprint();
        Ok(())
    }

    pub fn crash_recovery_plan(&self) -> Result<Vec<RecoveryAction>, &'static str> {
        self.validate()?;
        match self.state {
            JournalState::Prepared | JournalState::RolledBack => Ok(Vec::new()),
            JournalState::Applying | JournalState::Applied | JournalState::RollingBack => {
                self.rollback_actions()
            }
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        validate_digest(&self.base_snapshot)?;
        validate_digest(&self.target_snapshot)?;
        validate_digest(&self.barrier_fingerprint)?;
        let canonical = canonical_entries(self.entries.clone())?;
        if canonical != self.entries {
            return Err("journal entries are not canonical");
        }
        if !is_sorted_unique(&self.applied_paths) {
            return Err("applied paths must be sorted and unique");
        }
        let known = self
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<BTreeSet<_>>();
        if self
            .applied_paths
            .iter()
            .any(|path| !known.contains(path.as_str()))
        {
            return Err("applied path is not present in journal");
        }
        if self.fingerprint != self.compute_fingerprint() {
            return Err("journal fingerprint does not match its contents");
        }
        Ok(())
    }

    fn rollback_actions(&self) -> Result<Vec<RecoveryAction>, &'static str> {
        let entries = self
            .entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry))
            .collect::<BTreeMap<_, _>>();
        let mut actions = Vec::with_capacity(self.applied_paths.len() + 1);
        for path in self.applied_paths.iter().rev() {
            let entry = entries
                .get(path.as_str())
                .ok_or("applied path is not present in journal")?;
            actions.push(RecoveryAction::Restore {
                path: path.clone(),
                state: entry.before.clone(),
            });
        }
        actions.push(RecoveryAction::VerifySnapshot {
            expected_fingerprint: self.base_snapshot.clone(),
        });
        Ok(actions)
    }

    fn compute_fingerprint(&self) -> String {
        hash(&(
            self.journal_id.as_str(),
            self.base_snapshot.as_str(),
            self.target_snapshot.as_str(),
            self.barrier_fingerprint.as_str(),
            &self.entries,
            &self.applied_paths,
            &self.state,
        ))
    }

    fn refresh_fingerprint(&mut self) {
        self.fingerprint = self.compute_fingerprint();
    }
}

fn canonical_entries(entries: Vec<RollbackEntry>) -> Result<Vec<RollbackEntry>, &'static str> {
    let mut by_path = BTreeMap::new();
    for entry in entries {
        validate_path(&entry.path)?;
        validate_recovery_state(&entry.before)?;
        validate_state(&entry.after)?;
        if entry.before == entry.after {
            return Err("rollback entry must change repository state");
        }
        if by_path.insert(entry.path.clone(), entry).is_some() {
            return Err("rollback paths must be unique");
        }
    }
    Ok(by_path.into_values().collect())
}

fn validate_recovery_state(state: &FileState) -> Result<(), &'static str> {
    validate_state(state)?;
    if matches!(state, FileState::Present { .. }) {
        return Err("rollback before-state must contain restorable bytes and metadata");
    }
    Ok(())
}

fn validate_state(state: &FileState) -> Result<(), &'static str> {
    match state {
        FileState::Absent => Ok(()),
        FileState::Present {
            content_fingerprint,
            ..
        } => validate_digest(content_fingerprint),
        FileState::CapturedFile {
            content_fingerprint,
            content,
            unix_mode,
        } => {
            validate_digest(content_fingerprint)?;
            validate_mode(*unix_mode)?;
            if content_fingerprint != &fingerprint_bytes(content) {
                return Err("captured file fingerprint does not match its bytes");
            }
            Ok(())
        }
        FileState::Directory { unix_mode } => validate_mode(*unix_mode),
        FileState::Symlink { target } => {
            if target.is_empty() || target.contains('\0') {
                return Err("symlink target cannot be empty or contain NUL");
            }
            Ok(())
        }
    }
}

fn validate_path(path: &str) -> Result<(), &'static str> {
    let bytes = path.as_bytes();
    let drive_prefixed = bytes.len() >= 2 && bytes[1] == b':';
    let invalid_component = path
        .split(['/', '\\'])
        .any(|part| part.is_empty() || matches!(part, "." | ".."));
    if path.trim().is_empty()
        || path.contains('\0')
        || path.starts_with(['/', '\\'])
        || drive_prefixed
        || invalid_component
    {
        return Err("paths must be canonical workspace-relative paths");
    }
    Ok(())
}

fn validate_mode(mode: Option<u32>) -> Result<(), &'static str> {
    if mode.is_some_and(|mode| mode & !0o7777 != 0) {
        return Err("unix mode must contain permission and special bits only");
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), &'static str> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("fingerprint must be a SHA-256 hex digest");
    }
    Ok(())
}

fn is_sorted_unique(values: &[String]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn hash<T: Serialize>(value: &T) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(value).expect("rollback journal types are serializable"),
    ))
}

fn fingerprint_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(value: &[u8]) -> String {
        fingerprint_bytes(value)
    }

    fn entry(path: &str) -> RollbackEntry {
        RollbackEntry {
            path: path.into(),
            before: FileState::captured_file(b"old".to_vec(), Some(0o644)),
            after: FileState::Present {
                content_fingerprint: digest(b"new"),
                byte_len: 3,
            },
        }
    }

    fn journal() -> RecoveryJournal {
        RecoveryJournal::prepare(
            "run-1",
            digest(b"base"),
            digest(b"target"),
            digest(b"barrier"),
            vec![entry("b.rs"), entry("a.rs")],
        )
        .unwrap()
    }

    #[test]
    fn rollback_restores_applied_paths_in_reverse_order() {
        let mut journal = journal();
        journal.begin_apply().unwrap();
        journal.record_applied("a.rs").unwrap();
        journal.record_applied("b.rs").unwrap();
        journal.finish_apply().unwrap();
        let actions = journal.begin_rollback().unwrap();
        assert!(matches!(&actions[0], RecoveryAction::Restore { path, .. } if path == "b.rs"));
        assert!(matches!(&actions[1], RecoveryAction::Restore { path, .. } if path == "a.rs"));
        assert!(matches!(&actions[2], RecoveryAction::VerifySnapshot { .. }));
    }

    #[test]
    fn created_files_are_removed_on_rollback() {
        let mut journal = RecoveryJournal::prepare(
            "create",
            digest(b"base"),
            digest(b"target"),
            digest(b"barrier"),
            vec![RollbackEntry {
                path: "created.txt".into(),
                before: FileState::Absent,
                after: FileState::Present {
                    content_fingerprint: digest(b"created"),
                    byte_len: 7,
                },
            }],
        )
        .unwrap();
        journal.begin_apply().unwrap();
        journal.record_applied("created.txt").unwrap();
        let actions = journal.begin_rollback().unwrap();
        assert!(matches!(
            &actions[0],
            RecoveryAction::Restore {
                path,
                state: FileState::Absent
            } if path == "created.txt"
        ));
    }

    #[test]
    fn rollback_evidence_contains_bytes_and_mode() {
        let mut journal = journal();
        journal.begin_apply().unwrap();
        journal.record_applied("a.rs").unwrap();
        let actions = journal.begin_rollback().unwrap();
        assert!(matches!(
            &actions[0],
            RecoveryAction::Restore {
                state: FileState::CapturedFile {
                    content,
                    unix_mode: Some(0o644),
                    ..
                },
                ..
            } if content == b"old"
        ));
    }

    #[test]
    fn directory_and_symlink_before_states_are_actionable() {
        for before in [
            FileState::directory(Some(0o755)),
            FileState::symlink("target"),
        ] {
            let journal = RecoveryJournal::prepare(
                "metadata",
                digest(b"base"),
                digest(b"target"),
                digest(b"barrier"),
                vec![RollbackEntry {
                    path: "entry".into(),
                    before,
                    after: FileState::Absent,
                }],
            );
            assert!(journal.is_ok());
        }
    }

    #[test]
    fn fingerprint_only_before_state_is_rejected() {
        let result = RecoveryJournal::prepare(
            "legacy",
            digest(b"base"),
            digest(b"target"),
            digest(b"barrier"),
            vec![RollbackEntry {
                path: "entry".into(),
                before: FileState::Present {
                    content_fingerprint: digest(b"old"),
                    byte_len: 3,
                },
                after: FileState::Absent,
            }],
        );
        assert_eq!(
            result.unwrap_err(),
            "rollback before-state must contain restorable bytes and metadata"
        );
    }

    #[test]
    fn rejects_windows_and_mixed_separator_traversal() {
        for path in [
            "../outside",
            "..\\outside",
            "safe/..\\outside",
            "/absolute",
            "\\\\server\\share",
            "C:\\device",
            "\\\\?\\C:\\device",
        ] {
            let mut invalid = entry(path);
            invalid.after = FileState::Absent;
            assert!(
                RecoveryJournal::prepare(
                    "paths",
                    digest(b"base"),
                    digest(b"target"),
                    digest(b"barrier"),
                    vec![invalid],
                )
                .is_err()
            );
        }
    }

    #[test]
    fn interrupted_apply_has_a_recovery_plan() {
        let mut journal = journal();
        journal.begin_apply().unwrap();
        journal.record_applied("a.rs").unwrap();
        let plan = journal.crash_recovery_plan().unwrap();
        assert_eq!(plan.len(), 2);
    }

    #[test]
    fn completion_requires_every_entry() {
        let mut journal = journal();
        journal.begin_apply().unwrap();
        journal.record_applied("a.rs").unwrap();
        assert!(journal.finish_apply().is_err());
    }

    #[test]
    fn tampered_journal_is_rejected() {
        let mut journal = journal();
        journal.entries[0].path = "changed.rs".into();
        assert!(journal.validate().is_err());
    }
}
