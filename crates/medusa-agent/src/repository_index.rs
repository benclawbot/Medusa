use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, MutexGuard, OnceLock},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_intelligence::{
    CodeIndex, IndexRefresh, IndexSnapshot, RepositoryGraph, RepositoryGraphFreshness,
    RetrievalBudget, RetrievalReport,
};
use medusa_memory::{SessionSearchQuery, open_session_recall};

use crate::session_browser::RepositoryIndexCache;

const MAX_RETRIEVAL_TOKENS: u64 = 8_000;
const RETRIEVAL_WRAPPER_RESERVE_TOKENS: u64 = 256;
const MAX_RECALL_HITS: usize = 3;
const MAX_RECALL_EXCERPT_CHARS: usize = 1_200;

#[derive(Debug)]
struct CachedRepository {
    git_identity: Vec<u8>,
    source_paths: BTreeSet<PathBuf>,
    graph: Option<RepositoryGraph>,
    legacy_cache: Option<RepositoryIndexCache>,
}

impl CachedRepository {
    fn load(repo: PathBuf, git_identity: Vec<u8>) -> MedusaResult<Self> {
        if git_repository(&repo) {
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

    fn ensure_retrieval_freshness(&mut self) -> MedusaResult<()> {
        let Some(graph) = &mut self.graph else {
            return Ok(());
        };
        if graph.freshness() == RepositoryGraphFreshness::Stale {
            graph.refresh()?;
            self.source_paths = graph.snapshot().files.keys().cloned().collect();
        }
        Ok(())
    }

    fn graph_status(&self) -> Option<String> {
        let graph = self.graph.as_ref()?;
        Some(format!(
            "Persistent repository graph: repository revision {}, graph revision {}, freshness {:?}.",
            graph.snapshot().repository_revision,
            graph.snapshot().graph_revision,
            graph.freshness()
        ))
    }
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
        let identity = git_identity(repo)?;
        indexes.insert(key.clone(), CachedRepository::load(key.clone(), identity)?);
    }
    let entry = indexes.get_mut(&key).ok_or_else(|| {
        MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            "repository index cache was not initialized",
        )
    })?;
    entry.ensure_retrieval_freshness()?;
    let report = entry.index()?.retrieve(
        repo,
        query,
        RetrievalBudget {
            max_tokens: usize::try_from(max_tokens).unwrap_or(usize::MAX),
            max_results: 24,
            max_tokens_per_result: 1_200,
        },
    );
    let graph_status = entry.graph_status();
    drop(indexes);

    let recall = retrieve_recall_context(repo, query)?;
    if report.results.is_empty() && report.exclusions.is_empty() && recall.is_none() {
        return Ok(None);
    }

    let mut fragments = Vec::new();
    let mut statuses = Vec::new();
    if !report.results.is_empty() || !report.exclusions.is_empty() {
        fragments.push(format_retrieval_context(&report));
        statuses.push(retrieval_summary(&report, available_tokens));
    }
    if let Some(status) = graph_status {
        statuses.push(status);
    }
    if let Some(recall) = recall {
        fragments.push(recall.system_fragment);
        statuses.push(format!("Session recall: {}.", recall.status));
    }

    Ok(Some(RetrievalContext {
        system_fragment: fragments.join("\n\n"),
        status: statuses.join(" "),
    }))
}

