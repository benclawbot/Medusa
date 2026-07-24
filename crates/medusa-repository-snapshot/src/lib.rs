//! Deterministic repository snapshots and replay divergence detection.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SnapshotFile {
    pub path: String,
    pub content_fingerprint: String,
    pub byte_len: u64,
}

impl SnapshotFile {
    pub fn from_bytes(path: impl Into<String>, bytes: &[u8]) -> Result<Self, &'static str> {
        let path = path.into();
        validate_path(&path)?;
        Ok(Self {
            path,
            content_fingerprint: fingerprint_bytes(bytes),
            byte_len: bytes.len() as u64,
        })
    }

    fn validate(&self) -> Result<(), &'static str> {
        validate_path(&self.path)?;
        validate_digest(&self.content_fingerprint)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RepositorySnapshot {
    pub files: Vec<SnapshotFile>,
    pub parent_snapshot: Option<String>,
    pub fingerprint: String,
}

impl RepositorySnapshot {
    pub fn capture(
        files: impl IntoIterator<Item = SnapshotFile>,
        parent_snapshot: Option<String>,
    ) -> Result<Self, &'static str> {
        if let Some(parent) = &parent_snapshot {
            validate_digest(parent)?;
        }
        let files = canonical_files(files)?;
        let fingerprint = fingerprint(&(&files, &parent_snapshot));
        Ok(Self { files, parent_snapshot, fingerprint })
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        let rebuilt = Self::capture(self.files.clone(), self.parent_snapshot.clone())?;
        if rebuilt.fingerprint != self.fingerprint {
            return Err("snapshot fingerprint does not match its contents");
        }
        Ok(())
    }

    pub fn changed_paths(&self, previous: &Self) -> Result<Vec<String>, &'static str> {
        self.validate()?;
        previous.validate()?;
        let current = self.files.iter().map(|f| (f.path.as_str(), f)).collect::<BTreeMap<_, _>>();
        let old = previous.files.iter().map(|f| (f.path.as_str(), f)).collect::<BTreeMap<_, _>>();
        let mut paths = current.keys().chain(old.keys()).copied().collect::<Vec<_>>();
        paths.sort_unstable();
        paths.dedup();
        Ok(paths
            .into_iter()
            .filter(|path| current.get(path).map(|f| f.content_fingerprint.as_str())
                != old.get(path).map(|f| f.content_fingerprint.as_str()))
            .map(str::to_owned)
            .collect())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionManifest {
    pub execution_id: String,
    pub snapshot_fingerprint: String,
    pub prompt_fingerprint: String,
    pub context_fingerprint: String,
    pub memory_fingerprint: String,
    pub tool_output_fingerprints: Vec<String>,
    pub transaction_fingerprints: Vec<String>,
    pub final_result_fingerprint: String,
    pub fingerprint: String,
}

