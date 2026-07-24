use std::path::{Path, PathBuf};

use medusa_core::MedusaResult;
use medusa_intelligence::{CodeIndex, IndexRefresh, IndexSnapshot};

/// Repository index cache with deterministic invalidation semantics.
#[derive(Debug)]
pub struct RepositoryIndexCache {
    repo: PathBuf,
    snapshot: IndexSnapshot,
    index: CodeIndex,
}

impl RepositoryIndexCache {
    /// Builds an index cache for one repository identity.
    pub fn load(repo: PathBuf) -> MedusaResult<Self> {
        let snapshot = IndexSnapshot::capture(&repo)?;
        let index = CodeIndex::build(&repo)?;
        Ok(Self {
            repo,
            snapshot,
            index,
        })
    }

    #[must_use]
    pub fn repo(&self) -> &Path {
        &self.repo
    }

    #[must_use]
    pub fn index(&self) -> &CodeIndex {
        &self.index
    }

    /// Refreshes changed source files and returns `None` when the snapshot is unchanged.
    pub fn refresh(&mut self) -> MedusaResult<Option<IndexRefresh>> {
        let newer = IndexSnapshot::capture(&self.repo)?;
        let delta = self.snapshot.diff(&newer);
        if delta.is_empty() {
            return Ok(None);
        }
        let refresh = self.index.refresh(&self.repo, &delta)?;
        self.snapshot = newer;
        Ok(Some(refresh))
    }

    /// Switches repositories by replacing the complete snapshot and index.
    pub fn switch_repo(&mut self, repo: PathBuf) -> MedusaResult<()> {
        if repo == self.repo {
            return Ok(());
        }
        *self = Self::load(repo)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn refresh_is_incremental_and_noops_when_unchanged() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("lib.rs"), "fn before() {}\n").expect("source");
        let mut cache = RepositoryIndexCache::load(directory.path().to_path_buf()).expect("cache");
        assert!(cache.refresh().expect("unchanged").is_none());

        fs::write(directory.path().join("lib.rs"), "fn after() {}\n").expect("modify");
        let refresh = cache.refresh().expect("refresh").expect("changed");
        assert_eq!(refresh.reindexed, vec![PathBuf::from("lib.rs")]);
        assert!(cache.index().definitions("before").is_empty());
        assert_eq!(cache.index().definitions("after").len(), 1);
    }

    #[test]
    fn repository_switch_replaces_all_indexed_state() {
        let first = tempfile::tempdir().expect("first");
        let second = tempfile::tempdir().expect("second");
        fs::write(first.path().join("lib.rs"), "fn first_repo() {}\n").expect("first source");
        fs::write(second.path().join("lib.rs"), "fn second_repo() {}\n").expect("second source");

        let mut cache = RepositoryIndexCache::load(first.path().to_path_buf()).expect("cache");
        cache
            .switch_repo(second.path().to_path_buf())
            .expect("switch");

        assert!(cache.index().definitions("first_repo").is_empty());
        assert_eq!(cache.index().definitions("second_repo").len(), 1);
        assert_eq!(cache.repo(), second.path());
    }
}
