use std::{
    env, fmt, fs,
    io::Write,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};

pub const PROVIDER_PROFILE_KEYS: [&str; 8] = [
    "connection",
    "provider",
    "model",
    "speed",
    "reasoning",
    "auth",
    "base_url",
    "configured",
];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderProfile {
    pub connection: String,
    pub provider: String,
    pub model: String,
    pub speed: String,
    pub reasoning: String,
    pub auth: String,
    pub base_url: Option<String>,
    pub configured: bool,
}

impl Default for ProviderProfile {
    fn default() -> Self {
        Self {
            connection: "direct".into(),
            provider: "minimax".into(),
            model: "MiniMax-M3".into(),
            speed: "balanced".into(),
            reasoning: "medium".into(),
            auth: "api-key".into(),
            base_url: None,
            configured: false,
        }
    }
}

impl ProviderProfile {
    pub fn validate(&self) -> MedusaResult<()> {
        require_one_of(
            "connection",
            &self.connection,
            &[
                "omniroute",
                "chatgpt-oauth",
                "openai-api",
                "openai-compatible",
                "direct",
                "local",
            ],
        )?;
        require_one_of(
            "speed",
            &self.speed,
            &["fast", "balanced", "quality", "custom"],
        )?;
        require_one_of(
            "reasoning",
            &self.reasoning,
            &["low", "medium", "high", "maximum"],
        )?;
        require_one_of(
            "auth",
            &self.auth,
            &["oauth", "api-key", "existing", "none"],
        )?;

        if self.configured && (self.provider.trim().is_empty() || self.model.trim().is_empty()) {
            return Err(config_error(
                "configured provider profiles require a provider and model",
            ));
        }

        if let Some(base_url) = self.base_url.as_deref()
            && (!base_url.starts_with("http://") && !base_url.starts_with("https://"))
        {
            return Err(config_error(
                "provider base_url must use http:// or https://",
            ));
        }

        if self.connection == "chatgpt-oauth"
            && (self.provider != "openai-oauth"
                || self.auth != "none"
                || self.base_url.as_deref() != Some("http://127.0.0.1:10531/v1"))
        {
            return Err(config_error(
                "ChatGPT OAuth must use the local openai-oauth route",
            ));
        }

        if self.connection == "openai-api"
            && (self.provider != "openai"
                || self.auth != "api-key"
                || self.base_url.as_deref() != Some("https://api.openai.com/v1"))
        {
            return Err(config_error(
                "OpenAI API configuration must use the official API route",
            ));
        }

        Ok(())
    }

    #[must_use]
    pub fn protocol(&self) -> &'static str {
        match self.connection.as_str() {
            "direct"
                if matches!(
                    self.provider.as_str(),
                    "anthropic" | "anthropic-compatible"
                ) =>
            {
                "anthropic"
            }
            _ => "openai",
        }
    }

    #[must_use]
    pub fn uses_openai_oauth(&self) -> bool {
        self.connection == "chatgpt-oauth"
    }

    #[must_use]
    pub fn value(&self, key: &str) -> Option<ProviderProfileValue> {
        match key {
            "connection" => Some(ProviderProfileValue::String(self.connection.clone())),
            "provider" => Some(ProviderProfileValue::String(self.provider.clone())),
            "model" => Some(ProviderProfileValue::String(self.model.clone())),
            "speed" => Some(ProviderProfileValue::String(self.speed.clone())),
            "reasoning" => Some(ProviderProfileValue::String(self.reasoning.clone())),
            "auth" => Some(ProviderProfileValue::String(self.auth.clone())),
            "base_url" => Some(
                self.base_url
                    .clone()
                    .map_or(ProviderProfileValue::Null, ProviderProfileValue::String),
            ),
            "configured" => Some(ProviderProfileValue::Bool(self.configured)),
            _ => None,
        }
    }

    fn normalize_legacy_route(mut self) -> Self {
        if self.connection == "chatgpt-oauth"
            && self.provider == "openai-oauth"
            && self.model == "MiniMax-M3"
        {
            self.connection = "direct".into();
            self.provider = "minimax".into();
            self.auth = "api-key".into();
            self.base_url = None;
            self.configured = true;
        }
        self
    }
}

/// Returns the registered environment-variable credential source for a provider.
#[must_use]
pub fn credential_environment(provider: &str) -> Option<&'static str> {
    match provider {
        "minimax" => Some("MINIMAX_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "anthropic-compatible" | "openai-compatible" => Some("MEDUSA_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        "openai-oauth" | "omniroute" | "local" => None,
        _ => None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ProviderProfileValue {
    String(String),
    Bool(bool),
    Null,
}

impl fmt::Display for ProviderProfileValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String(value) => formatter.write_str(value),
            Self::Bool(value) => value.fmt(formatter),
            Self::Null => formatter.write_str("null"),
        }
    }
}

/// Persisted provider-profile storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderProfileStore {
    path: PathBuf,
}

