//! Deterministic worker read-set recording and stale-input validation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileSnapshot {
    pub path: String,
    pub content_fingerprint: String,
    pub byte_len: u64,
}

impl FileSnapshot {
    pub fn from_bytes(path: impl Into<String>, bytes: &[u8]) -> Result<Self, &'static str> {
        let path = path.into();
        validate_path(&path)?;
        Ok(Self {
            path,
            content_fingerprint: fingerprint_bytes(bytes),
            byte_len: bytes.len() as u64,
        })
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        validate_path(&self.path)?;
        if self.content_fingerprint.len() != 64 || !self.content_fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("file fingerprint must be a SHA-256 hex digest");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkerReadSet {
    pub worker_id: String,
    pub task_id: String,
    pub files: Vec<FileSnapshot>,
    pub fingerprint: String,
}

impl WorkerReadSet {
    pub fn record(
        worker_id: impl Into<String>,
        task_id: impl Into<String>,
        files: impl IntoIterator<Item = FileSnapshot>,
    ) -> Result<Self, &'static str> {
        let worker_id = worker_id.into();
        let task_id = task_id.into();
        if worker_id.trim().is_empty() || task_id.trim().is_empty() {
            return Err("worker and task identifiers cannot be empty");
        }

        let mut by_path = BTreeMap::new();
        for file in files {
            file.validate()?;
            if by_path.insert(file.path.clone(), file).is_some() {
                return Err("read-set file paths must be unique");
            }
        }
        let files = by_path.into_values().collect::<Vec<_>>();
        let fingerprint = fingerprint(&(worker_id.as_str(), task_id.as_str(), &files));
        Ok(Self { worker_id, task_id, files, fingerprint })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StaleRead {
    pub path: String,
    pub expected_fingerprint: String,
    pub actual_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReadSetValidation {
    pub valid: bool,
    pub stale_reads: Vec<StaleRead>,
    pub unchanged_paths: Vec<String>,
    pub validation_fingerprint: String,
}

pub fn validate_read_set(
    read_set: &WorkerReadSet,
    current_files: impl IntoIterator<Item = FileSnapshot>,
) -> Result<ReadSetValidation, &'static str> {
    let rebuilt = WorkerReadSet::record(
        read_set.worker_id.clone(),
        read_set.task_id.clone(),
        read_set.files.clone(),
    )?;
    if rebuilt.fingerprint != read_set.fingerprint {
        return Err("read-set fingerprint does not match its contents");
    }

    let mut current = BTreeMap::new();
    for file in current_files {
        file.validate()?;
        if current.insert(file.path.clone(), file).is_some() {
            return Err("current file paths must be unique");
        }
    }

    let mut stale_reads = Vec::new();
    let mut unchanged_paths = Vec::new();
    for expected in &read_set.files {
        match current.get(&expected.path) {
            Some(actual) if actual.content_fingerprint == expected.content_fingerprint => {
                unchanged_paths.push(expected.path.clone());
            }
            Some(actual) => stale_reads.push(StaleRead {
                path: expected.path.clone(),
                expected_fingerprint: expected.content_fingerprint.clone(),
                actual_fingerprint: Some(actual.content_fingerprint.clone()),
            }),
            None => stale_reads.push(StaleRead {
                path: expected.path.clone(),
                expected_fingerprint: expected.content_fingerprint.clone(),
                actual_fingerprint: None,
            }),
        }
    }

    stale_reads.sort_by(|a, b| a.path.cmp(&b.path));
    unchanged_paths.sort();
    let valid = stale_reads.is_empty();
    let validation_fingerprint = fingerprint(&(read_set.fingerprint.as_str(), &stale_reads, &unchanged_paths));
    Ok(ReadSetValidation { valid, stale_reads, unchanged_paths, validation_fingerprint })
}

pub fn affected_workers<'a>(
    read_sets: impl IntoIterator<Item = &'a WorkerReadSet>,
    changed_paths: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let changed = changed_paths.into_iter().collect::<BTreeSet<_>>();
    let mut workers = BTreeSet::new();
    for read_set in read_sets {
        if read_set.files.iter().any(|file| changed.contains(file.path.as_str())) {
            workers.insert(read_set.worker_id.clone());
        }
    }
    workers.into_iter().collect()
}

fn validate_path(path: &str) -> Result<(), &'static str> {
    if path.trim().is_empty() || path.starts_with('/') || path.split('/').any(|segment| segment == "..") {
        return Err("read-set paths must be non-empty workspace-relative paths");
    }
    Ok(())
}

fn fingerprint<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).expect("serializing read-set data cannot fail");
    fingerprint_bytes(&bytes)
}

fn fingerprint_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_modified_and_deleted_inputs() {
        let read_set = WorkerReadSet::record(
            "worker-a",
            "task-1",
            [
                FileSnapshot::from_bytes("src/lib.rs", b"old").unwrap(),
                FileSnapshot::from_bytes("Cargo.toml", b"manifest").unwrap(),
            ],
        ).unwrap();
        let validation = validate_read_set(
            &read_set,
            [FileSnapshot::from_bytes("src/lib.rs", b"new").unwrap()],
        ).unwrap();
        assert!(!validation.valid);
        assert_eq!(validation.stale_reads.len(), 2);
    }

    #[test]
    fn unchanged_inputs_are_valid() {
        let file = FileSnapshot::from_bytes("src/lib.rs", b"same").unwrap();
        let read_set = WorkerReadSet::record("worker-a", "task-1", [file.clone()]).unwrap();
        let validation = validate_read_set(&read_set, [file]).unwrap();
        assert!(validation.valid);
        assert_eq!(validation.unchanged_paths, vec!["src/lib.rs"]);
    }

    #[test]
    fn ordering_does_not_change_fingerprint() {
        let a = FileSnapshot::from_bytes("a", b"a").unwrap();
        let b = FileSnapshot::from_bytes("b", b"b").unwrap();
        let left = WorkerReadSet::record("w", "t", [a.clone(), b.clone()]).unwrap();
        let right = WorkerReadSet::record("w", "t", [b, a]).unwrap();
        assert_eq!(left, right);
    }

    #[test]
    fn identifies_only_workers_affected_by_changed_paths() {
        let a = WorkerReadSet::record("a", "t1", [FileSnapshot::from_bytes("a.rs", b"a").unwrap()]).unwrap();
        let b = WorkerReadSet::record("b", "t2", [FileSnapshot::from_bytes("b.rs", b"b").unwrap()]).unwrap();
        assert_eq!(affected_workers([&a, &b], ["b.rs"]), vec!["b"]);
    }
}
