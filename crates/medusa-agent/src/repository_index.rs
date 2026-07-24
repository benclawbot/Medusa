use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_intelligence::{
    IndexRefresh, IndexSnapshot, RetrievalBudget, RetrievalReport,
};

use crate::session_browser::RepositoryIndexCache;

const MAX_RETRIEVAL_TOKENS: u64 = 8_000;
const RETRIEVAL_WRAPPER_RESERVE_TOKENS: u64 = 256;

#[derive(Debug)]
struct CachedRepository {
    git_identity: Vec<u8>,
    source_paths: BTreeSet<PathBuf>,
    cache: RepositoryIndexCache,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RetrievalContext {
    pub system_fragment: String,
    pub status: String,
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

/// Refreshes the process-wide index for one repository before a model turn.
///
/// The first call builds the cache and returns `None`. Later calls refresh changed source files
/// incrementally. A branch, fetch, pull, or detached-HEAD transition forces a complete reload even
/// when the repository path is unchanged.
pub(crate) fn refresh(repo: &Path) -> MedusaResult<Option<IndexRefresh>> {
    let mut indexes = indexes()?;
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

/// Retrieves repository context within capacity left after protected prompt allocations.
pub(crate) fn retrieve_context(
    repo: &Path,
    query: &str,
    available_tokens: u64,
) -> MedusaResult<Option<RetrievalContext>> {
    let max_tokens = retrieval_token_budget(available_tokens);
    if max_tokens == 0 || query.trim().is_empty() {
        return Ok(None);
    }

    let mut indexes = indexes()?;
    let key = repo.to_path_buf();
    if !indexes.contains_key(&key) {
        let snapshot = IndexSnapshot::capture(repo)?;
        indexes.insert(
            key.clone(),
            CachedRepository {
                git_identity: git_identity(repo)?,
                source_paths: snapshot.files.into_keys().collect(),
                cache: RepositoryIndexCache::load(key.clone())?,
            },
        );
    }
    let entry = indexes.get(&key).ok_or_else(|| {
        MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            "repository index cache was not initialized",
        )
    })?;
    let report = entry.cache.index().retrieve(
        repo,
        query,
        RetrievalBudget {
            max_tokens: usize::try_from(max_tokens).unwrap_or(usize::MAX),
            max_results: 24,
            max_tokens_per_result: 1_200,
        },
    );
    if report.results.is_empty() && report.exclusions.is_empty() {
        return Ok(None);
    }
    Ok(Some(RetrievalContext {
        system_fragment: format_retrieval_context(&report),
        status: retrieval_summary(&report, available_tokens),
    }))
}

fn retrieval_token_budget(available_tokens: u64) -> u64 {
    available_tokens
        .saturating_sub(RETRIEVAL_WRAPPER_RESERVE_TOKENS)
        .min(MAX_RETRIEVAL_TOKENS)
}

fn format_retrieval_context(report: &RetrievalReport) -> String {
    let mut context = String::from(
        "REPOSITORY RETRIEVAL CONTEXT\nUse these ranked source fragments as evidence. Read files with tools before editing when broader context is needed.\n",
    );
    for result in &report.results {
        context.push_str(&format!(
            "\n--- {}:{}-{} · symbol {} · score {} ---\n{}\n",
            result.path.display(),
            result.start_line,
            result.end_line,
            result.symbol,
            result.score,
            result.content
        ));
    }
    context
}

fn retrieval_summary(report: &RetrievalReport, available_tokens: u64) -> String {
    let exclusions = report
        .exclusions
        .iter()
        .fold(BTreeMap::<&str, usize>::new(), |mut counts, exclusion| {
            *counts.entry(exclusion.reason.as_str()).or_default() += 1;
            counts
        })
        .into_iter()
        .map(|(reason, count)| format!("{reason}: {count}"))
        .collect::<Vec<_>>();
    format!(
        "Repository context: included {} fragment(s), used {}/{} retrieval tokens, protected capacity {} tokens, excluded {} fragment(s){}.",
        report.results.len(),
        report.used_tokens,
        report.budget.max_tokens,
        available_tokens.saturating_sub(u64::try_from(report.budget.max_tokens).unwrap_or(u64::MAX)),
        report.exclusions.len(),
        if exclusions.is_empty() {
            String::new()
        } else {
            format!(" ({})", exclusions.join(", "))
        }
    )
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
    fn retrieval_budget_preserves_wrapper_and_non_repository_capacity() {
        assert_eq!(retrieval_token_budget(200), 0);
        assert_eq!(retrieval_token_budget(1_000), 744);
        assert_eq!(retrieval_token_budget(20_000), MAX_RETRIEVAL_TOKENS);
    }

    #[test]
    fn retrieval_context_reports_inclusions_exclusions_and_protected_capacity() {
        let repository = tempfile::tempdir().expect("repository");
        fs::write(
            repository.path().join("lib.rs"),
            "pub fn retrieve_alpha() -> usize { 1 }\npub fn retrieve_beta() -> usize { 2 }\n",
        )
        .expect("source");
        assert!(refresh(repository.path()).expect("load").is_none());

        let context = retrieve_context(repository.path(), "retrieve", 300)
            .expect("retrieval")
            .expect("context");
        assert!(context.system_fragment.contains("retrieve_alpha"));
        assert!(context.status.contains("included 1 fragment(s)"));
        assert!(context.status.contains("excluded 1 fragment(s)"));
        assert!(context.status.contains("protected capacity 256 tokens"));
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
