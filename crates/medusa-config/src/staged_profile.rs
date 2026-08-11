use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};

use super::{
    PROVIDER_PROFILE_KEYS, ConfigurationApplyTiming, ConfigurationChangeOrigin,
    ConfigurationChanged, ProviderProfile, ProviderProfileCatalog, ProviderProfileSnapshot,
    ProviderProfileStore, ProviderProfileValue,
    configuration_state::ConfigurationStateStore,
};

const HISTORY_SCHEMA_VERSION: u32 = 1;
const TRANSACTION_SCHEMA_VERSION: u32 = 1;
const HISTORY_LIMIT: usize = 8;
const DEFAULT_PROFILE_NAME: &str = "default";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderProfileSection {
    Connection,
    Preferences,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProviderProfileDiffEntry {
    pub key: String,
    pub before: ProviderProfileValue,
    pub after: ProviderProfileValue,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderProfileHistoryEntry {
    schema_version: u32,
    pub revision: u64,
    pub active_profile: String,
    pub profile: ProviderProfile,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedProviderProfile {
    base_revision: u64,
    active_profile: String,
    original: ProviderProfile,
    candidate: ProviderProfile,
}

impl StagedProviderProfile {
    #[must_use]
    pub fn from_snapshot(snapshot: ProviderProfileSnapshot) -> Self {
        Self {
            base_revision: snapshot.revision,
            active_profile: snapshot.active_profile,
            original: snapshot.profile.clone(),
            candidate: snapshot.profile,
        }
    }

    #[must_use]
    pub const fn base_revision(&self) -> u64 {
        self.base_revision
    }

    #[must_use]
    pub fn active_profile(&self) -> &str {
        &self.active_profile
    }

    #[must_use]
    pub fn original(&self) -> &ProviderProfile {
        &self.original
    }

    #[must_use]
    pub fn candidate(&self) -> &ProviderProfile {
        &self.candidate
    }

    pub fn set(&mut self, key: &str, value: &str) -> MedusaResult<()> {
        let mut candidate = self.candidate.clone();
        candidate.set_value(key, value)?;
        self.candidate = candidate;
        Ok(())
    }

    pub fn unset(&mut self, key: &str) -> MedusaResult<()> {
        let mut candidate = self.candidate.clone();
        candidate.unset_value(key)?;
        self.candidate = candidate;
        Ok(())
    }

    pub fn replace(&mut self, profile: ProviderProfile) -> MedusaResult<()> {
        profile.validate()?;
        self.candidate = profile;
        Ok(())
    }

    pub fn reset_section(&mut self, section: ProviderProfileSection) -> MedusaResult<()> {
        let defaults = ProviderProfile::default();
        match section {
            ProviderProfileSection::Connection => {
                self.candidate.connection = defaults.connection;
                self.candidate.provider = defaults.provider;
                self.candidate.model = defaults.model;
                self.candidate.auth = defaults.auth;
                self.candidate.base_url = defaults.base_url;
                self.candidate.configured = defaults.configured;
            }
            ProviderProfileSection::Preferences => {
                self.candidate.speed = defaults.speed;
                self.candidate.reasoning = defaults.reasoning;
            }
        }
        self.candidate.validate()
    }

    pub fn reset_all(&mut self) {
        self.candidate = ProviderProfile::default();
    }

    pub fn discard(&mut self) {
        self.candidate = self.original.clone();
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.candidate != self.original
    }

    #[must_use]
    pub fn diff(&self) -> Vec<ProviderProfileDiffEntry> {
        PROVIDER_PROFILE_KEYS
            .iter()
            .filter_map(|key| {
                let before = self.original.value(key)?;
                let after = self.candidate.value(key)?;
                (before != after).then(|| ProviderProfileDiffEntry {
                    key: (*key).to_owned(),
                    before,
                    after,
                })
            })
            .collect()
    }

    #[must_use]
    pub fn render_diff(&self) -> String {
        self.diff()
            .into_iter()
            .map(|entry| format!("{}: {} -> {}", entry.key, entry.before, entry.after))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn validate(&self) -> MedusaResult<()> {
        self.candidate.validate()
    }

    pub fn commit(
        self,
        catalog: &ProviderProfileCatalog,
        origin: ConfigurationChangeOrigin,
        apply_timing: ConfigurationApplyTiming,
    ) -> MedusaResult<ConfigurationChanged> {
        self.validate()?;
        let changed_keys = self
            .diff()
            .into_iter()
            .map(|entry| entry.key)
            .collect::<Vec<_>>();
        if changed_keys.is_empty() {
            return Err(config_error("staged configuration has no changes to apply"));
        }
        catalog.save_active_profile(
            &self.candidate,
            self.base_revision,
            origin,
            changed_keys,
            apply_timing,
        )
    }
}

impl ProviderProfileCatalog {
    pub fn stage_active_profile(&self) -> MedusaResult<StagedProviderProfile> {
        Ok(StagedProviderProfile::from_snapshot(self.snapshot()?))
    }

    pub fn stage_active_profile_at_revision(
        &self,
        expected_revision: u64,
    ) -> MedusaResult<StagedProviderProfile> {
        let staged = self.stage_active_profile()?;
        if staged.base_revision() != expected_revision {
            return Err(config_error(format!(
                "stale configuration revision {expected_revision}; current revision is {}; reload configuration and retry",
                staged.base_revision()
            )));
        }
        Ok(staged)
    }

    pub fn active_profile_history(&self) -> MedusaResult<Vec<ProviderProfileHistoryEntry>> {
        reconcile_pending_transaction(self.root())?;
        let active_profile = self.active_name()?;
        load_history(self.root(), &active_profile)
    }

    pub fn restore_previous_active_profile(
        &self,
        expected_revision: u64,
        origin: ConfigurationChangeOrigin,
    ) -> MedusaResult<ConfigurationChanged> {
        let mut staged = self.stage_active_profile_at_revision(expected_revision)?;
        let history = load_history(self.root(), staged.active_profile())?;
        let previous = history
            .into_iter()
            .find(|entry| entry.revision < expected_revision)
            .ok_or_else(|| config_error("no previous known-good provider profile is available"))?;
        staged.replace(previous.profile)?;
        staged.commit(self, origin, ConfigurationApplyTiming::NextSession)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingProfileTransaction {
    schema_version: u32,
    expected_revision: u64,
    active_profile: String,
    previous_existed: bool,
    previous: ProviderProfile,
    candidate: ProviderProfile,
}

pub(crate) fn record_known_good(
    root: &Path,
    revision: u64,
    active_profile: &str,
    profile: &ProviderProfile,
) -> MedusaResult<()> {
    profile.validate()?;
    let directory = history_directory(root, active_profile);
    fs::create_dir_all(&directory)
        .map_err(|error| store_error(format!("create {}: {error}", directory.display())))?;
    let path = directory.join(format!("revision-{revision:020}.toml"));
    let entry = ProviderProfileHistoryEntry {
        schema_version: HISTORY_SCHEMA_VERSION,
        revision,
        active_profile: active_profile.to_owned(),
        profile: profile.clone(),
    };
    let text = toml::to_string_pretty(&entry).map_err(|error| store_error(error.to_string()))?;
    atomic_write(&path, text.as_bytes())?;
    prune_history(&directory)
}

pub(crate) fn begin_pending_transaction(
    root: &Path,
    expected_revision: u64,
    active_profile: &str,
    previous_existed: bool,
    previous: &ProviderProfile,
    candidate: &ProviderProfile,
) -> MedusaResult<()> {
    previous.validate()?;
    candidate.validate()?;
    let transaction = PendingProfileTransaction {
        schema_version: TRANSACTION_SCHEMA_VERSION,
        expected_revision,
        active_profile: active_profile.to_owned(),
        previous_existed,
        previous: previous.clone(),
        candidate: candidate.clone(),
    };
    let text =
        toml::to_string_pretty(&transaction).map_err(|error| store_error(error.to_string()))?;
    atomic_write(&transaction_path(root), text.as_bytes())
}

pub(crate) fn finish_pending_transaction(root: &Path) -> MedusaResult<()> {
    remove_if_exists(&transaction_path(root))
}

pub(crate) fn reconcile_pending_transaction(root: &Path) -> MedusaResult<()> {
    let path = transaction_path(root);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(store_error(format!("read {}: {error}", path.display()))),
    };
    let transaction: PendingProfileTransaction = toml::from_str(&text)
        .map_err(|error| store_error(format!("parse {}: {error}", path.display())))?;
    if transaction.schema_version != TRANSACTION_SCHEMA_VERSION {
        return Err(store_error(format!(
            "unsupported configuration transaction schema version {}",
            transaction.schema_version
        )));
    }
    transaction.previous.validate()?;
    transaction.candidate.validate()?;

    let state = ConfigurationStateStore::at(root);
    let revision = state.revision()?;
    let store = ProviderProfileStore::at(profile_path(root, &transaction.active_profile));
    if revision == transaction.expected_revision {
        if transaction.previous_existed {
            store.save(&transaction.previous)?;
        } else {
            store.reset()?;
        }
    } else if revision == transaction.expected_revision.saturating_add(1) {
        let current = store.load()?;
        if current != transaction.candidate {
            return Err(store_error(
                "configuration transaction metadata committed but the active profile does not match the committed candidate",
            ));
        }
    } else {
        return Err(store_error(format!(
            "configuration transaction cannot be reconciled safely: expected revision {} or {}, found {revision}",
            transaction.expected_revision,
            transaction.expected_revision.saturating_add(1)
        )));
    }
    finish_pending_transaction(root)
}

fn load_history(root: &Path, active_profile: &str) -> MedusaResult<Vec<ProviderProfileHistoryEntry>> {
    let directory = history_directory(root, active_profile);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(store_error(format!("read {}: {error}", directory.display())));
        }
    };
    let mut history = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| store_error(error.to_string()))?;
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
        let text = fs::read_to_string(&path)
            .map_err(|error| store_error(format!("read {}: {error}", path.display())))?;
        let item: ProviderProfileHistoryEntry = toml::from_str(&text)
            .map_err(|error| store_error(format!("parse {}: {error}", path.display())))?;
        if item.schema_version != HISTORY_SCHEMA_VERSION || item.active_profile != active_profile {
            return Err(store_error(format!(
                "configuration history entry {} has invalid scope or schema",
                path.display()
            )));
        }
        item.profile.validate()?;
        history.push(item);
    }
    history.sort_by(|left, right| right.revision.cmp(&left.revision));
    Ok(history)
}

fn prune_history(directory: &Path) -> MedusaResult<()> {
    let mut paths = fs::read_dir(directory)
        .map_err(|error| store_error(format!("read {}: {error}", directory.display())))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("toml"))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();
    let remove_count = paths.len().saturating_sub(HISTORY_LIMIT);
    for path in paths.into_iter().take(remove_count) {
        fs::remove_file(&path)
            .map_err(|error| store_error(format!("remove {}: {error}", path.display())))?;
    }
    sync_parent(directory);
    Ok(())
}

