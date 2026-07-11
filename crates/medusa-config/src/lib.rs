//! Typed configuration with deterministic precedence.

use std::{collections::BTreeMap, fs, path::Path};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};

/// Current configuration schema version.
pub const CONFIG_VERSION: u16 = 1;

/// Execution mode.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Yolo,
    Review,
    ReadOnly,
}

/// Runtime backend.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeBackend {
    Auto,
    Host,
    Container,
    Remote,
}

/// Root configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub version: u16,
    pub agent: AgentConfig,
    pub model: ModelConfig,
    pub runtime: RuntimeConfig,
    pub git: GitConfig,
    pub memory: MemoryConfig,
    pub verification: VerificationConfig,
}

/// Agent settings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    pub mode: Mode,
    pub max_turns: u32,
    pub parallel_workers: u16,
    pub ask_policy: String,
}

/// Model settings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelConfig {
    pub provider: String,
    pub name: String,
    pub protocol: String,
    pub temperature_milli: u16,
    pub max_output_tokens: u32,
}

/// Runtime settings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeConfig {
    pub backend: RuntimeBackend,
    pub network: String,
    pub process_limit: u32,
}

/// Git settings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitConfig {
    pub auto_commit: bool,
    pub allow_force_push: bool,
    pub protect_dirty_tree: bool,
}

/// Memory settings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryConfig {
    pub enabled: bool,
    pub format: String,
    pub auto_promote_low_risk: bool,
}

/// Verification settings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct VerificationConfig {
    pub required: bool,
    pub independent_review: bool,
    pub browser_on_ui_change: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            agent: AgentConfig::default(),
            model: ModelConfig::default(),
            runtime: RuntimeConfig::default(),
            git: GitConfig::default(),
            memory: MemoryConfig::default(),
            verification: VerificationConfig::default(),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self { mode: Mode::Yolo, max_turns: 500, parallel_workers: 4, ask_policy: "only_irreducible".into() }
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self { provider: "minimax".into(), name: "MiniMax-M3".into(), protocol: "anthropic".into(), temperature_milli: 200, max_output_tokens: 32_768 }
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self { backend: RuntimeBackend::Auto, network: "allowlist".into(), process_limit: 512 }
    }
}

impl Default for GitConfig {
    fn default() -> Self {
        Self { auto_commit: true, allow_force_push: false, protect_dirty_tree: true }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self { enabled: true, format: "markdown".into(), auto_promote_low_risk: true }
    }
}

impl Default for VerificationConfig {
    fn default() -> Self {
        Self { required: true, independent_review: true, browser_on_ui_change: true }
    }
}

impl Config {
    /// Parses and validates a TOML document.
    pub fn from_toml(text: &str) -> MedusaResult<Self> {
        let config: Self = toml::from_str(text).map_err(|error| invalid(error.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Loads user, project, environment, and CLI layers in increasing precedence.
    pub fn load_layers(
        user: Option<&Path>,
        project: Option<&Path>,
        environment: &BTreeMap<String, String>,
        cli: &BTreeMap<String, String>,
    ) -> MedusaResult<Self> {
        let mut value = toml::Value::try_from(Self::default()).map_err(|error| invalid(error.to_string()))?;
        if let Some(path) = user { merge_file(&mut value, path)?; }
        if let Some(path) = project { merge_file(&mut value, path)?; }
        apply_overrides(&mut value, environment)?;
        apply_overrides(&mut value, cli)?;
        let config: Self = value.try_into().map_err(|error| invalid(error.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Validates safety-sensitive invariants.
    pub fn validate(&self) -> MedusaResult<()> {
        if self.version != CONFIG_VERSION { return Err(invalid(format!("unsupported config version {}", self.version))); }
        if self.agent.max_turns == 0 || self.agent.parallel_workers == 0 { return Err(invalid("agent limits must be greater than zero")); }
        if self.model.temperature_milli > 1_000 { return Err(invalid("temperature_milli must be at most 1000")); }
        if self.git.allow_force_push { return Err(invalid("force push cannot be enabled by the built-in schema")); }
        if self.memory.format != "markdown" { return Err(invalid("memory format must remain markdown")); }
        Ok(())
    }
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidConfiguration, ErrorCategory::Validation, message)
}

fn merge_file(base: &mut toml::Value, path: &Path) -> MedusaResult<()> {
    let text = fs::read_to_string(path).map_err(|error| invalid(format!("read {}: {error}", path.display())))?;
    let overlay: toml::Value = toml::from_str(&text).map_err(|error| invalid(format!("parse {}: {error}", path.display())))?;
    merge(base, overlay);
    Ok(())
}

fn merge(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                if let Some(existing) = base.get_mut(&key) { merge(existing, value); } else { base.insert(key, value); }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

fn apply_overrides(root: &mut toml::Value, values: &BTreeMap<String, String>) -> MedusaResult<()> {
    for (path, raw) in values {
        let value = raw.parse::<toml::Value>().unwrap_or_else(|_| toml::Value::String(raw.clone()));
        set_path(root, path, value)?;
    }
    Ok(())
}

fn set_path(root: &mut toml::Value, path: &str, value: toml::Value) -> MedusaResult<()> {
    let parts: Vec<_> = path.split('.').collect();
    if parts.is_empty() || parts.iter().any(|part| part.is_empty()) { return Err(invalid("override path cannot be empty")); }
    let mut current = root;
    for part in &parts[..parts.len() - 1] {
        let table = current.as_table_mut().ok_or_else(|| invalid("override traverses a scalar"))?;
        current = table.entry((*part).to_owned()).or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    }
    current.as_table_mut().ok_or_else(|| invalid("override parent is a scalar"))?.insert(parts[parts.len() - 1].to_owned(), value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate() {
        Config::default().validate().expect("defaults");
    }

    #[test]
    fn unknown_fields_fail_closed() {
        assert!(Config::from_toml("version = 1\nunknown = true").is_err());
    }

    #[test]
    fn precedence_is_cli_environment_project_user_defaults() {
        let directory = tempfile::tempdir().expect("tempdir");
        let user = directory.path().join("user.toml");
        let project = directory.path().join("project.toml");
        fs::write(&user, "[agent]\nmax_turns = 100\n").expect("user config");
        fs::write(&project, "[agent]\nmax_turns = 200\n").expect("project config");
        let environment = BTreeMap::from([("agent.max_turns".into(), "300".into())]);
        let cli = BTreeMap::from([("agent.max_turns".into(), "400".into())]);
        let config = Config::load_layers(Some(&user), Some(&project), &environment, &cli).expect("layered config");
        assert_eq!(config.agent.max_turns, 400);
    }

    #[test]
    fn force_push_fails_closed() {
        assert!(Config::from_toml("version = 1\n[git]\nallow_force_push = true\n").is_err());
    }
}
