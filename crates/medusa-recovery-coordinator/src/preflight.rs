use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{FileChangeKind, RecoveryFileChange, RecoveryPreflightEvidence, RecoveryPreview};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryFileState {
    pub path: String,
    pub content_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositorySnapshot {
    pub repository_fingerprint: String,
    pub files: Vec<RepositoryFileState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPreflightReport {
    pub preview: RecoveryPreview,
    pub evidence: RecoveryPreflightEvidence,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RecoveryPreflightError {
    #[error("invalid repository fingerprint: {0}")]
    InvalidRepositoryFingerprint(String),
    #[error("invalid content fingerprint for {path}: {fingerprint}")]
    InvalidContentFingerprint { path: String, fingerprint: String },
    #[error("duplicate repository path: {0}")]
    DuplicatePath(String),
    #[error("repository path is unsafe: {0}")]
    UnsafePath(String),
}

pub fn build_restore_preflight(
    checkpoint_id: impl Into<String>,
    checkpoint: &RepositorySnapshot,
    current: &RepositorySnapshot,
    uncommitted_paths: impl IntoIterator<Item = String>,
    checkpoint_integrity_verified: bool,
) -> Result<RecoveryPreflightReport, RecoveryPreflightError> {
    validate_snapshot(checkpoint)?;
    validate_snapshot(current)?;

    let checkpoint_files = index(&checkpoint.files)?;
    let current_files = index(&current.files)?;
    let uncommitted = uncommitted_paths.into_iter().collect::<BTreeSet<_>>();
    for path in &uncommitted {
        validate_path(path)?;
    }

    let mut all_paths = checkpoint_files.keys().cloned().collect::<BTreeSet<_>>();
    all_paths.extend(current_files.keys().cloned());

    let mut files = Vec::new();
    let mut conflicting_uncommitted_paths = Vec::new();
    for path in all_paths {
        let checkpoint_fingerprint = checkpoint_files.get(&path);
        let current_fingerprint = current_files.get(&path);
        let kind = match (checkpoint_fingerprint, current_fingerprint) {
            (Some(expected), Some(actual)) if expected == actual => continue,
            (Some(_), Some(_)) => FileChangeKind::Modified,
            (Some(_), None) => FileChangeKind::Added,
            (None, Some(_)) => FileChangeKind::Deleted,
            (None, None) => continue,
        };
        let would_overwrite_uncommitted_work = uncommitted.contains(&path);
        if would_overwrite_uncommitted_work {
            conflicting_uncommitted_paths.push(path.clone());
        }
        files.push(RecoveryFileChange {
            path,
            kind,
            would_overwrite_uncommitted_work,
        });
    }

    let repository_matches_checkpoint_base =
        current.repository_fingerprint == checkpoint.repository_fingerprint;
    let mut unresolved_risks = Vec::new();
    if !checkpoint_integrity_verified {
        unresolved_risks.push("Checkpoint integrity has not been verified.".to_owned());
    }
    if !repository_matches_checkpoint_base {
        unresolved_risks.push(
            "The repository no longer matches the checkpoint base; review every affected file."
                .to_owned(),
        );
    }
    if !conflicting_uncommitted_paths.is_empty() {
        unresolved_risks.push(format!(
            "Restore would overwrite uncommitted work in {} file(s).",
            conflicting_uncommitted_paths.len()
        ));
    }

    let repository_preconditions_verified = checkpoint_integrity_verified
        && repository_matches_checkpoint_base
        && conflicting_uncommitted_paths.is_empty();
    let preview = RecoveryPreview {
        checkpoint_id: checkpoint_id.into(),
        files,
        unresolved_risks: unresolved_risks.clone(),
        repository_matches_checkpoint_base,
    };
    let evidence = RecoveryPreflightEvidence {
        repository_fingerprint_before: current.repository_fingerprint.clone(),
        checkpoint_integrity_verified,
        repository_preconditions_verified,
        conflicting_uncommitted_paths,
        unresolved_risks,
    };
    Ok(RecoveryPreflightReport { preview, evidence })
}

pub fn snapshot_fingerprint(
    files: &[RepositoryFileState],
) -> Result<String, RecoveryPreflightError> {
    let indexed = index(files)?;
    let mut hasher = Sha256::new();
    for (path, fingerprint) in indexed {
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(fingerprint.as_bytes());
        hasher.update([0]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn validate_snapshot(snapshot: &RepositorySnapshot) -> Result<(), RecoveryPreflightError> {
    validate_fingerprint(&snapshot.repository_fingerprint)
        .map_err(RecoveryPreflightError::InvalidRepositoryFingerprint)?;
    index(&snapshot.files).map(|_| ())
}

fn index(
    files: &[RepositoryFileState],
) -> Result<BTreeMap<String, String>, RecoveryPreflightError> {
    let mut indexed = BTreeMap::new();
    for file in files {
        validate_path(&file.path)?;
        validate_fingerprint(&file.content_fingerprint).map_err(|fingerprint| {
            RecoveryPreflightError::InvalidContentFingerprint {
                path: file.path.clone(),
                fingerprint,
            }
        })?;
        if indexed
            .insert(file.path.clone(), file.content_fingerprint.clone())
            .is_some()
        {
            return Err(RecoveryPreflightError::DuplicatePath(file.path.clone()));
        }
    }
    Ok(indexed)
}

fn validate_fingerprint(value: &str) -> Result<(), String> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(value.to_owned())
    }
}

fn validate_path(path: &str) -> Result<(), RecoveryPreflightError> {
    let unsafe_path = path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.split(['/', '\\']).any(|component| component == "..")
        || path.contains('\0');
    if unsafe_path {
        Err(RecoveryPreflightError::UnsafePath(path.to_owned()))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(value: u8) -> String {
        format!("{value:02x}").repeat(32)
    }

    fn snapshot(repository: u8, files: &[(&str, u8)]) -> RepositorySnapshot {
        RepositorySnapshot {
            repository_fingerprint: fp(repository),
            files: files
                .iter()
                .map(|(path, value)| RepositoryFileState {
                    path: (*path).to_owned(),
                    content_fingerprint: fp(*value),
                })
                .collect(),
        }
    }

    #[test]
    fn preview_is_file_level_deterministic_and_non_mutating() {
        let checkpoint = snapshot(1, &[("src/a.rs", 1), ("src/b.rs", 2)]);
        let current = snapshot(1, &[("src/a.rs", 9), ("src/c.rs", 3)]);
        let report =
            build_restore_preflight("cp-1", &checkpoint, &current, ["src/a.rs".to_owned()], true)
                .unwrap();

        assert_eq!(
            report
                .preview
                .files
                .iter()
                .map(|change| change.path.as_str())
                .collect::<Vec<_>>(),
            vec!["src/a.rs", "src/b.rs", "src/c.rs"]
        );
        assert!(report.preview.files[0].would_overwrite_uncommitted_work);
        assert!(!report.evidence.repository_preconditions_verified);
        assert_eq!(report.evidence.conflicting_uncommitted_paths, ["src/a.rs"]);
    }

    #[test]
    fn clean_matching_repository_enables_verified_preconditions() {
        let checkpoint = snapshot(1, &[("src/lib.rs", 1)]);
        let current = checkpoint.clone();
        let report = build_restore_preflight("cp-1", &checkpoint, &current, [], true).unwrap();
        assert!(report.preview.files.is_empty());
        assert!(report.evidence.repository_preconditions_verified);
        assert!(report.preview.unresolved_risks.is_empty());
    }

    #[test]
    fn stale_repository_and_unverified_checkpoint_fail_closed() {
        let checkpoint = snapshot(1, &[("src/lib.rs", 1)]);
        let current = snapshot(2, &[("src/lib.rs", 1)]);
        let report = build_restore_preflight("cp-1", &checkpoint, &current, [], false).unwrap();
        assert!(!report.preview.repository_matches_checkpoint_base);
        assert!(!report.evidence.repository_preconditions_verified);
        assert_eq!(report.preview.unresolved_risks.len(), 2);
    }

    #[test]
    fn unsafe_or_duplicate_paths_are_rejected() {
        let invalid = RepositorySnapshot {
            repository_fingerprint: fp(1),
            files: vec![RepositoryFileState {
                path: "../secret".to_owned(),
                content_fingerprint: fp(2),
            }],
        };
        assert!(matches!(
            build_restore_preflight("cp", &invalid, &snapshot(1, &[]), [], true),
            Err(RecoveryPreflightError::UnsafePath(_))
        ));
    }

    #[test]
    fn snapshot_fingerprint_is_order_independent() {
        let left = vec![
            RepositoryFileState {
                path: "b".into(),
                content_fingerprint: fp(2),
            },
            RepositoryFileState {
                path: "a".into(),
                content_fingerprint: fp(1),
            },
        ];
        let right = vec![left[1].clone(), left[0].clone()];
        assert_eq!(
            snapshot_fingerprint(&left).unwrap(),
            snapshot_fingerprint(&right).unwrap()
        );
    }
}
