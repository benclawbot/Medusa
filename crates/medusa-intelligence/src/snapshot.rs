use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::support::{relative, source_files};

/// Stable fingerprint of one indexed source file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileFingerprint {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}

/// Deterministic repository snapshot used to invalidate stale index entries.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct IndexSnapshot {
    pub files: BTreeMap<PathBuf, FileFingerprint>,
}

/// Source paths changed between two repository snapshots.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SnapshotDelta {
    pub added: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
}

impl SnapshotDelta {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.removed.is_empty()
    }

    #[must_use]
    pub fn invalidated_paths(&self) -> Vec<PathBuf> {
        let mut paths = self
            .added
            .iter()
            .chain(&self.modified)
            .chain(&self.removed)
            .cloned()
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
    }
}

impl IndexSnapshot {
    pub fn capture(repo: &Path) -> std::io::Result<Self> {
        let mut files = BTreeMap::new();
        for path in source_files(repo) {
            let bytes = fs::read(&path)?;
            let relative_path = relative(repo, &path);
            files.insert(
                relative_path.clone(),
                FileFingerprint {
                    path: relative_path,
                    bytes: bytes.len() as u64,
                    sha256: format!("{:x}", Sha256::digest(&bytes)),
                },
            );
        }
        Ok(Self { files })
    }

    #[must_use]
    pub fn diff(&self, newer: &Self) -> SnapshotDelta {
        let mut delta = SnapshotDelta::default();
        for (path, current) in &newer.files {
            match self.files.get(path) {
                None => delta.added.push(path.clone()),
                Some(previous) if previous != current => delta.modified.push(path.clone()),
                Some(_) => {}
            }
        }
        for path in self.files.keys() {
            if !newer.files.contains_key(path) {
                delta.removed.push(path.clone());
            }
        }
        delta
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn snapshot_diff_reports_added_modified_and_removed_paths() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("a.rs"), "fn a() {}\n").expect("a");
        fs::write(directory.path().join("b.rs"), "fn b() {}\n").expect("b");
        let before = IndexSnapshot::capture(directory.path()).expect("before");

        fs::write(
            directory.path().join("a.rs"),
            "fn a() { println!(\"changed\"); }\n",
        )
        .expect("modify a");
        fs::remove_file(directory.path().join("b.rs")).expect("remove b");
        fs::write(directory.path().join("c.rs"), "fn c() {}\n").expect("add c");
        let after = IndexSnapshot::capture(directory.path()).expect("after");

        let delta = before.diff(&after);
        assert_eq!(delta.added, vec![PathBuf::from("c.rs")]);
        assert_eq!(delta.modified, vec![PathBuf::from("a.rs")]);
        assert_eq!(delta.removed, vec![PathBuf::from("b.rs")]);
        assert_eq!(
            delta.invalidated_paths(),
            vec![
                PathBuf::from("a.rs"),
                PathBuf::from("b.rs"),
                PathBuf::from("c.rs")
            ]
        );
    }

    #[test]
    fn ignored_build_and_metadata_paths_do_not_invalidate() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(directory.path().join("target")).expect("target");
        fs::create_dir_all(directory.path().join(".medusa")).expect("medusa");
        fs::write(directory.path().join("lib.rs"), "fn live() {}\n").expect("lib");
        fs::write(
            directory.path().join("target/generated.rs"),
            "fn generated() {}\n",
        )
        .expect("generated");
        fs::write(
            directory.path().join(".medusa/cache.rs"),
            "fn cached() {}\n",
        )
        .expect("cache");

        let snapshot = IndexSnapshot::capture(directory.path()).expect("snapshot");
        assert_eq!(
            snapshot.files.keys().cloned().collect::<Vec<_>>(),
            vec![PathBuf::from("lib.rs")]
        );
    }
}
