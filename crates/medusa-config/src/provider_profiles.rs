use std::{
    fs,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, storage};
use serde::{Deserialize, Serialize};

use super::{
    PROVIDER_PROFILE_KEYS, ProviderProfile, ProviderProfileStore,
    configuration_state::{
        ConfigurationApplyTiming, ConfigurationChangeOrigin, ConfigurationChanged,
        ConfigurationStateGuard, ConfigurationStateStore,
    },
    staged_profile::{
        begin_pending_transaction, finish_pending_transaction, reconcile_pending_transaction,
        record_known_good,
    },
};

const ACTIVE_PROFILE_SCHEMA_VERSION: u32 = 1;
const DEFAULT_PROFILE_NAME: &str = "default";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ActiveProfileSelection {
    schema_version: u32,
    name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProviderProfileSnapshot {
    pub revision: u64,
    pub active_profile: String,
    pub profile: ProviderProfile,
}

pub struct ProviderProfileUpdate {
    root: PathBuf,
    guard: ConfigurationStateGuard,
    store: ProviderProfileStore,
    active_profile: String,
    existed: bool,
    previous: ProviderProfile,
}

impl ProviderProfileUpdate {
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.guard.revision()
    }

    #[must_use]
    pub fn profile(&self) -> &ProviderProfile {
        &self.previous
    }

    pub fn commit(
        mut self,
        profile: &ProviderProfile,
        origin: ConfigurationChangeOrigin,
        changed_keys: impl IntoIterator<Item = String>,
        apply_timing: ConfigurationApplyTiming,
    ) -> MedusaResult<ConfigurationChanged> {
        profile.validate()?;
        record_known_good(
            &self.root,
            self.guard.revision(),
            &self.active_profile,
            &self.previous,
        )?;
        begin_pending_transaction(
            &self.root,
            self.guard.revision(),
            &self.active_profile,
            self.existed,
            &self.previous,
            profile,
        )?;
        self.store.save(profile)?;
        match self.guard.commit(
            self.active_profile.clone(),
            changed_keys,
            origin,
            apply_timing,
        ) {
            Ok(change) => {
                finish_pending_transaction(&self.root)?;
                Ok(change)
            }
            Err(error) => {
                restore_profile(&self.store, self.existed, &self.previous).map_err(|rollback| {
                    store_error(format!(
                        "commit configuration metadata: {error}; rollback failed: {rollback}"
                    ))
                })?;
                finish_pending_transaction(&self.root)?;
                Err(error)
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProviderProfileSummary {
    pub name: String,
    pub active: bool,
    pub configured: bool,
    pub provider: String,
    pub model: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderProfileCatalog {
    root: PathBuf,
}

impl ProviderProfileCatalog {
    pub fn user() -> MedusaResult<Self> {
        let default = ProviderProfileStore::user()?;
        let root = default
            .path()
            .parent()
            .ok_or_else(|| store_error("provider configuration path has no parent"))?;
        Ok(Self::at(root))
    }

    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn revision(&self) -> MedusaResult<u64> {
        reconcile_pending_transaction(&self.root)?;
        self.state_store().revision()
    }

    pub fn last_change(&self) -> MedusaResult<Option<ConfigurationChanged>> {
        reconcile_pending_transaction(&self.root)?;
        self.state_store().last_change()
    }

    pub fn snapshot(&self) -> MedusaResult<ProviderProfileSnapshot> {
        reconcile_pending_transaction(&self.root)?;
        let guard = self.state_store().acquire()?;
        let active_profile = self.active_name()?;
        let profile = self.store_for_name(&active_profile).load()?;
        Ok(ProviderProfileSnapshot {
            revision: guard.revision(),
            active_profile,
            profile,
        })
    }

    pub fn active_name(&self) -> MedusaResult<String> {
        let path = self.selection_path();
        if !path.exists() {
            return Ok(DEFAULT_PROFILE_NAME.to_owned());
        }
        let text = fs::read_to_string(&path)
            .map_err(|error| store_error(format!("read {}: {error}", path.display())))?;
        let selection: ActiveProfileSelection = toml::from_str(&text)
            .map_err(|error| store_error(format!("parse {}: {error}", path.display())))?;
        if selection.schema_version != ACTIVE_PROFILE_SCHEMA_VERSION {
            return Err(store_error(format!(
                "unsupported active profile schema version {}",
                selection.schema_version
            )));
        }
        validate_profile_name(&selection.name)?;
        if selection.name == DEFAULT_PROFILE_NAME {
            return Ok(DEFAULT_PROFILE_NAME.to_owned());
        }
        let profile_path = self.named_profile_path(&selection.name);
        if !profile_path.is_file() {
            return Err(store_error(format!(
                "active provider profile `{}` is missing at {}",
                selection.name,
                profile_path.display()
            )));
        }
        Ok(selection.name)
    }

    pub fn active_store(&self) -> MedusaResult<ProviderProfileStore> {
        let name = self.active_name()?;
        Ok(self.store_for_name(&name))
    }

    /// Loads one existing named profile without changing the active selection.
    pub fn load_profile(&self, name: &str) -> MedusaResult<ProviderProfile> {
        validate_profile_name(name)?;
        let store = self.store_for_name(name);
        if name != DEFAULT_PROFILE_NAME && !store.path().is_file() {
            return Err(config_error(format!(
                "provider profile `{name}` does not exist"
            )));
        }
        store.load()
    }

    pub fn list(&self) -> MedusaResult<Vec<ProviderProfileSummary>> {
        let active = self.active_name()?;
        let mut names = vec![DEFAULT_PROFILE_NAME.to_owned()];
        let profiles_root = self.profiles_root();
        match fs::read_dir(&profiles_root) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|error| {
                        store_error(format!("read {}: {error}", profiles_root.display()))
                    })?;
                    if !entry
                        .file_type()
                        .map_err(|error| store_error(error.to_string()))?
                        .is_file()
                    {
                        continue;
                    }
                    let path = entry.path();
                    if path.extension().and_then(|value| value.to_str()) != Some("toml") {
                        continue;
                    }
                    let Some(name) = path.file_stem().and_then(|value| value.to_str()) else {
                        continue;
                    };
                    if validate_profile_name(name).is_ok() && name != DEFAULT_PROFILE_NAME {
                        names.push(name.to_owned());
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(store_error(format!(
                    "read {}: {error}",
                    profiles_root.display()
                )));
            }
        }
        names.sort();
        names.dedup();
        names
            .into_iter()
            .map(|name| {
                let profile = self.store_for_name(&name).load()?;
                Ok(ProviderProfileSummary {
                    active: name == active,
                    name,
                    configured: profile.configured,
                    provider: profile.provider,
                    model: profile.model,
                })
            })
            .collect()
    }

    pub fn begin_active_profile_update(
        &self,
        expected_revision: u64,
    ) -> MedusaResult<ProviderProfileUpdate> {
        reconcile_pending_transaction(&self.root)?;
        let guard = self.state_store().acquire()?;
        guard.check_expected(expected_revision)?;
        let active_profile = self.active_name()?;
        let store = self.store_for_name(&active_profile);
        let existed = store.path().is_file();
        let previous = store.load()?;
        Ok(ProviderProfileUpdate {
            root: self.root.clone(),
            guard,
            store,
            active_profile,
            existed,
            previous,
        })
    }

    pub fn save_active_profile(
        &self,
        profile: &ProviderProfile,
        expected_revision: u64,
        origin: ConfigurationChangeOrigin,
        changed_keys: impl IntoIterator<Item = String>,
        apply_timing: ConfigurationApplyTiming,
    ) -> MedusaResult<ConfigurationChanged> {
        self.begin_active_profile_update(expected_revision)?.commit(
            profile,
            origin,
            changed_keys,
            apply_timing,
        )
    }

    pub fn reset_active_profile(
        &self,
        expected_revision: u64,
        origin: ConfigurationChangeOrigin,
    ) -> MedusaResult<(bool, ConfigurationChanged)> {
        let mut guard = self.state_store().acquire()?;
        guard.check_expected(expected_revision)?;
        let active_profile = self.active_name()?;
        let store = self.store_for_name(&active_profile);
        let existed = store.path().is_file();
        let previous = store.load()?;
        let removed = store.reset()?;
        match guard.commit(
            active_profile,
            ["profile".to_owned()],
            origin,
            ConfigurationApplyTiming::NextSession,
        ) {
            Ok(change) => Ok((removed, change)),
            Err(error) => {
                restore_profile(&store, existed, &previous).map_err(|rollback| {
                    store_error(format!(
                        "commit configuration metadata: {error}; rollback failed: {rollback}"
                    ))
                })?;
                Err(error)
            }
        }
    }

    pub fn create(&self, name: &str) -> MedusaResult<ProviderProfileSummary> {
        let expected_revision = self.revision()?;
        self.create_at_revision(name, expected_revision, ConfigurationChangeOrigin::System)
            .map(|(summary, _)| summary)
    }

    pub fn create_at_revision(
        &self,
        name: &str,
        expected_revision: u64,
        origin: ConfigurationChangeOrigin,
    ) -> MedusaResult<(ProviderProfileSummary, ConfigurationChanged)> {
        validate_named_profile(name)?;
        let mut guard = self.state_store().acquire()?;
        guard.check_expected(expected_revision)?;
        let path = self.named_profile_path(name);
        if path.exists() {
            return Err(config_error(format!(
                "provider profile `{name}` already exists"
            )));
        }
        let active_profile = self.active_name()?;
        let profile = self.store_for_name(&active_profile).load()?;
        let store = ProviderProfileStore::at(path);
        store.save(&profile)?;
        let change = match guard.commit(
            active_profile,
            [format!("profiles.{name}")],
            origin,
            ConfigurationApplyTiming::Immediate,
        ) {
            Ok(change) => change,
            Err(error) => {
                store.reset().map_err(|rollback| {
                    store_error(format!(
                        "commit configuration metadata: {error}; rollback failed: {rollback}"
                    ))
                })?;
                return Err(error);
            }
        };
        Ok((
            ProviderProfileSummary {
                name: name.to_owned(),
                active: false,
                configured: profile.configured,
                provider: profile.provider,
                model: profile.model,
            },
            change,
        ))
    }

    pub fn use_profile(&self, name: &str) -> MedusaResult<ProviderProfileSummary> {
        let expected_revision = self.revision()?;
        self.use_profile_at_revision(name, expected_revision, ConfigurationChangeOrigin::System)
            .map(|(summary, _)| summary)
    }

    pub fn use_profile_at_revision(
        &self,
        name: &str,
        expected_revision: u64,
        origin: ConfigurationChangeOrigin,
    ) -> MedusaResult<(ProviderProfileSummary, ConfigurationChanged)> {
        validate_profile_name(name)?;
        let mut guard = self.state_store().acquire()?;
        guard.check_expected(expected_revision)?;
        let store = self.store_for_name(name);
        if name != DEFAULT_PROFILE_NAME && !store.path().is_file() {
            return Err(config_error(format!(
                "provider profile `{name}` does not exist"
            )));
        }
        let profile = store.load()?;
        let selection_path = self.selection_path();
        let previous_selection = read_optional(&selection_path)?;
        write_selection(&selection_path, name)?;
        let change = match guard.commit(
            name.to_owned(),
            ["active_profile".to_owned()],
            origin,
            ConfigurationApplyTiming::Immediate,
        ) {
            Ok(change) => change,
            Err(error) => {
                restore_optional(&selection_path, previous_selection.as_deref()).map_err(
                    |rollback| {
                        store_error(format!(
                            "commit configuration metadata: {error}; rollback failed: {rollback}"
                        ))
                    },
                )?;
                return Err(error);
            }
        };
        Ok((
            ProviderProfileSummary {
                name: name.to_owned(),
                active: true,
                configured: profile.configured,
                provider: profile.provider,
                model: profile.model,
            },
            change,
        ))
    }

    pub fn delete(&self, name: &str) -> MedusaResult<()> {
        let expected_revision = self.revision()?;
        self.delete_at_revision(name, expected_revision, ConfigurationChangeOrigin::System)
            .map(|_| ())
    }

    pub fn delete_at_revision(
        &self,
        name: &str,
        expected_revision: u64,
        origin: ConfigurationChangeOrigin,
    ) -> MedusaResult<ConfigurationChanged> {
        validate_named_profile(name)?;
        let mut guard = self.state_store().acquire()?;
        guard.check_expected(expected_revision)?;
        if self.active_name()? == name {
            return Err(config_error(format!(
                "cannot delete active provider profile `{name}`; select another profile first"
            )));
        }
        let path = self.named_profile_path(name);
        let previous = read_optional(&path)?
            .ok_or_else(|| config_error(format!("provider profile `{name}` does not exist")))?;
        fs::remove_file(&path)
            .map_err(|error| store_error(format!("remove {}: {error}", path.display())))?;
        sync_parent(&path);
        let active_profile = self.active_name()?;
        match guard.commit(
            active_profile,
            [format!("profiles.{name}")],
            origin,
            ConfigurationApplyTiming::Immediate,
        ) {
            Ok(change) => Ok(change),
            Err(error) => {
                atomic_write(&path, &previous).map_err(|rollback| {
                    store_error(format!(
                        "commit configuration metadata: {error}; rollback failed: {rollback}"
                    ))
                })?;
                Err(error)
            }
        }
    }

    fn state_store(&self) -> ConfigurationStateStore {
        ConfigurationStateStore::at(&self.root)
    }

    fn store_for_name(&self, name: &str) -> ProviderProfileStore {
        if name == DEFAULT_PROFILE_NAME {
            ProviderProfileStore::at(self.root.join("provider.toml"))
        } else {
            ProviderProfileStore::at(self.named_profile_path(name))
        }
    }

    fn named_profile_path(&self, name: &str) -> PathBuf {
        self.profiles_root().join(format!("{name}.toml"))
    }

    fn profiles_root(&self) -> PathBuf {
        self.root.join("profiles")
    }

    fn selection_path(&self) -> PathBuf {
        self.root.join("active-profile.toml")
    }
}

impl ProviderProfile {
    pub fn set_value(&mut self, key: &str, value: &str) -> MedusaResult<()> {
        let value = value.trim();
        match key {
            "connection" => {
                self.connection = non_empty(key, value)?.to_owned();
                match self.connection.as_str() {
                    "chatgpt-oauth" => {
                        self.provider = "openai-oauth".to_owned();
                        self.auth = "none".to_owned();
                        self.base_url = None;
                    }
                    "openai-api" => {
                        self.provider = "openai".to_owned();
                        self.auth = "api-key".to_owned();
                        self.base_url = Some("https://api.openai.com/v1".to_owned());
                    }
                    "omniroute" if self.base_url.is_none() => {
                        self.base_url = Some("http://127.0.0.1:20128/v1".to_owned());
                    }
                    "local" if self.base_url.is_none() => {
                        self.base_url = Some("http://127.0.0.1:11434/v1".to_owned());
                    }
                    _ => {}
                }
            }
            "provider" => self.provider = non_empty(key, value)?.to_owned(),
            "model" => self.model = non_empty(key, value)?.to_owned(),
            "speed" => self.speed = non_empty(key, value)?.to_owned(),
            "reasoning" => self.reasoning = non_empty(key, value)?.to_owned(),
            "auth" => self.auth = non_empty(key, value)?.to_owned(),
            "base_url" => self.base_url = Some(non_empty(key, value)?.to_owned()),
            "configured" => {
                self.configured = value
                    .parse::<bool>()
                    .map_err(|_| config_error("configured must be `true` or `false`"))?;
            }
            _ => return Err(unknown_key(key)),
        }
        self.validate()
    }

    pub fn unset_value(&mut self, key: &str) -> MedusaResult<()> {
        let defaults = Self::default();
        match key {
            "connection" => self.connection = defaults.connection,
            "provider" => self.provider = defaults.provider,
            "model" => self.model = defaults.model,
            "speed" => self.speed = defaults.speed,
            "reasoning" => self.reasoning = defaults.reasoning,
            "auth" => self.auth = defaults.auth,
            "base_url" => self.base_url = None,
            "configured" => self.configured = false,
            _ => return Err(unknown_key(key)),
        }
        self.validate()
    }
}

fn validate_named_profile(name: &str) -> MedusaResult<()> {
    validate_profile_name(name)?;
    if name == DEFAULT_PROFILE_NAME {
        return Err(config_error(
            "`default` is the built-in provider profile and cannot be created or deleted",
        ));
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
            "profile names must be 1..=64 ASCII letters or digits, with `-` and `_` allowed after the first character",
        ))
    }
}

fn non_empty<'a>(key: &str, value: &'a str) -> MedusaResult<&'a str> {
    if value.is_empty() {
        Err(config_error(format!(
            "configuration key `{key}` cannot be empty"
        )))
    } else {
        Ok(value)
    }
}

