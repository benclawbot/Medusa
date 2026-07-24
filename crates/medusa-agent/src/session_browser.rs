use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use medusa_browser_client::BrowserClient;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use medusa_intelligence::{CodeIndex, IndexRefresh, IndexSnapshot};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::session::load;

/// Lightweight durable-session metadata suitable for frontend discovery lists.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub objective: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub completed: bool,
    pub waiting_for_user: bool,
    pub turn: u32,
}

/// Discovers all durable sessions for one repository across primary and fallback storage.
///
/// Duplicate session IDs are returned once. Sessions are ordered by most recently updated,
/// then by ID for deterministic presentation.
pub fn list_sessions(repo: &Path) -> MedusaResult<Vec<SessionSummary>> {
    let mut ids = BTreeSet::new();
    collect_session_ids(&repo.join(".medusa/sessions"), &mut ids)?;
    collect_session_ids(&fallback_session_root(repo), &mut ids)?;

    let mut sessions = ids
        .into_iter()
        .map(|id| {
            let session = load(repo, id.as_str())?;
            Ok(SessionSummary {
                id: session.id.to_string(),
                objective: session.objective,
                created_at: session.created_at,
                updated_at: session.updated_at,
                completed: session.completed,
                waiting_for_user: session.pending_question.is_some(),
                turn: session.turn,
            })
        })
        .collect::<MedusaResult<Vec<_>>>()?;
    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(sessions)
}

fn collect_session_ids(root: &Path, ids: &mut BTreeSet<SessionId>) -> MedusaResult<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if let Ok(id) = SessionId::parse(stem) {
            ids.insert(id);
        }
    }
    Ok(())
}

fn fallback_session_root(repo: &Path) -> PathBuf {
    let root = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    root.join("Medusa/sessions").join(repository_key(repo))
}

fn repository_key(repo: &Path) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in repo.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[derive(Clone, Debug)]
pub struct SessionBrowserConfig {
    pub enabled: bool,
    pub path: Option<PathBuf>,
    pub timeout: Duration,
}

impl Default for SessionBrowserConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: None,
            timeout: Duration::from_secs(30),
        }
    }
}

pub struct SessionBrowser {
    #[allow(dead_code)]
    config: SessionBrowserConfig,
    client: Option<BrowserClient>,
}

impl SessionBrowser {
    pub fn connect(config: &SessionBrowserConfig) -> MedusaResult<Self> {
        if !config.enabled {
            return Ok(Self {
                config: config.clone(),
                client: None,
            });
        }
        let path = resolve_path(config.path.as_deref())?;
        if !path.exists() {
            return Ok(Self {
                config: config.clone(),
                client: None,
            });
        }
        let client = BrowserClient::spawn(path.to_str().ok_or_else(|| invalid("non-utf8 path"))?)?;
        Ok(Self {
            config: config.clone(),
            client: Some(client),
        })
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.client.is_some()
    }

    pub fn client(&mut self) -> MedusaResult<&mut BrowserClient> {
        self.client
            .as_mut()
            .ok_or_else(|| unavailable("browser is not enabled in this session"))
    }
}

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

fn resolve_path(configured: Option<&Path>) -> MedusaResult<PathBuf> {
    if let Some(path) = configured {
        return Ok(path.to_path_buf());
    }
    let exe_name = if cfg!(windows) {
        "medusa-browserd.exe"
    } else {
        "medusa-browserd"
    };
    let agent_exe =
        std::env::current_exe().map_err(|error| unavailable(format!("current_exe: {error}")))?;
    let adjacent = agent_exe.parent().map(|parent| parent.join(exe_name));
    if let Some(adjacent) = &adjacent
        && adjacent.exists()
    {
        return Ok(adjacent.clone());
    }
    if let Ok(found) = which(exe_name) {
        return Ok(found);
    }
    Err(unavailable(format!(
        "{exe_name} not found on PATH and not adjacent to the agent binary"
    )))
}

fn which(command: &str) -> Result<PathBuf, ()> {
    let path = std::env::var_os("PATH").ok_or(())?;
    for entry in std::env::split_paths(&path) {
        let candidate = entry.join(command);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(())
}

fn unavailable(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        message,
    )
    .with_retryable(true)
}

fn invalid(message: &'static str) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use medusa_config::Config;
    use medusa_core::MedusaResult;
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};

    use super::*;
    use crate::AgentEngine;

    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("session creation does not call the provider")
        }
    }

    #[test]
    fn session_browser_disabled_when_path_missing() {
        let config = SessionBrowserConfig {
            enabled: true,
            path: Some(PathBuf::from("/nonexistent/medusa-browserd")),
            timeout: Duration::from_secs(5),
        };
        let session = SessionBrowser::connect(&config).expect("browser configuration");
        assert!(!session.is_enabled());
    }

    #[test]
    fn session_browser_disabled_when_flag_false() {
        let config = SessionBrowserConfig {
            enabled: false,
            path: None,
            timeout: Duration::from_secs(5),
        };
        let session = SessionBrowser::connect(&config).expect("browser configuration");
        assert!(!session.is_enabled());
    }

    #[test]
    fn durable_sessions_are_discovered_once_with_frontend_metadata() {
        let repository = tempfile::tempdir().expect("repository");
        let engine = AgentEngine::new(UnusedProvider, Config::default());
        let session = engine
            .create_session(repository.path(), "Resume desktop work".to_owned())
            .expect("session");

        let primary = repository
            .path()
            .join(".medusa/sessions")
            .join(format!("{}.json", session.id));
        let fallback = fallback_session_root(repository.path());
        fs::create_dir_all(&fallback).expect("fallback directory");
        fs::copy(&primary, fallback.join(format!("{}.json", session.id)))
            .expect("duplicate fallback session");
        fs::write(fallback.join("not-a-session.json"), b"{}").expect("unrelated json file");

        let sessions = list_sessions(repository.path()).expect("session catalog");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session.id.to_string());
        assert_eq!(sessions[0].objective, "Resume desktop work");
        assert!(!sessions[0].completed);
        assert!(!sessions[0].waiting_for_user);
    }

    #[test]
    fn repository_index_refreshes_incrementally_and_noops_when_unchanged() {
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