impl ProviderProfileStore {
    pub fn user() -> MedusaResult<Self> {
        let base = if cfg!(windows) {
            env::var_os("APPDATA").map(PathBuf::from)
        } else if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
            Some(PathBuf::from(path))
        } else {
            env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
        }
        .ok_or_else(|| {
            MedusaError::new(
                ErrorCode::InvalidConfiguration,
                ErrorCategory::Environment,
                "could not resolve the user configuration directory",
            )
        })?;
        Ok(Self::at(base.join("medusa").join("provider.toml")))
    }

    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> MedusaResult<ProviderProfile> {
        if !self.path.exists() {
            return Ok(ProviderProfile::default());
        }
        let text = fs::read_to_string(&self.path)
            .map_err(|error| store_error(format!("read {}: {error}", self.path.display())))?;
        let profile: ProviderProfile = toml::from_str(&text)
            .map_err(|error| store_error(format!("parse {}: {error}", self.path.display())))?;
        let profile = profile.normalize_legacy_route();
        profile.validate()?;
        Ok(profile)
    }

    pub fn save(&self, profile: &ProviderProfile) -> MedusaResult<()> {
        profile.validate()?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| store_error("configuration path has no parent"))?;
        fs::create_dir_all(parent)
            .map_err(|error| store_error(format!("create {}: {error}", parent.display())))?;
        let text =
            toml::to_string_pretty(profile).map_err(|error| store_error(error.to_string()))?;
        let temporary = self.path.with_extension("toml.tmp");
        let mut file = fs::File::create(&temporary)
            .map_err(|error| store_error(format!("write {}: {error}", temporary.display())))?;
        file.write_all(text.as_bytes())
            .map_err(|error| store_error(format!("write {}: {error}", temporary.display())))?;
        file.sync_all()
            .map_err(|error| store_error(format!("sync {}: {error}", temporary.display())))?;
        fs::rename(&temporary, &self.path)
            .map_err(|error| store_error(format!("replace {}: {error}", self.path.display())))?;
        sync_parent(&self.path);
        Ok(())
    }

    pub fn reset(&self) -> MedusaResult<bool> {
        if !self.path.exists() {
            return Ok(false);
        }
        fs::remove_file(&self.path)
            .map_err(|error| store_error(format!("remove {}: {error}", self.path.display())))?;
        sync_parent(&self.path);
        Ok(true)
    }
}

fn require_one_of(field: &str, value: &str, allowed: &[&str]) -> MedusaResult<()> {
    if allowed.contains(&value) {
        return Ok(());
    }
    Err(config_error(format!(
        "{field} must be one of {}",
        allowed.join(", ")
    )))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_preserve_existing_first_run_contract() {
        let profile = ProviderProfile::default();
        profile.validate().expect("defaults");
        assert_eq!(profile.connection, "direct");
        assert_eq!(profile.provider, "minimax");
        assert_eq!(profile.model, "MiniMax-M3");
        assert!(!profile.configured);
    }

    #[test]
    fn oauth_route_is_typed_and_secret_free() {
        let profile = ProviderProfile {
            connection: "chatgpt-oauth".into(),
            provider: "openai-oauth".into(),
            model: "gpt-5".into(),
            auth: "none".into(),
            base_url: Some("http://127.0.0.1:10531/v1".into()),
            configured: true,
            ..ProviderProfile::default()
        };
        profile.validate().expect("oauth profile");
        assert!(profile.uses_openai_oauth());
        let encoded = toml::to_string(&profile).expect("serialize");
        assert!(!encoded.to_ascii_lowercase().contains("token"));
        assert!(!encoded.to_ascii_lowercase().contains("api_key"));
    }

    #[test]
    fn legacy_oauth_minimax_route_is_restored_to_direct_api_key() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("provider.toml");
        fs::write(
            &path,
            "connection = 'chatgpt-oauth'\nprovider = 'openai-oauth'\nmodel = 'MiniMax-M3'\nspeed = 'balanced'\nreasoning = 'medium'\nauth = 'none'\nbase_url = 'http://127.0.0.1:10531/v1'\nconfigured = true\n",
        )
        .expect("legacy route");
        let profile = ProviderProfileStore::at(path)
            .load()
            .expect("normalized profile");
        assert_eq!(profile.connection, "direct");
        assert_eq!(profile.provider, "minimax");
        assert_eq!(profile.model, "MiniMax-M3");
        assert_eq!(profile.auth, "api-key");
        assert!(profile.base_url.is_none());
        assert!(profile.configured);
    }

    #[test]
    fn store_round_trips_and_resets_atomically() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = ProviderProfileStore::at(directory.path().join("provider.toml"));
        let profile = ProviderProfile {
            configured: true,
            ..ProviderProfile::default()
        };
        store.save(&profile).expect("save");
        assert_eq!(store.load().expect("load"), profile);
        assert!(store.reset().expect("reset"));
        assert_eq!(store.load().expect("default"), ProviderProfile::default());
    }

    #[test]
    fn unknown_profile_keys_are_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("provider.toml");
        fs::write(
            &path,
            "connection = 'direct'\nprovider = 'minimax'\nmodel = 'MiniMax-M3'\nspeed = 'balanced'\nreasoning = 'medium'\nauth = 'api-key'\nconfigured = true\nsecret = 'nope'\n",
        )
        .expect("write");
        let error = ProviderProfileStore::at(path)
            .load()
            .expect_err("unknown field");
        assert!(error.to_string().contains("unknown field"));
    }
}