fn unknown_key(key: &str) -> MedusaError {
    config_error(format!(
        "unknown configuration key `{key}`; available keys: {}",
        PROVIDER_PROFILE_KEYS.join(", ")
    ))
}

fn write_selection(path: &Path, name: &str) -> MedusaResult<()> {
    if name == DEFAULT_PROFILE_NAME {
        return remove_if_exists(path);
    }
    let selection = ActiveProfileSelection {
        schema_version: ACTIVE_PROFILE_SCHEMA_VERSION,
        name: name.to_owned(),
    };
    let text =
        toml::to_string_pretty(&selection).map_err(|error| store_error(error.to_string()))?;
    atomic_write(path, text.as_bytes())
}

fn read_optional(path: &Path) -> MedusaResult<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(store_error(format!("read {}: {error}", path.display()))),
    }
}

fn restore_optional(path: &Path, previous: Option<&[u8]>) -> MedusaResult<()> {
    match previous {
        Some(bytes) => atomic_write(path, bytes),
        None => remove_if_exists(path),
    }
}

fn restore_profile(
    store: &ProviderProfileStore,
    existed: bool,
    previous: &ProviderProfile,
) -> MedusaResult<()> {
    if existed {
        store.save(previous)
    } else {
        store.reset().map(|_| ())
    }
}

