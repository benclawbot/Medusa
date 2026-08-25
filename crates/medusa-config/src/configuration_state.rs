use std::{
    fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, storage};
use serde::{Deserialize, Serialize};

const CONFIGURATION_STATE_SCHEMA_VERSION: u32 = 1;
const LOCK_RETRY_ATTEMPTS: usize = 200;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);
const STALE_LOCK_AGE: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigurationChangeOrigin {
    Cli,
    Tui,
    Desktop,
    System,
}

impl ConfigurationChangeOrigin {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Tui => "tui",
            Self::Desktop => "desktop",
            Self::System => "system",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigurationApplyTiming {
    Immediate,
    NextSession,
    RestartRequired,
}

impl ConfigurationApplyTiming {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::NextSession => "next-session",
            Self::RestartRequired => "restart-required",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationChanged {
    pub revision: u64,
    pub active_profile: String,
    pub changed_keys: Vec<String>,
    pub origin: ConfigurationChangeOrigin,
    pub apply_timing: ConfigurationApplyTiming,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ConfigurationState {
    schema_version: u32,
    revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_change: Option<ConfigurationChanged>,
}

impl Default for ConfigurationState {
    fn default() -> Self {
        Self {
            schema_version: CONFIGURATION_STATE_SCHEMA_VERSION,
            revision: 0,
            last_change: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConfigurationStateStore {
    root: PathBuf,
}

impl ConfigurationStateStore {
    pub(crate) fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub(crate) fn revision(&self) -> MedusaResult<u64> {
        Ok(self.load()?.revision)
    }

    pub(crate) fn last_change(&self) -> MedusaResult<Option<ConfigurationChanged>> {
        Ok(self.load()?.last_change)
    }

    pub(crate) fn acquire(&self) -> MedusaResult<ConfigurationStateGuard> {
        fs::create_dir_all(&self.root)
            .map_err(|error| store_error(format!("create {}: {error}", self.root.display())))?;
        let lock_path = self.root.join(".configuration.lock");
        for _ in 0..LOCK_RETRY_ATTEMPTS {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(mut file) => {
                    writeln!(file, "pid={}", std::process::id()).map_err(|error| {
                        store_error(format!("write {}: {error}", lock_path.display()))
                    })?;
                    file.sync_all().map_err(|error| {
                        store_error(format!("sync {}: {error}", lock_path.display()))
                    })?;
                    drop(file);
                    let state = match self.load() {
                        Ok(state) => state,
                        Err(error) => {
                            let _ = fs::remove_file(&lock_path);
                            return Err(error);
                        }
                    };
                    return Ok(ConfigurationStateGuard {
                        state,
                        state_path: self.root.join("configuration-state.toml"),
                        lock_path,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&lock_path) {
                        match fs::remove_file(&lock_path) {
                            Ok(()) => continue,
                            Err(remove_error)
                                if remove_error.kind() == std::io::ErrorKind::NotFound =>
                            {
                                continue;
                            }
                            Err(_) => {}
                        }
                    }
                    thread::sleep(LOCK_RETRY_DELAY);
                }
                Err(error) => {
                    return Err(store_error(format!(
                        "create configuration lock {}: {error}",
                        lock_path.display()
                    )));
                }
            }
        }
        Err(store_error(format!(
            "configuration is busy; could not acquire {}",
            lock_path.display()
        )))
    }

    fn load(&self) -> MedusaResult<ConfigurationState> {
        let path = self.root.join("configuration-state.toml");
        if !path.exists() {
            return Ok(ConfigurationState::default());
        }
        let text = fs::read_to_string(&path)
            .map_err(|error| store_error(format!("read {}: {error}", path.display())))?;
        let state: ConfigurationState = toml::from_str(&text)
            .map_err(|error| store_error(format!("parse {}: {error}", path.display())))?;
        validate_state(&state)?;
        Ok(state)
    }
}

pub(crate) struct ConfigurationStateGuard {
    state: ConfigurationState,
    state_path: PathBuf,
    lock_path: PathBuf,
}

impl ConfigurationStateGuard {
    pub(crate) fn revision(&self) -> u64 {
        self.state.revision
    }

    pub(crate) fn check_expected(&self, expected_revision: u64) -> MedusaResult<()> {
        if self.state.revision == expected_revision {
            return Ok(());
        }
        Err(config_error(format!(
            "stale configuration revision {expected_revision}; current revision is {}; reload configuration and retry",
            self.state.revision
        )))
    }

    pub(crate) fn commit(
        &mut self,
        active_profile: String,
        changed_keys: impl IntoIterator<Item = String>,
        origin: ConfigurationChangeOrigin,
        apply_timing: ConfigurationApplyTiming,
    ) -> MedusaResult<ConfigurationChanged> {
        validate_profile_name(&active_profile)?;
        let revision = self
            .state
            .revision
            .checked_add(1)
            .ok_or_else(|| store_error("configuration revision overflow"))?;
        let mut changed_keys = changed_keys.into_iter().collect::<Vec<_>>();
        changed_keys.sort();
        changed_keys.dedup();
        if changed_keys.is_empty() || changed_keys.iter().any(|key| !safe_change_key(key)) {
            return Err(config_error(
                "configuration change keys must be non-secret identifiers",
            ));
        }
        let change = ConfigurationChanged {
            revision,
            active_profile,
            changed_keys,
            origin,
            apply_timing,
        };
        let next = ConfigurationState {
            schema_version: CONFIGURATION_STATE_SCHEMA_VERSION,
            revision,
            last_change: Some(change.clone()),
        };
        let text = toml::to_string_pretty(&next).map_err(|error| store_error(error.to_string()))?;
        atomic_write(&self.state_path, text.as_bytes())?;
        self.state = next;
        Ok(change)
    }
}

impl Drop for ConfigurationStateGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
        sync_parent(&self.lock_path);
    }
}

fn validate_state(state: &ConfigurationState) -> MedusaResult<()> {
    if state.schema_version != CONFIGURATION_STATE_SCHEMA_VERSION {
        return Err(store_error(format!(
            "unsupported configuration state schema version {}",
            state.schema_version
        )));
    }
    match &state.last_change {
        Some(change) if change.revision == state.revision => {
            validate_profile_name(&change.active_profile)?;
            if change.changed_keys.is_empty()
                || change.changed_keys.iter().any(|key| !safe_change_key(key))
            {
                return Err(store_error(
                    "configuration state contains an unsafe change key",
                ));
            }
        }
        Some(_) => {
            return Err(store_error(
                "configuration state revision does not match its last change",
            ));
        }
        None if state.revision == 0 => {}
        None => {
            return Err(store_error(
                "configuration state omitted the last change for a non-zero revision",
            ));
        }
    }
    Ok(())
}

fn validate_profile_name(name: &str) -> MedusaResult<()> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'-' | b'_'))
        });
    if valid {
        Ok(())
    } else {
        Err(config_error(
            "configuration state contains an invalid profile name",
        ))
    }
}