fn retrieve_recall_context(repo: &Path, query: &str) -> MedusaResult<Option<RetrievalContext>> {
    let store = open_session_recall(repo)?;
    let hits = store.session_search(&SessionSearchQuery {
        query: query.to_owned(),
        repository_fingerprint: Some(format!("path:{}", repo.to_string_lossy())),
        outcome: Some("success".to_owned()),
        limit: MAX_RECALL_HITS,
        ..SessionSearchQuery::default()
    })?;
    if hits.is_empty() {
        return Ok(None);
    }

    let mut fragment = String::from(
        "PRIOR SUCCESSFUL SESSION EVIDENCE\nReuse only details that remain applicable. Inspect the current repository before acting.\n",
    );
    for hit in &hits {
        fragment.push_str(&format!(
            "\n- Session {} ({}, relevance {}): {}",
            hit.session_id,
            hit.created_at,
            hit.relevance_milli,
            truncate_chars(hit.excerpt.trim(), MAX_RECALL_EXCERPT_CHARS)
        ));
    }
    Ok(Some(RetrievalContext {
        system_fragment: fragment,
        status: format!("loaded {} relevant prior session(s)", hits.len()),
    }))
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    let mut truncated = value.chars().take(limit).collect::<String>();
    truncated.push('…');
    truncated
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

fn git_repository(repo: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(repo)
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout.starts_with(b"true"))
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
    fn git_repository_retrieval_uses_persistent_graph_and_refreshes_stale_content() {
        let repository = tempfile::tempdir().expect("repository");
        git_ok(repository.path(), &["init", "-q"]);
        git_ok(
            repository.path(),
            &["config", "user.email", "medusa@example.invalid"],
        );
        git_ok(repository.path(), &["config", "user.name", "Medusa Test"]);
        fs::write(
            repository.path().join("lib.rs"),
            "pub fn graph_before() -> usize { 1 }\n",
        )
        .expect("source");
        git_ok(repository.path(), &["add", "lib.rs"]);
        git_ok(repository.path(), &["commit", "-qm", "fixture"]);

        assert!(refresh(repository.path()).expect("initial graph load").is_none());
        {
            let indexes = indexes().expect("indexes");
            let entry = indexes.get(repository.path()).expect("entry");
            assert!(entry.graph.is_some());
            assert!(entry.legacy_cache.is_none());
        }

        fs::write(
            repository.path().join("lib.rs"),
            "pub fn graph_after() -> usize { 2 }\n",
        )
        .expect("modify");
        let context = retrieve_context(repository.path(), "graph_after", 2_000)
            .expect("retrieval")
            .expect("context");
        assert!(context.system_fragment.contains("graph_after"));
        assert!(context.status.contains("Persistent repository graph"));
        assert!(context.status.contains("Current"));
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
            "pub fn retrieve_alpha() -> usize { 1 }\npub fn retrieve_beta() -> usize { let oversized_repository_context_fragment = [0usize; 256]; oversized_repository_context_fragment.iter().copied().sum() }\n",
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
    fn retrieval_context_includes_successful_same_repository_recall() {
        use medusa_memory::{SessionEvent, SessionRecord};

        let repository = tempfile::tempdir().expect("repository");
        fs::write(repository.path().join("lib.rs"), "fn unrelated_source() {}\n")
            .expect("source");
        let store = open_session_recall(repository.path()).expect("recall store");
        store
            .upsert(&SessionRecord {
                session_id: "matching".to_owned(),
                parent_session_id: None,
                created_at: "2026-07-26T10:00:00Z".to_owned(),
                repository_fingerprint: format!(
                    "path:{}",
                    repository.path().to_string_lossy()
                ),
                outcome: "success".to_owned(),
                events: vec![SessionEvent {
                    ordinal: 0,
                    kind: "objective".to_owned(),
                    tool: None,
                    success: Some(true),
                    text: "repair Windows containment policy".to_owned(),
                }],
            })
            .expect("matching session");
        store
            .upsert(&SessionRecord {
                session_id: "failed".to_owned(),
                parent_session_id: None,
                created_at: "2026-07-26T11:00:00Z".to_owned(),
                repository_fingerprint: format!(
                    "path:{}",
                    repository.path().to_string_lossy()
                ),
                outcome: "failed".to_owned(),
                events: vec![SessionEvent {
                    ordinal: 0,
                    kind: "objective".to_owned(),
                    tool: None,
                    success: Some(false),
                    text: "repair Windows containment policy".to_owned(),
                }],
            })
            .expect("failed session");

        let context = retrieve_context(
            repository.path(),
            "repair Windows containment policy",
            2_000,
        )
        .expect("retrieval")
        .expect("context");
        assert!(context.system_fragment.contains("matching"));
        assert!(!context.system_fragment.contains("failed"));
        assert!(context.status.contains("loaded 1 relevant prior session(s)"));
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
