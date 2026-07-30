use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};

use super::{PROVIDER_PROFILE_KEYS, ProviderProfile, ProviderProfileStore};

const ACTIVE_PROFILE_SCHEMA_VERSION: u32 = 1;
const DEFAULT_PROFILE_NAME: &str = "default";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ActiveProfileSelection {
    schema_version: u32,
    name: String,
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

    pub fn create(&self, name: &str) -> MedusaResult<ProviderProfileSummary> {
        validate_named_profile(name)?;
        let path = self.named_profile_path(name);
        if path.exists() {
            return Err(config_error(format!(
                "provider profile `{name}` already exists"
            )));
        }
        let profile = self.active_store()?.load()?;
        let store = ProviderProfileStore::at(path);
        store.save(&profile)?;
        Ok(ProviderProfileSummary {
            name: name.to_owned(),
            active: false,
            configured: profile.configured,
            provider: profile.provider,
            model: profile.model,
        })
    }

    pub fn use_profile(&self, name: &str) -> MedusaResult<ProviderProfileSummary> {
        validate_profile_name(name)?;
        let store = self.store_for_name(name);
        if name != DEFAULT_PROFILE_NAME && !store.path().is_file() {
            return Err(config_error(format!(
                "provider profile `{name}` does not exist"
            )));
        }
        let profile = store.load()?;
        if name == DEFAULT_PROFILE_NAME {
            remove_if_exists(&self.selection_path())?;
        } else {
            let selection = ActiveProfileSelection {
                schema_version: ACTIVE_PROFILE_SCHEMA_VERSION,
                name: name.to_owned(),
            };
            let text = toml::to_string_pretty(&selection)
                .map_err(|error| store_error(error.to_string()))?;
            atomic_write(&self.selection_path(), text.as_bytes())?;
        }
        Ok(ProviderProfileSummary {
            name: name.to_owned(),
            active: true,
            configured: profile.configured,
            provider: profile.provider,
            model: profile.model,
        })
    }

    pub fn delete(&self, name: &str) -> MedusaResult<()> {
        validate_named_profile(name)?;
        if self.active_name()? == name {
            return Err(config_error(format!(
                "cannot delete active provider profile `{name}`; select another profile first"
            )));
        }
        let path = self.named_profile_path(name);
        if !path.is_file() {
            return Err(config_error(format!(
                "provider profile `{name}` does not exist"
            )));
        }
        fs::remove_file(&path)
            .map_err(|error| store_error(format!("remove {}: {error}", path.display())))?;
        sync_parent(&path);
        Ok(())
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
                        self.base_url = Some("http://127.0.0.1:10531/v1".to_owned());
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
    let parent = path
        .parent()
        .ok_or_else(|| store_error("configuration path has no parent"))?;
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
        assert_eq!(
            profile.base_url.as_deref(),
            Some("http://127.0.0.1:10531/v1")
        );
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
    fn active_profile_cannot_be_deleted() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path());
        catalog.create("work").expect("create");
        catalog.use_profile("work").expect("use");
        assert!(catalog.delete("work").is_err());
    }
}
