//! Transactional worker patch planning gated by deterministic read-set validation.

use std::collections::{BTreeMap, BTreeSet};

use medusa_worker_read_set::{validate_read_set, FileSnapshot, ReadSetValidation, WorkerReadSet};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchOperation {
    Create,
    Replace,
    Delete,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FilePatch {
    pub path: String,
    pub operation: PatchOperation,
    pub expected_before_fingerprint: Option<String>,
    pub after_content: Option<Vec<u8>>,
}

impl FilePatch {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_path(&self.path)?;
        match self.operation {
            PatchOperation::Create if self.expected_before_fingerprint.is_some() || self.after_content.is_none() => {
                Err("create patches require content and no before fingerprint")
            }
            PatchOperation::Replace if self.expected_before_fingerprint.is_none() || self.after_content.is_none() => {
                Err("replace patches require content and a before fingerprint")
            }
            PatchOperation::Delete if self.expected_before_fingerprint.is_none() || self.after_content.is_some() => {
                Err("delete patches require a before fingerprint and no content")
            }
            _ => {
                if let Some(fingerprint) = &self.expected_before_fingerprint {
                    validate_fingerprint(fingerprint)?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PatchTransaction {
    pub transaction_id: String,
    pub worker_id: String,
    pub task_id: String,
    pub read_set_fingerprint: String,
    pub patches: Vec<FilePatch>,
    pub fingerprint: String,
}

impl PatchTransaction {
    pub fn plan(
        transaction_id: impl Into<String>,
        read_set: &WorkerReadSet,
        patches: impl IntoIterator<Item = FilePatch>,
    ) -> Result<Self, &'static str> {
        let transaction_id = transaction_id.into();
        if transaction_id.trim().is_empty() {
            return Err("transaction id cannot be empty");
        }
        let mut by_path = BTreeMap::new();
        for patch in patches {
            patch.validate()?;
            if by_path.insert(patch.path.clone(), patch).is_some() {
                return Err("transaction patch paths must be unique");
            }
        }
        if by_path.is_empty() {
            return Err("transaction must contain at least one patch");
        }
        let patches = by_path.into_values().collect::<Vec<_>>();
        let fingerprint = fingerprint(&(
            transaction_id.as_str(),
            read_set.worker_id.as_str(),
            read_set.task_id.as_str(),
            read_set.fingerprint.as_str(),
            &patches,
        ));
        Ok(Self {
            transaction_id,
            worker_id: read_set.worker_id.clone(),
            task_id: read_set.task_id.clone(),
            read_set_fingerprint: read_set.fingerprint.clone(),
            patches,
            fingerprint,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionDecision {
    Commit,
    AbortStaleReadSet,
    AbortPatchConflict,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransactionValidation {
    pub decision: TransactionDecision,
    pub read_set_validation: ReadSetValidation,
    pub conflicting_paths: Vec<String>,
    pub validation_fingerprint: String,
}

pub fn validate_transaction(
    transaction: &PatchTransaction,
    read_set: &WorkerReadSet,
    current_files: impl IntoIterator<Item = FileSnapshot>,
) -> Result<TransactionValidation, &'static str> {
    let rebuilt = PatchTransaction::plan(
        transaction.transaction_id.clone(),
        read_set,
        transaction.patches.clone(),
    )?;
    if rebuilt != *transaction {
        return Err("transaction fingerprint does not match its contents");
    }
    if transaction.read_set_fingerprint != read_set.fingerprint {
        return Err("transaction references a different read-set");
    }

    let current_files = current_files.into_iter().collect::<Vec<_>>();
    let read_set_validation = validate_read_set(read_set, current_files.clone())?;
    let current = current_files
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect::<BTreeMap<_, _>>();

    let mut conflicting_paths = Vec::new();
    for patch in &transaction.patches {
        let actual = current.get(&patch.path).map(|file| file.content_fingerprint.as_str());
        let conflict = match patch.operation {
            PatchOperation::Create => actual.is_some(),
            PatchOperation::Replace | PatchOperation::Delete => {
                actual != patch.expected_before_fingerprint.as_deref()
            }
        };
        if conflict {
            conflicting_paths.push(patch.path.clone());
        }
    }
    conflicting_paths.sort();

    let decision = if !read_set_validation.valid {
        TransactionDecision::AbortStaleReadSet
    } else if !conflicting_paths.is_empty() {
        TransactionDecision::AbortPatchConflict
    } else {
        TransactionDecision::Commit
    };
    let validation_fingerprint = fingerprint(&(
        transaction.fingerprint.as_str(),
        &read_set_validation,
        &conflicting_paths,
        decision,
    ));
    Ok(TransactionValidation {
        decision,
        read_set_validation,
        conflicting_paths,
        validation_fingerprint,
    })
}

pub fn conflicting_transactions<'a>(
    transactions: impl IntoIterator<Item = &'a PatchTransaction>,
) -> Vec<(String, String, Vec<String>)> {
    let ordered = transactions.into_iter().collect::<Vec<_>>();
    let mut conflicts = Vec::new();
    for (index, left) in ordered.iter().enumerate() {
        let left_paths = left.patches.iter().map(|patch| patch.path.as_str()).collect::<BTreeSet<_>>();
        for right in ordered.iter().skip(index + 1) {
            let mut paths = right
                .patches
                .iter()
                .map(|patch| patch.path.as_str())
                .filter(|path| left_paths.contains(path))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            paths.sort();
            if !paths.is_empty() {
                let mut ids = [left.transaction_id.clone(), right.transaction_id.clone()];
                ids.sort();
                conflicts.push((ids[0].clone(), ids[1].clone(), paths));
            }
        }
    }
    conflicts.sort();
    conflicts.dedup();
    conflicts
}

fn validate_path(path: &str) -> Result<(), &'static str> {
    if path.trim().is_empty() || path.starts_with('/') || path.split('/').any(|segment| segment == "..") {
        return Err("patch paths must be non-empty workspace-relative paths");
    }
    Ok(())
}

fn validate_fingerprint(value: &str) -> Result<(), &'static str> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("patch fingerprints must be SHA-256 hex digests");
    }
    Ok(())
}

fn fingerprint<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).expect("serializing transaction data cannot fail");
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (WorkerReadSet, FileSnapshot) {
        let file = FileSnapshot::from_bytes("src/lib.rs", b"old").unwrap();
        let read_set = WorkerReadSet::record("worker", "task", [file.clone()]).unwrap();
        (read_set, file)
    }

    #[test]
    fn commits_when_read_set_and_patch_preconditions_match() {
        let (read_set, file) = setup();
        let transaction = PatchTransaction::plan("tx-1", &read_set, [FilePatch {
            path: file.path.clone(),
            operation: PatchOperation::Replace,
            expected_before_fingerprint: Some(file.content_fingerprint.clone()),
            after_content: Some(b"new".to_vec()),
        }]).unwrap();
        let validation = validate_transaction(&transaction, &read_set, [file]).unwrap();
        assert_eq!(validation.decision, TransactionDecision::Commit);
    }

    #[test]
    fn aborts_entire_transaction_for_stale_read() {
        let (read_set, file) = setup();
        let transaction = PatchTransaction::plan("tx-1", &read_set, [FilePatch {
            path: file.path,
            operation: PatchOperation::Replace,
            expected_before_fingerprint: Some(file.content_fingerprint),
            after_content: Some(b"new".to_vec()),
        }]).unwrap();
        let changed = FileSnapshot::from_bytes("src/lib.rs", b"changed").unwrap();
        let validation = validate_transaction(&transaction, &read_set, [changed]).unwrap();
        assert_eq!(validation.decision, TransactionDecision::AbortStaleReadSet);
    }

    #[test]
    fn reports_overlapping_transaction_paths() {
        let (read_set, file) = setup();
        let patch = FilePatch {
            path: file.path,
            operation: PatchOperation::Replace,
            expected_before_fingerprint: Some(file.content_fingerprint),
            after_content: Some(b"new".to_vec()),
        };
        let first = PatchTransaction::plan("a", &read_set, [patch.clone()]).unwrap();
        let second = PatchTransaction::plan("b", &read_set, [patch]).unwrap();
        assert_eq!(conflicting_transactions([&first, &second])[0].2, vec!["src/lib.rs"]);
    }

    #[test]
    fn patch_order_does_not_change_transaction_identity() {
        let (read_set, file) = setup();
        let a = FilePatch {
            path: file.path,
            operation: PatchOperation::Replace,
            expected_before_fingerprint: Some(file.content_fingerprint),
            after_content: Some(b"new".to_vec()),
        };
        let b = FilePatch {
            path: "README.md".into(),
            operation: PatchOperation::Create,
            expected_before_fingerprint: None,
            after_content: Some(b"readme".to_vec()),
        };
        let left = PatchTransaction::plan("tx", &read_set, [a.clone(), b.clone()]).unwrap();
        let right = PatchTransaction::plan("tx", &read_set, [b, a]).unwrap();
        assert_eq!(left, right);
    }
}
