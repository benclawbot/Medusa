//! Durable, frontend-neutral storage and scope resolution for approved learnings.
//!
//! User-level data lives outside repositories while repository-specific data remains local. The
//! resolver exposes shadowed entries instead of silently discarding conflicts, and scope changes
//! require an explicit reviewed action.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningScope {
    Global,
    User,
    Organization,
    Workspace,
    Repository,
    Session,
    Task,
}

impl LearningScope {
    const fn precedence(self) -> u8 {
        match self {
            Self::Global => 0,
            Self::User => 1,
            Self::Organization => 2,
            Self::Workspace => 3,
            Self::Repository => 4,
            Self::Session => 5,
            Self::Task => 6,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RepositoryIdentity {
    /// Stable normalized remote identity. Forks must use their own origin identity.
    pub origin_fingerprint: String,
    /// Git common-directory identity shared by worktrees of the same clone.
    pub common_directory_fingerprint: String,
}

impl RepositoryIdentity {
    pub fn new(origin: &str, common_directory: &str) -> Result<Self, StoreError> {
        require_non_empty(origin, "repository origin cannot be empty")?;
        require_non_empty(common_directory, "git common directory cannot be empty")?;
        Ok(Self {
            origin_fingerprint: digest(&normalize_origin(origin)),
            common_directory_fingerprint: digest(common_directory.trim()),
        })
    }

    #[must_use]
    pub fn same_logical_repository(&self, other: &Self) -> bool {
        self.origin_fingerprint == other.origin_fingerprint
    }

    #[must_use]
    pub fn same_clone_or_worktree(&self, other: &Self) -> bool {
        self.common_directory_fingerprint == other.common_directory_fingerprint
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Applicability {
    pub repository: Option<RepositoryIdentity>,
    pub workspace_id: Option<String>,
    pub organization_id: Option<String>,
    pub task_kinds: BTreeSet<String>,
    pub artifact_kinds: BTreeSet<String>,
    pub excluded_contexts: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LearningProvenance {
    pub source_signal_ids: Vec<String>,
    pub evidence_digests: Vec<String>,
    pub generalized_from_private_content: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningState {
    Active,
    Disabled,
    Deleted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScopedLearning {
    pub id: String,
    pub conflict_key: String,
    pub owner_id: String,
    pub scope: LearningScope,
    pub version: u64,
    pub state: LearningState,
    pub generalized_rule: String,
    pub provenance: LearningProvenance,
    pub applicability: Applicability,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

impl ScopedLearning {
    fn validate(&self) -> Result<(), StoreError> {
        require_non_empty(&self.id, "learning identifier cannot be empty")?;
        require_non_empty(&self.conflict_key, "learning conflict key cannot be empty")?;
        require_non_empty(&self.owner_id, "learning owner cannot be empty")?;
        if self.version == 0 {
            return Err(StoreError::Validation("learning version must be positive"));
        }
        if self.state != LearningState::Deleted {
            require_non_empty(&self.generalized_rule, "generalized rule cannot be empty")?;
            reject_sensitive_content(&self.generalized_rule)?;
        }
        validate_scope_binding(self.scope, &self.applicability)?;
        for digest_value in &self.provenance.evidence_digests {
            if digest_value.len() != 64
                || !digest_value.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(StoreError::Validation(
                    "evidence references must be SHA-256 digests",
                ));
            }
        }
        Ok(())
    }

    fn tombstone(&self, now_unix_ms: i64) -> Self {
        Self {
            id: self.id.clone(),
            conflict_key: self.conflict_key.clone(),
            owner_id: self.owner_id.clone(),
            scope: self.scope,
            version: self.version.saturating_add(1),
            state: LearningState::Deleted,
            generalized_rule: String::new(),
            provenance: LearningProvenance {
                source_signal_ids: Vec::new(),
                evidence_digests: self.provenance.evidence_digests.clone(),
                generalized_from_private_content: self.provenance.generalized_from_private_content,
            },
            applicability: self.applicability.clone(),
            created_at_unix_ms: self.created_at_unix_ms,
            updated_at_unix_ms: now_unix_ms,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScopeContext {
    pub owner_id: String,
    pub repository: Option<RepositoryIdentity>,
    pub workspace_id: Option<String>,
    pub organization_id: Option<String>,
    pub session_id: Option<String>,
    pub task_id: Option<String>,
    pub task_kind: Option<String>,
    pub artifact_kind: Option<String>,
    pub context_tags: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedLearning {
    pub winner: ScopedLearning,
    pub shadowed: Vec<ScopedLearning>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolutionSet {
    pub resolved: Vec<ResolvedLearning>,
    pub conflicts: Vec<ScopeConflict>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeConflict {
    pub conflict_key: String,
    pub winner_id: String,
    pub shadowed_ids: Vec<String>,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScopeReview {
    pub reviewer_id: String,
    pub source_scope: LearningScope,
    pub target_scope: LearningScope,
    pub approved: bool,
    pub rationale: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StoreDocument {
    schema_version: u32,
    revision: u64,
    entries: BTreeMap<String, ScopedLearning>,
}

impl Default for StoreDocument {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 0,
            entries: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScopedMemoryStore {
    user_store: PathBuf,
    repository_store: Option<PathBuf>,
}

impl ScopedMemoryStore {
    #[must_use]
    pub fn new(user_store: PathBuf, repository_store: Option<PathBuf>) -> Self {
        Self {
            user_store,
            repository_store,
        }
    }

    pub fn revision(&self, scope: LearningScope) -> Result<u64, StoreError> {
        Ok(self.load_document(scope)?.revision)
    }

    pub fn upsert(
        &self,
        learning: ScopedLearning,
        expected_revision: u64,
    ) -> Result<u64, StoreError> {
        learning.validate()?;
        let path = self.path_for_scope(learning.scope)?;
        let mut document = load_document(&path)?;
        ensure_revision(document.revision, expected_revision)?;
        if let Some(existing) = document.entries.get(&learning.id) {
            if learning.version <= existing.version {
                return Err(StoreError::Validation(
                    "learning version must increase when replacing an entry",
                ));
            }
            if learning.owner_id != existing.owner_id {
                return Err(StoreError::Validation("learning owner cannot change"));
            }
        }
        document.entries.insert(learning.id.clone(), learning);
        document.revision = document
            .revision
            .checked_add(1)
            .ok_or(StoreError::Validation("store revision overflow"))?;
        write_document(&path, &document)?;
        Ok(document.revision)
    }

    pub fn disable(
        &self,
        scope: LearningScope,
        learning_id: &str,
        expected_revision: u64,
        now_unix_ms: i64,
    ) -> Result<u64, StoreError> {
        self.mutate(scope, learning_id, expected_revision, |entry| {
            entry.state = LearningState::Disabled;
            entry.version = entry.version.saturating_add(1);
            entry.updated_at_unix_ms = now_unix_ms;
            Ok(())
        })
    }

    pub fn delete(
        &self,
        scope: LearningScope,
        learning_id: &str,
        expected_revision: u64,
        now_unix_ms: i64,
    ) -> Result<u64, StoreError> {
        self.mutate(scope, learning_id, expected_revision, |entry| {
            *entry = entry.tombstone(now_unix_ms);
            Ok(())
        })
    }

    pub fn reviewed_scope_change(
        &self,
        learning_id: &str,
        review: &ScopeReview,
        source_expected_revision: u64,
        target_expected_revision: u64,
        target_applicability: Applicability,
        now_unix_ms: i64,
    ) -> Result<(u64, u64), StoreError> {
        if !review.approved {
            return Err(StoreError::ReviewRequired);
        }
        require_non_empty(&review.reviewer_id, "reviewer cannot be empty")?;
        require_non_empty(&review.rationale, "scope-change rationale cannot be empty")?;
        if review.source_scope == review.target_scope {
            return Err(StoreError::Validation("scope change must change scope"));
        }

        let source_path = self.path_for_scope(review.source_scope)?;
        let target_path = self.path_for_scope(review.target_scope)?;
        let mut source = load_document(&source_path)?;
        let mut target = load_document(&target_path)?;
        ensure_revision(source.revision, source_expected_revision)?;
        ensure_revision(target.revision, target_expected_revision)?;
        let existing = source
            .entries
            .get(learning_id)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        let mut promoted = existing.clone();
        promoted.scope = review.target_scope;
        promoted.applicability = target_applicability;
        promoted.version = promoted.version.saturating_add(1);
        promoted.updated_at_unix_ms = now_unix_ms;
        promoted.validate()?;
        source
            .entries
            .insert(learning_id.to_owned(), existing.tombstone(now_unix_ms));
        target.entries.insert(learning_id.to_owned(), promoted);
        source.revision = source.revision.saturating_add(1);
        target.revision = target.revision.saturating_add(1);
        write_document(&target_path, &target)?;
        write_document(&source_path, &source)?;
        Ok((source.revision, target.revision))
    }

    pub fn resolve(&self, context: &ScopeContext) -> Result<ResolutionSet, StoreError> {
        require_non_empty(&context.owner_id, "scope context owner cannot be empty")?;
        let mut candidates = self
            .all_entries()?
            .into_iter()
            .filter(|entry| entry.owner_id == context.owner_id)
            .filter(|entry| entry.state == LearningState::Active)
            .filter(|entry| applies(entry, context))
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .scope
                .precedence()
                .cmp(&left.scope.precedence())
                .then_with(|| right.version.cmp(&left.version))
                .then_with(|| left.id.cmp(&right.id))
        });

        let mut grouped = BTreeMap::<String, Vec<ScopedLearning>>::new();
        for candidate in candidates {
            grouped
                .entry(candidate.conflict_key.clone())
                .or_default()
                .push(candidate);
        }
        let mut output = ResolutionSet::default();
        for (conflict_key, mut entries) in grouped {
            let winner = entries.remove(0);
            if !entries.is_empty() {
                output.conflicts.push(ScopeConflict {
                    conflict_key,
                    winner_id: winner.id.clone(),
                    shadowed_ids: entries.iter().map(|entry| entry.id.clone()).collect(),
                    reason: "more specific scope wins; equal scope uses highest version then stable identifier"
                        .to_owned(),
                });
            }
            output.resolved.push(ResolvedLearning {
                winner,
                shadowed: entries,
            });
        }
        output
            .resolved
            .sort_by(|left, right| left.winner.conflict_key.cmp(&right.winner.conflict_key));
        output
            .conflicts
            .sort_by(|left, right| left.conflict_key.cmp(&right.conflict_key));
        Ok(output)
    }

    pub fn export_user(&self) -> Result<Vec<u8>, StoreError> {
        let document = load_document(&self.user_store)?;
        Ok(serde_json::to_vec_pretty(&document)?)
    }

    pub fn import_user(&self, bytes: &[u8], expected_revision: u64) -> Result<u64, StoreError> {
        let mut imported: StoreDocument = serde_json::from_slice(bytes)?;
        migrate(&mut imported)?;
        let current = load_document(&self.user_store)?;
        ensure_revision(current.revision, expected_revision)?;
        for entry in imported.entries.values() {
            entry.validate()?;
            if is_repository_local(entry.scope) {
                return Err(StoreError::Validation(
                    "user import cannot contain repository-local entries",
                ));
            }
        }
        imported.revision = current.revision.saturating_add(1);
        write_document(&self.user_store, &imported)?;
        Ok(imported.revision)
    }

    fn all_entries(&self) -> Result<Vec<ScopedLearning>, StoreError> {
        let mut entries = load_document(&self.user_store)?
            .entries
            .into_values()
            .collect::<Vec<_>>();
        if let Some(path) = &self.repository_store {
            entries.extend(load_document(path)?.entries.into_values());
        }
        Ok(entries)
    }

    fn mutate<F>(
        &self,
        scope: LearningScope,
        learning_id: &str,
        expected_revision: u64,
        mutation: F,
    ) -> Result<u64, StoreError>
    where
        F: FnOnce(&mut ScopedLearning) -> Result<(), StoreError>,
    {
        let path = self.path_for_scope(scope)?;
        let mut document = load_document(&path)?;
        ensure_revision(document.revision, expected_revision)?;
        let entry = document
            .entries
            .get_mut(learning_id)
            .ok_or(StoreError::NotFound)?;
        mutation(entry)?;
        entry.validate()?;
        document.revision = document.revision.saturating_add(1);
        write_document(&path, &document)?;
        Ok(document.revision)
    }

    fn load_document(&self, scope: LearningScope) -> Result<StoreDocument, StoreError> {
        load_document(&self.path_for_scope(scope)?)
    }

    fn path_for_scope(&self, scope: LearningScope) -> Result<PathBuf, StoreError> {
        if is_repository_local(scope) {
            self.repository_store
                .clone()
                .ok_or(StoreError::RepositoryStoreUnavailable)
        } else {
            Ok(self.user_store.clone())
        }
    }
}

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Json(serde_json::Error),
    Conflict { expected: u64, actual: u64 },
    Validation(&'static str),
    NotFound,
    ReviewRequired,
    RepositoryStoreUnavailable,
    UnsupportedSchema(u32),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "scoped memory I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "scoped memory JSON failed: {error}"),
            Self::Conflict { expected, actual } => write!(
                formatter,
                "scoped memory revision conflict: expected {expected}, actual {actual}"
            ),
            Self::Validation(message) => formatter.write_str(message),
            Self::NotFound => formatter.write_str("learning entry was not found"),
            Self::ReviewRequired => formatter.write_str("scope change requires approved review"),
            Self::RepositoryStoreUnavailable => {
                formatter.write_str("repository-local store is unavailable")
            }
            Self::UnsupportedSchema(version) => {
                write!(
                    formatter,
                    "unsupported scoped memory schema version {version}"
                )
            }
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

fn load_document(path: &Path) -> Result<StoreDocument, StoreError> {
    if !path.exists() {
        return Ok(StoreDocument::default());
    }
    let bytes = fs::read(path)?;
    let mut document: StoreDocument = serde_json::from_slice(&bytes)?;
    migrate(&mut document)?;
    for entry in document.entries.values() {
        entry.validate()?;
    }
    Ok(document)
}

fn migrate(document: &mut StoreDocument) -> Result<(), StoreError> {
    match document.schema_version {
        SCHEMA_VERSION => Ok(()),
        0 => {
            document.schema_version = SCHEMA_VERSION;
            Ok(())
        }
        version => Err(StoreError::UnsupportedSchema(version)),
    }
}

fn write_document(path: &Path, document: &StoreDocument) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(document)?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, bytes)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&temporary, path)?;
    Ok(())
}

fn ensure_revision(actual: u64, expected: u64) -> Result<(), StoreError> {
    if actual == expected {
        Ok(())
    } else {
        Err(StoreError::Conflict { expected, actual })
    }
}

fn validate_scope_binding(
    scope: LearningScope,
    applicability: &Applicability,
) -> Result<(), StoreError> {
    match scope {
        LearningScope::Repository if applicability.repository.is_none() => Err(
            StoreError::Validation("repository learning requires repository identity"),
        ),
        LearningScope::Workspace if applicability.workspace_id.is_none() => Err(
            StoreError::Validation("workspace learning requires workspace identity"),
        ),
        LearningScope::Organization if applicability.organization_id.is_none() => Err(
            StoreError::Validation("organization learning requires organization identity"),
        ),
        _ => Ok(()),
    }
}

fn applies(entry: &ScopedLearning, context: &ScopeContext) -> bool {
    let scope_matches = match entry.scope {
        LearningScope::Global | LearningScope::User => true,
        LearningScope::Organization => {
            entry.applicability.organization_id == context.organization_id
        }
        LearningScope::Workspace => entry.applicability.workspace_id == context.workspace_id,
        LearningScope::Repository => entry
            .applicability
            .repository
            .as_ref()
            .zip(context.repository.as_ref())
            .is_some_and(|(expected, actual)| expected.same_logical_repository(actual)),
        LearningScope::Session => context.session_id.is_some(),
        LearningScope::Task => context.task_id.is_some(),
    };
    scope_matches
        && matches_constraint(
            &entry.applicability.task_kinds,
            context.task_kind.as_deref(),
        )
        && matches_constraint(
            &entry.applicability.artifact_kinds,
            context.artifact_kind.as_deref(),
        )
        && entry
            .applicability
            .excluded_contexts
            .is_disjoint(&context.context_tags)
}

fn matches_constraint(values: &BTreeSet<String>, current: Option<&str>) -> bool {
    values.is_empty() || current.is_some_and(|value| values.contains(value))
}

const fn is_repository_local(scope: LearningScope) -> bool {
    matches!(
        scope,
        LearningScope::Repository | LearningScope::Session | LearningScope::Task
    )
}

fn reject_sensitive_content(value: &str) -> Result<(), StoreError> {
    let lower = value.to_ascii_lowercase();
    let contains_secret = [
        "-----begin private key-----",
        "seed phrase",
        "recovery phrase",
        "password=",
        "token=",
        "bearer ",
        "sk-",
        "ghp_",
        "github_pat_",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    if contains_secret {
        Err(StoreError::Validation(
            "generalized learning cannot contain secrets or private credentials",
        ))
    } else {
        Ok(())
    }
}

fn require_non_empty(value: &str, message: &'static str) -> Result<(), StoreError> {
    if value.trim().is_empty() {
        Err(StoreError::Validation(message))
    } else {
        Ok(())
    }
}

fn normalize_origin(origin: &str) -> String {
    origin
        .trim()
        .trim_end_matches(".git")
        .to_ascii_lowercase()
        .replace("git@github.com:", "https://github.com/")
}

fn digest(value: &str) -> String {
    hex_digest(Sha256::digest(value.as_bytes()).as_slice())
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn repository(origin: &str, common: &str) -> RepositoryIdentity {
        RepositoryIdentity::new(origin, common).unwrap()
    }

    fn learning(
        id: &str,
        key: &str,
        scope: LearningScope,
        repository: Option<RepositoryIdentity>,
        rule: &str,
    ) -> ScopedLearning {
        ScopedLearning {
            id: id.to_owned(),
            conflict_key: key.to_owned(),
            owner_id: "user-1".to_owned(),
            scope,
            version: 1,
            state: LearningState::Active,
            generalized_rule: rule.to_owned(),
            provenance: LearningProvenance {
                source_signal_ids: vec!["signal-1".to_owned()],
                evidence_digests: vec![digest("evidence")],
                generalized_from_private_content: false,
            },
            applicability: Applicability {
                repository,
                ..Applicability::default()
            },
            created_at_unix_ms: 1,
            updated_at_unix_ms: 1,
        }
    }

    fn store(temp: &TempDir) -> ScopedMemoryStore {
        ScopedMemoryStore::new(
            temp.path().join("user/learnings.json"),
            Some(temp.path().join("repo/.medusa/learnings.json")),
        )
    }

    fn context(repository: Option<RepositoryIdentity>) -> ScopeContext {
        ScopeContext {
            owner_id: "user-1".to_owned(),
            repository,
            workspace_id: None,
            organization_id: None,
            session_id: Some("session".to_owned()),
            task_id: Some("task".to_owned()),
            task_kind: None,
            artifact_kind: None,
            context_tags: BTreeSet::new(),
        }
    }

    #[test]
    fn user_learning_survives_repository_changes() {
        let temp = TempDir::new().unwrap();
        let memory_store = store(&temp);
        memory_store
            .upsert(
                learning(
                    "general",
                    "workflow.complete",
                    LearningScope::User,
                    None,
                    "inventory authoritative sources first",
                ),
                0,
            )
            .unwrap();
        let reopened = store(&temp);
        for repository in [
            repository("https://example.test/a.git", "/clone/a/.git"),
            repository("https://example.test/b.git", "/clone/b/.git"),
        ] {
            let resolved = reopened.resolve(&context(Some(repository))).unwrap();
            assert_eq!(resolved.resolved[0].winner.id, "general");
        }
    }

    #[test]
    fn repository_learning_does_not_leak_and_overrides_user_conflict() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp);
        let repo_a = repository("https://example.test/a.git", "/clone/a/.git");
        let repo_b = repository("https://example.test/b.git", "/clone/b/.git");
        store
            .upsert(
                learning(
                    "user",
                    "format.command",
                    LearningScope::User,
                    None,
                    "use the default formatter",
                ),
                0,
            )
            .unwrap();
        store
            .upsert(
                learning(
                    "repo",
                    "format.command",
                    LearningScope::Repository,
                    Some(repo_a.clone()),
                    "use cargo fmt --all",
                ),
                0,
            )
            .unwrap();

        let in_a = store.resolve(&context(Some(repo_a))).unwrap();
        assert_eq!(in_a.resolved[0].winner.id, "repo");
        assert_eq!(in_a.resolved[0].shadowed[0].id, "user");
        assert_eq!(in_a.conflicts.len(), 1);
        let in_b = store.resolve(&context(Some(repo_b))).unwrap();
        assert_eq!(in_b.resolved[0].winner.id, "user");
        assert!(in_b.conflicts.is_empty());
    }

    #[test]
    fn rename_clone_fork_and_worktree_identity_are_deterministic() {
        let original = repository("git@github.com:Owner/Repo.git", "/old/.git");
        let renamed_checkout = repository("https://github.com/owner/repo", "/new/.git");
        let worktree = repository("https://github.com/owner/repo.git", "/old/.git");
        let fork = repository("https://github.com/fork/repo.git", "/fork/.git");
        assert!(original.same_logical_repository(&renamed_checkout));
        assert!(original.same_clone_or_worktree(&worktree));
        assert!(!original.same_logical_repository(&fork));
    }

    #[test]
    fn scope_change_requires_explicit_review() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp);
        let repo = repository("https://example.test/a", "/a/.git");
        store
            .upsert(
                learning(
                    "repo-rule",
                    "workflow.rule",
                    LearningScope::Repository,
                    Some(repo),
                    "verify locally",
                ),
                0,
            )
            .unwrap();
        let rejected = ScopeReview {
            reviewer_id: "user-1".to_owned(),
            source_scope: LearningScope::Repository,
            target_scope: LearningScope::User,
            approved: false,
            rationale: "not reviewed".to_owned(),
        };
        assert!(matches!(
            store.reviewed_scope_change("repo-rule", &rejected, 1, 0, Applicability::default(), 2,),
            Err(StoreError::ReviewRequired)
        ));
        let approved = ScopeReview {
            approved: true,
            rationale: "applies across repositories".to_owned(),
            ..rejected
        };
        store
            .reviewed_scope_change("repo-rule", &approved, 1, 0, Applicability::default(), 2)
            .unwrap();
        let resolved = store.resolve(&context(None)).unwrap();
        assert_eq!(resolved.resolved[0].winner.scope, LearningScope::User);
    }

    #[test]
    fn deletion_removes_behavior_but_keeps_minimal_audit_tombstone() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp);
        store
            .upsert(
                learning(
                    "delete-me",
                    "workflow.delete",
                    LearningScope::User,
                    None,
                    "apply everywhere",
                ),
                0,
            )
            .unwrap();
        store
            .delete(LearningScope::User, "delete-me", 1, 5)
            .unwrap();
        assert!(store.resolve(&context(None)).unwrap().resolved.is_empty());
        let exported: StoreDocument =
            serde_json::from_slice(&store.export_user().unwrap()).unwrap();
        let tombstone = &exported.entries["delete-me"];
        assert_eq!(tombstone.state, LearningState::Deleted);
        assert!(tombstone.generalized_rule.is_empty());
        assert!(tombstone.provenance.source_signal_ids.is_empty());
    }

    #[test]
    fn stale_concurrent_edit_is_rejected() {
        let temp = TempDir::new().unwrap();
        let first = store(&temp);
        let second = store(&temp);
        first
            .upsert(
                learning("one", "one", LearningScope::User, None, "first"),
                0,
            )
            .unwrap();
        assert!(matches!(
            second.upsert(
                learning("two", "two", LearningScope::User, None, "second",),
                0,
            ),
            Err(StoreError::Conflict {
                expected: 0,
                actual: 1
            })
        ));
    }

    #[test]
    fn export_import_migration_and_offline_reopen_work() {
        let source_temp = TempDir::new().unwrap();
        let source = store(&source_temp);
        source
            .upsert(
                learning(
                    "portable",
                    "portable",
                    LearningScope::User,
                    None,
                    "portable rule",
                ),
                0,
            )
            .unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&source.export_user().unwrap()).unwrap();
        value["schema_version"] = serde_json::json!(0);

        let destination_temp = TempDir::new().unwrap();
        let destination = store(&destination_temp);
        destination
            .import_user(&serde_json::to_vec(&value).unwrap(), 0)
            .unwrap();
        drop(destination);
        let offline = store(&destination_temp);
        assert_eq!(
            offline.resolve(&context(None)).unwrap().resolved[0]
                .winner
                .id,
            "portable"
        );
    }

    #[test]
    fn rejects_secret_bearing_rules() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp);
        assert!(matches!(
            store.upsert(
                learning(
                    "secret",
                    "secret",
                    LearningScope::User,
                    None,
                    "use token=super-secret",
                ),
                0,
            ),
            Err(StoreError::Validation(_))
        ));
    }
}
