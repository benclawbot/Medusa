use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};

use crate::policy::safe_path;

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

/// Applies every repository file mutation through one symlink-aware, rollback-capable boundary.
///
/// All targets are resolved and validated before parent directories or temporary files are
/// created. This prevents a later invalid mutation from leaving earlier staging artifacts behind.
pub fn apply_atomic(repo: &Path, mutations: &[FileMutation]) -> MedusaResult<TransactionOutcome> {
    if mutations.is_empty() {
        return Err(MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            "transaction must contain at least one file mutation",
        ));
    }

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
        let temporary = target.with_extension(format!("medusa-txn-{index}.tmp"));
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

    for (index, (target, temporary)) in staged.iter().enumerate() {
        if let Err(error) = fs::rename(temporary, target) {
            let rollback = rollback(&backups[..index]);
            cleanup_staged(&staged[index..]);
            return Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Execution,
                format!("transaction commit failed: {error}; rollback={rollback}"),
            ));
        }
    }

    Ok(TransactionOutcome {
        affected_files: mutations
            .iter()
            .map(|mutation| mutation.path.clone())
            .collect(),
        rolled_back: false,
        detail: "all file mutations committed through the repository boundary".to_owned(),
    })
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
    use super::*;

    #[test]
    fn commits_multiple_files() {
        let directory = tempfile::tempdir().expect("tempdir");
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
        assert_eq!(fs::read_to_string(directory.path().join("a.txt")).unwrap(), "a");
        assert_eq!(fs::read_to_string(directory.path().join("nested/b.txt")).unwrap(), "b");
    }

    #[test]
    fn rejects_escape_before_any_write() {
        let directory = tempfile::tempdir().expect("tempdir");
        let result = apply_atomic(
            directory.path(),
            &[
                FileMutation { path: "safe.txt".into(), content: "safe".into() },
                FileMutation { path: "../escape.txt".into(), content: "bad".into() },
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
                FileMutation { path: "same.txt".into(), content: "first".into() },
                FileMutation { path: "same.txt".into(), content: "second".into() },
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
            &[FileMutation { path: "linked/escape.txt".into(), content: "bad".into() }],
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
            &[FileMutation { path: "script.sh".into(), content: "new".into() }],
        )
        .expect("transaction");
        assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o750);
    }
}

#[path = "transaction_pipeline.rs"]
mod safety_pipeline;
pub use safety_pipeline::{
    SafeTransactionOutcome, TransactionEvidence, WorkerMutationProposal, execute_safe_transaction,
};
