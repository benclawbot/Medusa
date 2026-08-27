use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_intelligence::{
    CodeIndex, IndexRefresh, IndexSnapshot, RepositoryGraph,
};

use crate::{
    repository_boundary::is_revisioned_git_repository_root,
    session_browser::RepositoryIndexCache,
};

#[derive(Debug)]
struct CachedRepository {
    git_identity: Vec<u8>,
    source_paths: BTreeSet<PathBuf>,
    graph: Option<RepositoryGraph>,
    legacy_cache: Option<RepositoryIndexCache>,
}

impl CachedRepository {
    fn load(repo: PathBuf, git_identity: Vec<u8>) -> MedusaResult<Self> {
        if is_revisioned_git_repository_root(&repo) {
            let graph = RepositoryGraph::open(&repo)?;
            let source_paths = graph.snapshot().files.keys().cloned().collect();
            return Ok(Self {
                git_identity,
                source_paths,
                graph: Some(graph),
                legacy_cache: None,
            });
        }

        let snapshot = IndexSnapshot::capture(&repo)?;
        Ok(Self {
            git_identity,
            source_paths: snapshot.files.into_keys().collect(),
            graph: None,
            legacy_cache: Some(RepositoryIndexCache::load(repo)?),
        })
    }

    fn index(&self) -> MedusaResult<&CodeIndex> {
        if let Some(graph) = &self.graph {
            return Ok(&graph.snapshot().index);
        }
        self.legacy_cache
            .as_ref()
            .map(RepositoryIndexCache::index)
            .ok_or_else(|| {
                MedusaError::new(
                    ErrorCode::InternalInvariant,
                    ErrorCategory::Internal,
                    "repository retrieval cache has no active index",
                )
            })
    }

    fn parse_errors(&self) -> MedusaResult<Vec<PathBuf>> {
        Ok(self.index()?.parse_errors.clone())
    }

    fn refresh_incremental(&mut self) -> MedusaResult<Option<IndexRefresh>> {
        if let Some(graph) = &mut self.graph {
            let changed = graph.refresh()?;
            if changed.is_empty() {
                return Ok(None);
            }
            let source_paths = graph.snapshot().files.keys().cloned().collect::<BTreeSet<_>>();
            let removed = self
                .source_paths
                .difference(&source_paths)
                .cloned()
                .collect::<Vec<_>>();
            let reindexed = changed
                .into_iter()
                .filter(|path| source_paths.contains(path))
                .collect::<Vec<_>>();
            self.source_paths = source_paths;
            return Ok(Some(IndexRefresh {
                reindexed,
                removed,
                parse_errors: graph.snapshot().index.parse_errors.clone(),
            }));
        }

        let cache = self.legacy_cache.as_mut().ok_or_else(|| {
            MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                "repository retrieval cache has no refresh adapter",
            )
        })?;
        let refresh = cache.refresh()?;
        if refresh.is_some() {
            self.source_paths = IndexSnapshot::capture(cache.repo())?.files.into_keys().collect();
        }
        Ok(refresh)
    }

}

static REPOSITORY_INDEXES: OnceLock<Mutex<BTreeMap<PathBuf, CachedRepository>>> = OnceLock::new();

fn indexes() -> MedusaResult<MutexGuard<'static, BTreeMap<PathBuf, CachedRepository>>> {
    REPOSITORY_INDEXES
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| {
            MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                "repository index cache lock was poisoned",
            )
        })
}

/// Refreshes the process-wide repository evidence before a model turn.
///
/// Real Git repositories use the persistent revision-bound repository graph. Non-Git fixtures and
/// repositories retain the legacy in-memory index as an explicit compatibility fallback.
pub(crate) fn refresh(repo: &Path) -> MedusaResult<Option<IndexRefresh>> {
    let mut indexes = indexes()?;
    let key = repo.to_path_buf();
    let identity = git_identity(repo)?;

    if let Some(entry) = indexes.get_mut(&key) {
        if entry.git_identity != identity {
            let previous_paths = entry.source_paths.clone();
            let replacement = CachedRepository::load(key, identity)?;
            let removed = previous_paths
                .difference(&replacement.source_paths)
                .cloned()
                .collect::<Vec<_>>();
            let refresh = IndexRefresh {
                reindexed: replacement.source_paths.iter().cloned().collect(),
                removed,
                parse_errors: replacement.parse_errors()?,
            };
            *entry = replacement;
            return Ok(Some(refresh));
        }

        return entry.refresh_incremental();
    }

    indexes.insert(key.clone(), CachedRepository::load(key, identity)?);
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
    use std::{fs, process::Command};

    use super::*;

    fn git_ok(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .expect("git command");
        assert!(status.success(), "git {args:?} failed");
    }

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
    fn ancestor_git_worktree_is_not_treated_as_repository_root() {
        let parent = tempfile::tempdir().expect("parent repository");
        git_ok(parent.path(), &["init", "-q"]);
        let nested = parent.path().join("nested");
        fs::create_dir(&nested).expect("nested directory");

        assert!(!is_revisioned_git_repository_root(&nested));
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