fn history_directory(root: &Path, active_profile: &str) -> PathBuf {
    root.join("profile-history").join(active_profile)
}

fn transaction_path(root: &Path) -> PathBuf {
    root.join(".configuration-transaction.toml")
}

fn profile_path(root: &Path, active_profile: &str) -> PathBuf {
    if active_profile == DEFAULT_PROFILE_NAME {
        root.join("provider.toml")
    } else {
        root.join("profiles").join(format!("{active_profile}.toml"))
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> MedusaResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| store_error("configuration persistence path has no parent"))?;
    fs::create_dir_all(parent)
        .map_err(|error| store_error(format!("create {}: {error}", parent.display())))?;
    let temporary = path.with_extension("toml.tmp");
    let mut file = fs::File::create(&temporary)
        .map_err(|error| store_error(format!("write {}: {error}", temporary.display())))?;
    file.write_all(bytes)
        .map_err(|error| store_error(format!("write {}: {error}", temporary.display())))?;
    file.sync_all()
        .map_err(|error| store_error(format!("sync {}: {error}", temporary.display())))?;
    fs::rename(&temporary, path)
        .map_err(|error| store_error(format!("replace {}: {error}", path.display())))?;
    sync_parent(path);
    Ok(())
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

    fn configured_profile(model: &str) -> ProviderProfile {
        ProviderProfile {
            model: model.to_owned(),
            configured: true,
            ..ProviderProfile::default()
        }
    }

    #[test]
    fn staged_edits_diff_discard_and_section_reset_are_typed() {
        let snapshot = ProviderProfileSnapshot {
            revision: 4,
            active_profile: "default".to_owned(),
            profile: configured_profile("before"),
        };
        let mut staged = StagedProviderProfile::from_snapshot(snapshot);
        staged.set("model", "after").expect("set model");
        staged.set("speed", "quality").expect("set speed");
        assert_eq!(staged.diff().len(), 2);
        assert!(staged.render_diff().contains("model: before -> after"));
        staged
            .reset_section(ProviderProfileSection::Preferences)
            .expect("reset preferences");
        assert_eq!(staged.candidate().speed, ProviderProfile::default().speed);
        staged.discard();
        assert!(!staged.is_dirty());
    }

    #[test]
    fn commit_records_bounded_secret_free_history_and_restores_previous() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path());
        catalog
            .active_store()
            .expect("store")
            .save(&configured_profile("first"))
            .expect("seed");

        let mut staged = catalog.stage_active_profile().expect("stage");
        staged.set("model", "second").expect("set");
        let change = staged
            .commit(
                &catalog,
                ConfigurationChangeOrigin::Cli,
                ConfigurationApplyTiming::NextSession,
            )
            .expect("commit");
        assert_eq!(change.revision, 1);
        let history = catalog.active_profile_history().expect("history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].revision, 0);
        assert_eq!(history[0].profile.model, "first");

        catalog
            .restore_previous_active_profile(1, ConfigurationChangeOrigin::Cli)
            .expect("restore");
        assert_eq!(catalog.snapshot().expect("snapshot").profile.model, "first");
        let encoded = toml::to_string(&history[0]).expect("history encode");
        assert!(!encoded.to_ascii_lowercase().contains("token"));
        assert!(!encoded.to_ascii_lowercase().contains("api_key"));
    }

    #[test]
    fn pending_transaction_recovers_pre_metadata_crash_and_accepts_post_metadata_state() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path();
        let store = ProviderProfileStore::at(root.join("provider.toml"));
        let before = configured_profile("before");
        let after = configured_profile("after");
        store.save(&before).expect("seed");

        begin_pending_transaction(root, 0, "default", true, &before, &after)
            .expect("transaction");
        store.save(&after).expect("partial profile write");
        reconcile_pending_transaction(root).expect("recover");
        assert_eq!(store.load().expect("load"), before);

        begin_pending_transaction(root, 0, "default", true, &before, &after)
            .expect("transaction two");
        store.save(&after).expect("profile write");
        let mut guard = ConfigurationStateStore::at(root).acquire().expect("guard");
        guard
            .commit(
                "default".to_owned(),
                ["model".to_owned()],
                ConfigurationChangeOrigin::Cli,
                ConfigurationApplyTiming::NextSession,
            )
            .expect("metadata");
        drop(guard);
        reconcile_pending_transaction(root).expect("finish committed");
        assert_eq!(store.load().expect("load"), after);
    }

    #[test]
    fn history_is_bounded() {
        let directory = tempfile::tempdir().expect("tempdir");
        for revision in 0..12 {
            record_known_good(
                directory.path(),
                revision,
                "default",
                &configured_profile(&format!("model-{revision}")),
            )
            .expect("record");
        }
        let history = load_history(directory.path(), "default").expect("history");
        assert_eq!(history.len(), HISTORY_LIMIT);
        assert_eq!(history[0].revision, 11);
        assert_eq!(history.last().expect("last").revision, 4);
    }
}