impl ExecutionManifest {
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        execution_id: impl Into<String>,
        snapshot_fingerprint: impl Into<String>,
        prompt_fingerprint: impl Into<String>,
        context_fingerprint: impl Into<String>,
        memory_fingerprint: impl Into<String>,
        mut tool_output_fingerprints: Vec<String>,
        mut transaction_fingerprints: Vec<String>,
        final_result_fingerprint: impl Into<String>,
    ) -> Result<Self, &'static str> {
        let execution_id = execution_id.into();
        if execution_id.trim().is_empty() {
            return Err("execution identifier cannot be empty");
        }
        let snapshot_fingerprint = snapshot_fingerprint.into();
        let prompt_fingerprint = prompt_fingerprint.into();
        let context_fingerprint = context_fingerprint.into();
        let memory_fingerprint = memory_fingerprint.into();
        let final_result_fingerprint = final_result_fingerprint.into();
        for digest in [
            &snapshot_fingerprint,
            &prompt_fingerprint,
            &context_fingerprint,
            &memory_fingerprint,
            &final_result_fingerprint,
        ] {
            validate_digest(digest)?;
        }
        for digest in tool_output_fingerprints.iter().chain(transaction_fingerprints.iter()) {
            validate_digest(digest)?;
        }
        tool_output_fingerprints.sort();
        transaction_fingerprints.sort();
        let fingerprint = fingerprint(&(
            execution_id.as_str(),
            snapshot_fingerprint.as_str(),
            prompt_fingerprint.as_str(),
            context_fingerprint.as_str(),
            memory_fingerprint.as_str(),
            &tool_output_fingerprints,
            &transaction_fingerprints,
            final_result_fingerprint.as_str(),
        ));
        Ok(Self {
            execution_id,
            snapshot_fingerprint,
            prompt_fingerprint,
            context_fingerprint,
            memory_fingerprint,
            tool_output_fingerprints,
            transaction_fingerprints,
            final_result_fingerprint,
            fingerprint,
        })
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        let rebuilt = Self::record(
            self.execution_id.clone(),
            self.snapshot_fingerprint.clone(),
            self.prompt_fingerprint.clone(),
            self.context_fingerprint.clone(),
            self.memory_fingerprint.clone(),
            self.tool_output_fingerprints.clone(),
            self.transaction_fingerprints.clone(),
            self.final_result_fingerprint.clone(),
        )?;
        if rebuilt.fingerprint != self.fingerprint {
            return Err("execution manifest fingerprint does not match its contents");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ReplayDivergenceKind {
    Snapshot,
    Prompt,
    Context,
    Memory,
    ToolOutputs,
    Transactions,
    FinalResult,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplayDivergence {
    pub kind: ReplayDivergenceKind,
    pub expected: String,
    pub actual: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplayReport {
    pub reproducible: bool,
    pub divergences: Vec<ReplayDivergence>,
    pub fingerprint: String,
}

pub fn compare_replay(
    expected: &ExecutionManifest,
    actual: &ExecutionManifest,
) -> Result<ReplayReport, &'static str> {
    expected.validate()?;
    actual.validate()?;
    let mut divergences = Vec::new();
    compare_field(&mut divergences, ReplayDivergenceKind::Snapshot, &expected.snapshot_fingerprint, &actual.snapshot_fingerprint);
    compare_field(&mut divergences, ReplayDivergenceKind::Prompt, &expected.prompt_fingerprint, &actual.prompt_fingerprint);
    compare_field(&mut divergences, ReplayDivergenceKind::Context, &expected.context_fingerprint, &actual.context_fingerprint);
    compare_field(&mut divergences, ReplayDivergenceKind::Memory, &expected.memory_fingerprint, &actual.memory_fingerprint);
    compare_field(
        &mut divergences,
        ReplayDivergenceKind::ToolOutputs,
        &fingerprint(&expected.tool_output_fingerprints),
        &fingerprint(&actual.tool_output_fingerprints),
    );
    compare_field(
        &mut divergences,
        ReplayDivergenceKind::Transactions,
        &fingerprint(&expected.transaction_fingerprints),
        &fingerprint(&actual.transaction_fingerprints),
    );
    compare_field(&mut divergences, ReplayDivergenceKind::FinalResult, &expected.final_result_fingerprint, &actual.final_result_fingerprint);
    let reproducible = divergences.is_empty();
    let report_fingerprint = fingerprint(&(expected.fingerprint.as_str(), actual.fingerprint.as_str(), &divergences));
    Ok(ReplayReport { reproducible, divergences, fingerprint: report_fingerprint })
}

fn compare_field(
    divergences: &mut Vec<ReplayDivergence>,
    kind: ReplayDivergenceKind,
    expected: &str,
    actual: &str,
) {
    if expected != actual {
        divergences.push(ReplayDivergence {
            kind,
            expected: expected.to_owned(),
            actual: actual.to_owned(),
        });
    }
}

fn canonical_files(files: impl IntoIterator<Item = SnapshotFile>) -> Result<Vec<SnapshotFile>, &'static str> {
    let mut by_path = BTreeMap::new();
    for file in files {
        file.validate()?;
        if by_path.insert(file.path.clone(), file).is_some() {
            return Err("snapshot paths must be unique");
        }
    }
    Ok(by_path.into_values().collect())
}

fn validate_path(path: &str) -> Result<(), &'static str> {
    if path.trim().is_empty() || path.starts_with('/') || path.split('/').any(|part| part == "..") {
        return Err("snapshot paths must be non-empty workspace-relative paths");
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), &'static str> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("fingerprint must be a SHA-256 hex digest");
    }
    Ok(())
}

fn fingerprint<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).expect("serializing deterministic snapshot data cannot fail");
    fingerprint_bytes(&bytes)
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

    fn manifest(result: &[u8]) -> ExecutionManifest {
        ExecutionManifest::record(
            "run-1",
            digest(b"snapshot"),
            digest(b"prompt"),
            digest(b"context"),
            digest(b"memory"),
            vec![digest(b"tool-b"), digest(b"tool-a")],
            vec![digest(b"transaction")],
            digest(result),
        ).unwrap()
    }

    #[test]
    fn snapshot_identity_is_independent_of_input_order() {
        let a = SnapshotFile::from_bytes("a.rs", b"a").unwrap();
        let b = SnapshotFile::from_bytes("b.rs", b"b").unwrap();
        let left = RepositorySnapshot::capture([a.clone(), b.clone()], None).unwrap();
        let right = RepositorySnapshot::capture([b, a], None).unwrap();
        assert_eq!(left, right);
    }

    #[test]
    fn changed_paths_include_created_modified_and_deleted_files() {
        let old = RepositorySnapshot::capture([
            SnapshotFile::from_bytes("modified", b"old").unwrap(),
            SnapshotFile::from_bytes("deleted", b"gone").unwrap(),
        ], None).unwrap();
        let new = RepositorySnapshot::capture([
            SnapshotFile::from_bytes("modified", b"new").unwrap(),
            SnapshotFile::from_bytes("created", b"here").unwrap(),
        ], Some(old.fingerprint.clone())).unwrap();
        assert_eq!(new.changed_paths(&old).unwrap(), vec!["created", "deleted", "modified"]);
    }

    #[test]
    fn identical_execution_is_reproducible() {
        let expected = manifest(b"result");
        let actual = manifest(b"result");
        let report = compare_replay(&expected, &actual).unwrap();
        assert!(report.reproducible);
        assert!(report.divergences.is_empty());
    }

    #[test]
    fn changed_result_is_reported_as_divergence() {
        let report = compare_replay(&manifest(b"one"), &manifest(b"two")).unwrap();
        assert!(!report.reproducible);
        assert_eq!(report.divergences.len(), 1);
        assert_eq!(report.divergences[0].kind, ReplayDivergenceKind::FinalResult);
    }

    #[test]
    fn tampered_snapshot_is_rejected() {
        let mut snapshot = RepositorySnapshot::capture([
            SnapshotFile::from_bytes("a", b"a").unwrap(),
        ], None).unwrap();
        snapshot.files[0].byte_len = 99;
        assert!(snapshot.validate().is_err());
    }
}
