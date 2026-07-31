use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CONFIG_VERSION, Config};

/// Persistence format for the authoritative shared configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionedConfig {
    pub schema_version: u16,
    pub revision: u64,
    pub config: Config,
}

impl Default for RevisionedConfig {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_VERSION,
            revision: 0,
            config: Config::default(),
        }
    }
}

/// When a configuration change becomes effective.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApplyTiming {
    Immediate,
    NextSession,
    RestartRequired,
}

/// Secret-free event published after an authoritative configuration change.
///
/// Events intentionally include paths and lifecycle metadata only, never old or new values.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigChangedEvent {
    pub schema_version: u16,
    pub previous_revision: u64,
    pub revision: u64,
    pub changed_paths: Vec<String>,
    pub apply_timing: ApplyTiming,
}

/// Result of a successful optimistic configuration update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigUpdate {
    pub state: RevisionedConfig,
    pub event: ConfigChangedEvent,
}

/// File-backed authority for shared configuration state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the authoritative state, returning validated defaults before first persistence.
    pub fn load(&self) -> MedusaResult<RevisionedConfig> {
        match fs::read_to_string(&self.path) {
            Ok(text) => parse_state(&text, &self.path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let state = RevisionedConfig::default();
                state.config.validate()?;
                Ok(state)
            }
            Err(error) => Err(persistence(format!(
                "read {}: {error}",
                self.path.display()
            ))),
        }
    }

    /// Validates and atomically persists a complete candidate if the caller's revision is current.
    pub fn update(&self, expected_revision: u64, candidate: Config) -> MedusaResult<ConfigUpdate> {
        candidate.validate()?;
        let _lock = StoreLock::acquire(&self.lock_path())?;
        let current = self.load()?;
        if current.revision != expected_revision {
            return Err(conflict(expected_revision, current.revision));
        }

        let revision = current
            .revision
            .checked_add(1)
            .ok_or_else(|| persistence("configuration revision overflow"))?;
        let changed_paths = changed_paths(&current.config, &candidate)?;
        let apply_timing = apply_timing(&changed_paths);
        let state = RevisionedConfig {
            schema_version: CONFIG_VERSION,
            revision,
            config: candidate,
        };
        let encoded = toml::to_string_pretty(&state)
            .map_err(|error| persistence(format!("encode configuration: {error}")))?;
        atomic_write(&self.path, encoded.as_bytes())?;

        Ok(ConfigUpdate {
            event: ConfigChangedEvent {
                schema_version: CONFIG_VERSION,
                previous_revision: current.revision,
                revision,
                changed_paths,
                apply_timing,
            },
            state,
        })
    }

    fn lock_path(&self) -> PathBuf {
        self.path.with_extension("lock")
    }
}

fn parse_state(text: &str, path: &Path) -> MedusaResult<RevisionedConfig> {
    let state: RevisionedConfig = toml::from_str(text)
        .map_err(|error| invalid(format!("parse {}: {error}", path.display())))?;
    if state.schema_version != CONFIG_VERSION {
        return Err(invalid(format!(
            "unsupported configuration schema version {}",
            state.schema_version
        )));
    }
    state.config.validate()?;
    Ok(state)
}

fn changed_paths(before: &Config, after: &Config) -> MedusaResult<Vec<String>> {
    let before = serde_json::to_value(before)
        .map_err(|error| persistence(format!("inspect prior configuration: {error}")))?;
    let after = serde_json::to_value(after)
        .map_err(|error| persistence(format!("inspect candidate configuration: {error}")))?;
    let mut paths = BTreeSet::new();
    collect_changes("", &before, &after, &mut paths);
    Ok(paths.into_iter().collect())
}

fn collect_changes(prefix: &str, before: &Value, after: &Value, paths: &mut BTreeSet<String>) {
    match (before, after) {
        (Value::Object(before), Value::Object(after)) => {
            let keys = before.keys().chain(after.keys()).collect::<BTreeSet<_>>();
            for key in keys {
                let path = if prefix.is_empty() {
                    key.to_owned()
                } else {
                    format!("{prefix}.{key}")
                };
                match (before.get(key), after.get(key)) {
                    (Some(before), Some(after)) => collect_changes(&path, before, after, paths),
                    _ => {
                        paths.insert(path);
                    }
                }
            }
        }
        (Value::Array(before), Value::Array(after)) if before == after => {}
        _ if before == after => {}
        _ => {
            if !prefix.is_empty() {
                paths.insert(prefix.to_owned());
            }
        }
    }
}

