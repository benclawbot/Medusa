use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::policy::safe_path;

#[path = "mutation_provenance.rs"]
mod mutation_provenance;
pub use mutation_provenance::{MutationContext, MutationKind, ScopeValidation};
use mutation_provenance::{build_record, load as load_provenance, persist as persist_provenance};

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
pub fn apply_atomic(repo: &Path, mutations: &[FileMutation]) -> MedusaResult<TransactionOutcome> {
    apply_atomic_inner(repo, mutations, None)
}

/// Applies every repository mutation through the rollback-capable boundary and atomically records
/// authoritative mutation provenance. If provenance persistence fails, all committed file writes
/// are rolled back before returning an error.
pub fn apply_atomic_with_context(
    repo: &Path,
    mutations: &[FileMutation],
    context: &MutationContext,
) -> MedusaResult<TransactionOutcome> {
    apply_atomic_inner(repo, mutations, Some(context))
}

fn apply_atomic_inner(
    repo: &Path,
    mutations: &[FileMutation],
    context: Option<&MutationContext>,
) -> MedusaResult<TransactionOutcome> {
    if mutations.is_empty() {
        return Err(MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            "transaction must contain at least one file mutation",
        ));
    }

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

    let mut mutation_ids = Vec::new();
    if let Some(context) = context {
        let repository_before = repository_before.ok_or_else(|| {
            provenance_boundary_error("authoritative mutation fingerprint is unavailable")
        })?;
        let repository_after = repository_fingerprint(repo)?;
        let mut journal = match load_provenance(repo) {
            Ok(journal) => journal,
            Err(error) => {
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
                let rollback = rollback(&backups);
                return Err(MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Execution,
                    format!("mutation provenance conflict; rollback={rollback}: {error}"),
                ));
            }
        }
        if let Err(error) = persist_provenance(repo, &journal) {
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

pub fn preview_selective_revert(repo: &Path, mutation_id: &str) -> MedusaResult<RevertPreview> {
    let journal = load_provenance(repo)?;
    let record = journal
        .records
        .iter()
        .find(|record| record.id == mutation_id)
        .ok_or_else(|| provenance_boundary_error("mutation provenance record is missing"))?;
    let current = fs::read(safe_path(repo, &record.path)?)?;
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
    let preview = preview_selective_revert(repo, mutation_id)?;
    let journal = load_provenance(repo)?;
    let record = journal
        .records
        .iter()
        .find(|record| record.id == mutation_id)
        .ok_or_else(|| provenance_boundary_error("mutation provenance record disappeared"))?;
    let path = safe_path(repo, &preview.path)?;
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
    apply_atomic_with_context(
        repo,
        &[FileMutation {
            path: preview.path,
            content,
        }],
        context,
    )
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
    let output = std::process::Command::new("git")
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

    #[test]
    fn commits_multiple_files() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::process::Command::new("git")
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
        std::process::Command::new("git")
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
    fn overlapping_user_edit_rejects_selective_revert() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::process::Command::new("git")
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
}