fn remove_if_exists(path: &Path) -> MedusaResult<()> {
    match fs::remove_file(path) {
        Ok(()) => {
            sync_parent(path);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(store_error(format!("remove {}: {error}", path.display()))),
    }
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
    fn mutations_validate_complete_candidate() {
        let mut profile = ProviderProfile::default();
        profile
            .set_value("connection", "chatgpt-oauth")
            .expect("oauth route");
        assert_eq!(profile.provider, "openai-oauth");
        assert_eq!(profile.auth, "none");
        assert!(profile.base_url.is_none());
        assert!(profile.set_value("configured", "maybe").is_err());
        profile.unset_value("configured").expect("unset");
        assert!(!profile.configured);
    }

    #[test]
    fn named_profiles_are_isolated_and_active_selection_is_atomic() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path());
        let default_store = catalog.active_store().expect("default store");
        let default = ProviderProfile {
            configured: true,
            ..ProviderProfile::default()
        };
        default_store.save(&default).expect("save default");

        catalog.create("work").expect("create profile");
        let selected = catalog.use_profile("work").expect("select profile");
        assert!(selected.active);
        assert_eq!(catalog.active_name().expect("active"), "work");

        let mut work = catalog
            .active_store()
            .expect("work store")
            .load()
            .expect("load");
        work.set_value("model", "work-model").expect("set model");
        catalog
            .active_store()
            .expect("work store")
            .save(&work)
            .expect("save");

        catalog.use_profile("default").expect("select default");
        assert_eq!(
            catalog
                .active_store()
                .expect("default")
                .load()
                .expect("load"),
            default
        );
        assert!(catalog.delete("work").is_ok());
    }

    #[test]
    fn stale_active_profile_write_is_rejected_without_replacing_valid_state() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path());
        let snapshot = catalog.snapshot().expect("snapshot");

        let mut first = snapshot.profile.clone();
        first
            .set_value("model", "first-model")
            .expect("first model");
        let change = catalog
            .save_active_profile(
                &first,
                snapshot.revision,
                ConfigurationChangeOrigin::Cli,
                ["model".to_owned()],
                ConfigurationApplyTiming::Immediate,
            )
            .expect("first save");
        assert_eq!(change.revision, 1);

        let mut stale = snapshot.profile;
        stale
            .set_value("model", "stale-model")
            .expect("stale model");
        let error = catalog
            .save_active_profile(
                &stale,
                snapshot.revision,
                ConfigurationChangeOrigin::Desktop,
                ["model".to_owned()],
                ConfigurationApplyTiming::Immediate,
            )
            .expect_err("stale write");
        assert!(error.to_string().contains("current revision is 1"));
        assert_eq!(catalog.snapshot().expect("current").profile, first);
    }

    #[test]
    fn concurrent_writers_with_one_revision_have_one_winner() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path());
        let snapshot = catalog.snapshot().expect("snapshot");
        let barrier = Arc::new(Barrier::new(3));
        let revision = snapshot.revision;
        let mut handles = Vec::new();
        for model in ["model-a", "model-b"] {
            let catalog = catalog.clone();
            let barrier = Arc::clone(&barrier);
            let mut profile = snapshot.profile.clone();
            profile.set_value("model", model).expect("model");
            handles.push(thread::spawn(move || {
                barrier.wait();
                catalog.save_active_profile(
                    &profile,
                    revision,
                    ConfigurationChangeOrigin::System,
                    ["model".to_owned()],
                    ConfigurationApplyTiming::Immediate,
                )
            }));
        }
        barrier.wait();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("writer"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        assert_eq!(catalog.revision().expect("revision"), 1);
    }

    #[test]
    fn persisted_change_record_contains_keys_but_no_values() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path());
        let snapshot = catalog.snapshot().expect("snapshot");
        let mut profile = snapshot.profile;
        profile
            .set_value("model", "sensitive-looking-model-value")
            .expect("model");
        catalog
            .save_active_profile(
                &profile,
                snapshot.revision,
                ConfigurationChangeOrigin::Tui,
                ["model".to_owned()],
                ConfigurationApplyTiming::Immediate,
            )
            .expect("save");
        let state =
            fs::read_to_string(directory.path().join("configuration-state.toml")).expect("state");
        assert!(state.contains("model"));
        assert!(!state.contains("sensitive-looking-model-value"));
    }

    #[test]
    fn active_profile_cannot_be_deleted() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path());
        catalog.create("work").expect("create");
        catalog.use_profile("work").expect("use");
        assert!(catalog.delete("work").is_err());
    }
}
