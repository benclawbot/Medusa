use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_intelligence::{IndexRefresh, IndexSnapshot};

use crate::session_browser::RepositoryIndexCache;

#[derive(Debug)]
struct CachedRepository {
    git_identity: Vec<u8>,
    source_paths: BTreeSet<PathBuf>,
    cache: RepositoryIndexCache,
}

static REPOSITORY_INDEXES: OnceLock<Mutex<BTreeMap<PathBuf, CachedRepository>>> = OnceLock::new();

/// Refreshes the process-wide index for one repository before a model turn.
///
/// The first call builds the cache and returns `None`. Later calls refresh changed source files
/// incrementally. A branch, fetch, pull, or detached-HEAD transition forces a complete reload even
/// when the repository path is unchanged.
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
    let identity = git_identity(repo)?;

    if let Some(entry) = indexes.get_mut(&key) {
        if entry.git_identity != identity {
            let snapshot = IndexSnapshot::capture(repo)?;
            let source_paths = snapshot.files.keys().cloned().collect::<BTreeSet<_>>();
            let removed = entry
                .source_paths
                .difference(&source_paths)
                .cloned()
                .collect::<Vec<_>>();
            let cache = RepositoryIndexCache::load(key)?;
            let refresh = IndexRefresh {
                reindexed: source_paths.iter().cloned().collect(),
                removed,
                parse_errors: cache.index().parse_errors.clone(),
            };
            *entry = CachedRepository {
                git_identity: identity,
                source_paths,
                cache,
            };
            return Ok(Some(refresh));
        }

        let refresh = entry.cache.refresh()?;
        if refresh.is_some() {
            entry.source_paths = IndexSnapshot::capture(repo)?.files.into_keys().collect();
        }
        return Ok(refresh);
    }

    let snapshot = IndexSnapshot::capture(repo)?;
    indexes.insert(
        key.clone(),
        CachedRepository {
            git_identity: identity,
            source_paths: snapshot.files.into_keys().collect(),
            cache: RepositoryIndexCache::load(key)?,
        },
    );
    Ok(None)
}

fn git_identity(repo: &Path) -> std::io::Result<Vec<u8>> {
    let Some(git_dir) = resolve_git_dir(repo)? else {
        return Ok(Vec::new());
    };
    let head = read_optional(&git_dir.join("HEAD"))?;
    let mut identity = Vec::new();
    append_identity_part(&mut identity, b"HEAD", &head);

    if let Some(reference) = std::str::from_utf8(&head)
        .ok()
        .and_then(|head| head.trim().strip_prefix("ref: "))
    {
        append_identity_part(
            &mut identity,
            b"REF",
            &read_optional(&git_dir.join(reference))?,
        );
    }
    append_identity_part(
        &mut identity,
        b"PACKED_REFS",
        &read_optional(&git_dir.join("packed-refs"))?,
    );
    append_identity_part(
        &mut identity,
        b"FETCH_HEAD",
        &read_optional(&git_dir.join("FETCH_HEAD"))?,
    );
    Ok(identity)
}

fn resolve_git_dir(repo: &Path) -> std::io::Result<Option<PathBuf>> {
    let dot_git = repo.join(".git");
    if dot_git.is_dir() {
        return Ok(Some(dot_git));
    }
    if !dot_git.is_file() {
        return Ok(None);
    }
    let marker = fs::read_to_string(&dot_git)?;
    let Some(path) = marker.trim().strip_prefix("gitdir: ") else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    Ok(Some(if path.is_absolute() {
        path
    } else {
        repo.join(path)
    }))
}

fn read_optional(path: &Path) -> std::io::Result<Vec<u8>> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

fn append_identity_part(identity: &mut Vec<u8>, label: &[u8], value: &[u8]) {
    identity.extend_from_slice(&(label.len() as u64).to_le_bytes());
    identity.extend_from_slice(label);
    identity.extend_from_slice(&(value.len() as u64).to_le_bytes());
    identity.extend_from_slice(value);
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

    #[test]
    fn git_identity_change_forces_complete_reindex() {
        let repository = tempfile::tempdir().expect("repository");
        fs::create_dir(repository.path().join(".git")).expect("git dir");
        fs::write(repository.path().join(".git/HEAD"), "ref: refs/heads/main\n").expect("head");
        fs::create_dir_all(repository.path().join(".git/refs/heads")).expect("refs");
        fs::write(repository.path().join(".git/refs/heads/main"), "first\n").expect("ref");
        fs::write(repository.path().join("lib.rs"), "fn stable() {}\n").expect("source");

        assert!(refresh(repository.path()).expect("initial load").is_none());
        fs::write(repository.path().join(".git/refs/heads/main"), "second\n").expect("new ref");

        let report = refresh(repository.path())
            .expect("identity refresh")
            .expect("forced reindex");
        assert_eq!(report.reindexed, vec![PathBuf::from("lib.rs")]);
        assert!(report.removed.is_empty());
    }

    #[test]
    fn linked_worktree_gitdir_changes_are_detected() {
        let repository = tempfile::tempdir().expect("repository");
        let metadata = tempfile::tempdir().expect("metadata");
        fs::write(
            repository.path().join(".git"),
            format!("gitdir: {}\n", metadata.path().display()),
        )
        .expect("gitdir marker");
        fs::write(metadata.path().join("HEAD"), "detached-one\n").expect("head");
        fs::write(repository.path().join("lib.rs"), "fn stable() {}\n").expect("source");

        assert!(refresh(repository.path()).expect("initial load").is_none());
        fs::write(metadata.path().join("HEAD"), "detached-two\n").expect("new head");

        assert!(refresh(repository.path())
            .expect("identity refresh")
            .is_some());
    }
}
