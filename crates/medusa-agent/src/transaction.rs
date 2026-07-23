use std::{fs, path::{Path, PathBuf}};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};

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

pub fn preview(mutations: &[FileMutation], checkpoint: &str, test_plan: Vec<String>) -> TransactionPreview {
    TransactionPreview {
        affected_files: mutations.iter().map(|mutation| mutation.path.clone()).collect(),
        risk: if mutations.len() > 1 { "multi_file_write" } else { "single_file_write" }.to_owned(),
        test_plan,
        rollback_checkpoint: checkpoint.to_owned(),
    }
}

pub fn apply_atomic(repo: &Path, mutations: &[FileMutation]) -> MedusaResult<TransactionOutcome> {
    if mutations.is_empty() {
        return Err(MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            "transaction must contain at least one file mutation",
        ));
    }

    let mut backups: Vec<(PathBuf, Option<Vec<u8>>)> = Vec::with_capacity(mutations.len());
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(mutations.len());

    for (index, mutation) in mutations.iter().enumerate() {
        let relative = Path::new(&mutation.path);
        if relative.is_absolute() || relative.components().any(|component| matches!(component, std::path::Component::ParentDir)) {
            cleanup_staged(&staged);
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                format!("transaction path escapes repository: {}", mutation.path),
            ));
        }
        let target = repo.join(relative);
        let original = if target.exists() { Some(fs::read(&target)?) } else { None };
        backups.push((target.clone(), original));
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = target.with_extension(format!("medusa-txn-{index}.tmp"));
        if let Err(error) = fs::write(&temporary, mutation.content.as_bytes()) {
            cleanup_staged(&staged);
            return Err(error.into());
        }
        staged.push((target, temporary));
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
        affected_files: mutations.iter().map(|mutation| mutation.path.clone()).collect(),
        rolled_back: false,
        detail: "all file mutations committed atomically".to_owned(),
    })
}

fn rollback(backups: &[(PathBuf, Option<Vec<u8>>)]) -> &'static str {
    for (path, original) in backups.iter().rev() {
        let result = match original {
            Some(content) => fs::write(path, content),
            None => if path.exists() { fs::remove_file(path) } else { Ok(()) },
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
        let outcome = apply_atomic(directory.path(), &[
            FileMutation { path: "a.txt".into(), content: "a".into() },
            FileMutation { path: "nested/b.txt".into(), content: "b".into() },
        ]).expect("transaction");
        assert!(!outcome.rolled_back);
        assert_eq!(fs::read_to_string(directory.path().join("a.txt")).unwrap(), "a");
        assert_eq!(fs::read_to_string(directory.path().join("nested/b.txt")).unwrap(), "b");
    }

    #[test]
    fn rejects_escape_before_any_write() {
        let directory = tempfile::tempdir().expect("tempdir");
        let result = apply_atomic(directory.path(), &[
            FileMutation { path: "safe.txt".into(), content: "safe".into() },
            FileMutation { path: "../escape.txt".into(), content: "bad".into() },
        ]);
        assert!(result.is_err());
        assert!(!directory.path().join("safe.txt").exists());
    }
}
