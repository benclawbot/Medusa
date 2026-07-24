use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};
use walkdir::{DirEntry, WalkDir};

use crate::support::{hash, relative};

const INDEXED_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cs", "go", "h", "hpp", "java", "js", "jsx", "json", "kt", "kts", "md", "py",
    "rs", "sh", "swift", "toml", "ts", "tsx", "yaml", "yml",
];

const IGNORED_DIRECTORIES: &[&str] = &[
    ".git",
    ".medusa",
    ".next",
    ".nuxt",
    ".pytest_cache",
    ".ruff_cache",
    ".terraform",
    ".venv",
    "__pycache__",
    "bower_components",
    "build",
    "coverage",
    "dist",
    "node_modules",
    "out",
    "target",
    "vendor",
];

/// Stable metadata used to determine whether an indexed file changed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileFingerprint {
    pub bytes: u64,
    pub sha256: String,
}

/// Deterministic snapshot of repository files eligible for indexing.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RepositorySnapshot {
    pub files: BTreeMap<PathBuf, FileFingerprint>,
}

/// Paths changed between two repository snapshots.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SnapshotChanges {
    pub added: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
}

impl RepositorySnapshot {
    /// Scans indexable text files in stable path order.
    pub fn scan(repo: &Path) -> MedusaResult<Self> {
        let mut files = BTreeMap::new();
        for entry in WalkDir::new(repo)
            .into_iter()
            .filter_entry(|entry| !ignored_entry(entry))
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.path();
            if !indexed_extension(path) {
                continue;
            }
            let bytes = fs::read(path)?;
            if looks_binary(&bytes) {
                continue;
            }
            files.insert(
                relative(repo, path),
                FileFingerprint {
                    bytes: bytes.len() as u64,
                    sha256: hash(&bytes),
                },
            );
        }
        Ok(Self { files })
    }

    /// Computes deterministic added, modified, and removed path sets.
    #[must_use]
    pub fn changes_since(&self, previous: &Self) -> SnapshotChanges {
        let current = self.files.keys().cloned().collect::<BTreeSet<_>>();
        let prior = previous.files.keys().cloned().collect::<BTreeSet<_>>();
        let added = current.difference(&prior).cloned().collect();
        let removed = prior.difference(&current).cloned().collect();
        let modified = current
            .intersection(&prior)
            .filter(|path| self.files.get(*path) != previous.files.get(*path))
            .cloned()
            .collect();
        SnapshotChanges {
            added,
            modified,
            removed,
        }
    }

    /// Returns indexed files with the requested extension.
    #[must_use]
    pub fn paths_with_extension(&self, extension: &str) -> Vec<PathBuf> {
        self.files
            .keys()
            .filter(|path| path.extension().is_some_and(|value| value == extension))
            .cloned()
            .collect()
    }
}

fn ignored_entry(entry: &DirEntry) -> bool {
    entry.path().components().any(|component| {
        matches!(
            component,
            Component::Normal(name)
                if IGNORED_DIRECTORIES.iter().any(|ignored| name == *ignored)
        )
    })
}

fn indexed_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| INDEXED_EXTENSIONS.binary_search(&extension).is_ok())
}

fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8 * 1024).any(|byte| *byte == 0)
}