fn safe_change_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    !key.is_empty()
        && key.len() <= 128
        && key.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'[' | b']')
        })
        && !["secret", "token", "credential", "api_key", "authorization"]
            .iter()
            .any(|forbidden| normalized.contains(forbidden))
}

fn lock_is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age >= STALE_LOCK_AGE)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> MedusaResult<()> {
    storage::atomic_write(path, bytes)
        .map_err(|error| store_error(format!("write {}: {error}", path.display())))
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

fn config_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn store_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Environment,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_state_is_revision_zero_and_changes_are_secret_free() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ConfigurationStateStore::at(directory.path());
        assert_eq!(store.revision().expect("revision"), 0);
        assert_eq!(store.last_change().expect("change"), None);

        let mut guard = store.acquire().expect("lock");
        guard.check_expected(0).expect("expected");
        let change = guard
            .commit(
                "default".to_owned(),
                ["model".to_owned()],
                ConfigurationChangeOrigin::Cli,
                ConfigurationApplyTiming::Immediate,
            )
            .expect("commit");
        assert_eq!(change.revision, 1);
        let encoded = toml::to_string(&change).expect("encode");
        assert!(!encoded.contains("secret"));
        assert!(!encoded.contains("token"));
    }

    #[test]
    fn stale_revision_fails_before_commit() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ConfigurationStateStore::at(directory.path());
        let mut guard = store.acquire().expect("lock");
        guard
            .commit(
                "default".to_owned(),
                ["model".to_owned()],
                ConfigurationChangeOrigin::System,
                ConfigurationApplyTiming::Immediate,
            )
            .expect("first commit");
        drop(guard);

        let guard = store.acquire().expect("lock");
        let error = guard.check_expected(0).expect_err("stale");
        assert!(error.to_string().contains("current revision is 1"));
    }

    #[test]
    fn secret_like_change_keys_fail_closed() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ConfigurationStateStore::at(directory.path());
        let mut guard = store.acquire().expect("lock");
        assert!(
            guard
                .commit(
                    "default".to_owned(),
                    ["telegram.bot_token".to_owned()],
                    ConfigurationChangeOrigin::System,
                    ConfigurationApplyTiming::RestartRequired,
                )
                .is_err()
        );
    }
}