fn apply_timing(paths: &[String]) -> ApplyTiming {
    if paths.iter().any(|path| {
        path.starts_with("model.provider")
            || path.starts_with("model.protocol")
            || path.starts_with("model.base_url")
            || path.starts_with("model.auth")
    }) {
        ApplyTiming::RestartRequired
    } else if paths
        .iter()
        .any(|path| path.starts_with("model.") || path.starts_with("agent."))
    {
        ApplyTiming::NextSession
    } else {
        ApplyTiming::Immediate
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> MedusaResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| persistence("configuration path has no parent"))?;
    fs::create_dir_all(parent)
        .map_err(|error| persistence(format!("create {}: {error}", parent.display())))?;
    let temporary = path.with_extension("toml.tmp");
    let mut file = fs::File::create(&temporary)
        .map_err(|error| persistence(format!("write {}: {error}", temporary.display())))?;
    file.write_all(bytes)
        .map_err(|error| persistence(format!("write {}: {error}", temporary.display())))?;
    file.sync_all()
        .map_err(|error| persistence(format!("sync {}: {error}", temporary.display())))?;
    fs::rename(&temporary, path)
        .map_err(|error| persistence(format!("replace {}: {error}", path.display())))?;
    sync_parent(path);
    Ok(())
}

fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent()
        && let Ok(directory) = fs::File::open(parent)
    {
        let _ = directory.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}

struct StoreLock {
    path: PathBuf,
}

impl StoreLock {
    fn acquire(path: &Path) -> MedusaResult<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| persistence("configuration lock path has no parent"))?;
        fs::create_dir_all(parent)
            .map_err(|error| persistence(format!("create {}: {error}", parent.display())))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut file) => {
                    writeln!(file, "{}", std::process::id()).map_err(|error| {
                        persistence(format!("write lock {}: {error}", path.display()))
                    })?;
                    return Ok(Self {
                        path: path.to_owned(),
                    });
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::AlreadyExists
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    return Err(persistence(format!(
                        "acquire configuration lock {}: {error}",
                        path.display()
                    )));
                }
            }
        }
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn conflict(expected: u64, actual: u64) -> MedusaError {
    let mut error = MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        format!(
            "configuration revision is stale: expected {expected}, authoritative revision is {actual}"
        ),
    );
    error
        .context
        .insert("expected_revision".into(), expected.into());
    error
        .context
        .insert("actual_revision".into(), actual.into());
    error
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn persistence(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::PersistenceFailed,
        ErrorCategory::Persistence,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_round_trips_and_emits_value_free_paths() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(directory.path().join("config.toml"));
        let mut candidate = Config::default();
        candidate.agent.max_turns = 42;
        candidate.memory.enabled = false;

        let update = store.update(0, candidate.clone()).expect("update");
        assert_eq!(update.state.revision, 1);
        assert_eq!(update.event.previous_revision, 0);
        assert_eq!(
            update.event.changed_paths,
            vec!["agent.max_turns", "memory.enabled"]
        );
        assert_eq!(update.event.apply_timing, ApplyTiming::NextSession);
        assert_eq!(store.load().expect("load").config, candidate);

        let event = serde_json::to_string(&update.event).expect("event");
        assert!(!event.contains("42"));
    }

    #[test]
    fn stale_update_is_rejected_without_overwrite() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(directory.path().join("config.toml"));
        let mut first = Config::default();
        first.agent.max_turns = 10;
        store.update(0, first.clone()).expect("first update");

        let mut stale = Config::default();
        stale.agent.max_turns = 20;
        let error = store.update(0, stale).expect_err("stale update");
        assert_eq!(error.context["expected_revision"], 0);
        assert_eq!(error.context["actual_revision"], 1);
        assert_eq!(store.load().expect("load").config, first);
    }

    #[test]
    fn invalid_candidate_preserves_last_valid_configuration() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(directory.path().join("config.toml"));
        let valid = Config::default();
        store.update(0, valid.clone()).expect("valid update");

        let mut invalid = valid.clone();
        invalid.agent.max_turns = 0;
        assert!(store.update(1, invalid).is_err());
        assert_eq!(store.load().expect("load").config, valid);
    }

    #[test]
    fn provider_route_changes_require_restart() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(directory.path().join("config.toml"));
        let mut candidate = Config::default();
        candidate.model.provider = "openai".into();
        candidate.model.name = "gpt-5".into();
        let update = store.update(0, candidate).expect("update");
        assert_eq!(update.event.apply_timing, ApplyTiming::RestartRequired);
    }
}
