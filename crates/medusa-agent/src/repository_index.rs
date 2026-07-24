use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_intelligence::IndexRefresh;

use crate::session_browser::RepositoryIndexCache;

static REPOSITORY_INDEXES: OnceLock<Mutex<BTreeMap<PathBuf, RepositoryIndexCache>>> =
    OnceLock::new();

/// Refreshes the process-wide index for one repository before a model turn.
///
/// The first call builds the cache and returns `None`; later calls return a report only when
/// source files changed. Repository identities remain isolated in the process-wide map.
pub(crate) fn refresh(repo: &Path) -> MedusaResult<Option<IndexRefresh>> {
    let indexes = REPOSITORY_INDEXES.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut indexes = indexes.lock().map_err(|_| {
        MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            "repository index cache lock was poisoned",
        )
    })?;
    let key = repo.to_path_buf();
    if let Some(cache) = indexes.get_mut(&key) {
        return cache.refresh();
    }
    indexes.insert(key.clone(), RepositoryIndexCache::load(key)?);
    Ok(None)
}

#[must_use]
pub(crate) fn summary(refresh: &IndexRefresh) -> String {
    let reindexed = refresh
        .reindexed
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    let removed = refresh
        .removed
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    let parse_errors = refresh
        .parse_errors
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    format!(
        "Repository index refreshed. Reindexed: [{}]. Removed: [{}]. Parse errors: [{}].",
        reindexed.join(", "),
        removed.join(", "),
        parse_errors.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn process_cache_noops_then_reports_changed_sources() {
        let repository = tempfile::tempdir().expect("repository");
        fs::write(repository.path().join("lib.rs"), "fn before() {}\n").expect("source");

        assert!(refresh(repository.path()).expect("initial load").is_none());
        assert!(refresh(repository.path()).expect("unchanged").is_none());

        fs::write(repository.path().join("lib.rs"), "fn after() {}\n").expect("modify");
        let report = refresh(repository.path()).expect("refresh").expect("changed");
        assert_eq!(report.reindexed, vec![PathBuf::from("lib.rs")]);
        assert!(summary(&report).contains("lib.rs"));
    }
}
