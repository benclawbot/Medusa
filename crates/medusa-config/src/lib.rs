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
    pub context_window_tokens: u64,
    pub auto_compact_percent: u8,
    pub base_url: Option<String>,
    pub auth: String,
    pub speed: String,
    pub reasoning: String,
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
        Self {
            mode: Mode::Yolo,
            max_turns: 500,
            parallel_workers: 4,
            ask_policy: "only_irreducible".into(),
        }
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: "minimax".into(),
            name: "MiniMax-M3".into(),
            protocol: "anthropic".into(),
            temperature_milli: 200,
            max_output_tokens: 32_768,
            context_window_tokens: 1_000_000,
            auto_compact_percent: 40,
            base_url: None,
            auth: "api-key".into(),
            speed: "balanced".into(),
            reasoning: "medium".into(),
        }
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            backend: RuntimeBackend::Auto,
            network: "allowlist".into(),
            process_limit: 512,
        }
    }
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            auto_commit: true,
            allow_force_push: false,
            protect_dirty_tree: true,
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            format: "markdown".into(),
            auto_promote_low_risk: true,
        }
    }
}

impl Default for VerificationConfig {
    fn default() -> Self {
        Self {
            required: true,
            independent_review: true,
            browser_on_ui_change: true,
        }
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
        let mut value =
            toml::Value::try_from(Self::default()).map_err(|error| invalid(error.to_string()))?;
        merge_provider_profile(&mut value)?;
        merge_provider_profile(&mut value)?;
        if let Some(path) = user {
            merge_file(&mut value, path)?;
        }
        if let Some(path) = project {
            merge_file(&mut value, path)?;
        }
        apply_overrides(&mut value, environment)?;
        apply_overrides(&mut value, cli)?;
        let config: Self = value
            .try_into()
            .map_err(|error| invalid(error.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Validates safety-sensitive invariants.
    pub fn validate(&self) -> MedusaResult<()> {
        if self.version != CONFIG_VERSION {
            return Err(invalid(format!(
                "unsupported config version {}",
                self.version
            )));
        }
        if self.agent.max_turns == 0 || self.agent.parallel_workers == 0 {
            return Err(invalid("agent limits must be greater than zero"));
        }
        if self.model.temperature_milli > 1_000 {
            return Err(invalid("temperature_milli must be at most 1000"));
        }
        if self.model.context_window_tokens == 0 {
            return Err(invalid("context_window_tokens must be greater than zero"));
        }
        if !(1..=100).contains(&self.model.auto_compact_percent) {
            return Err(invalid("auto_compact_percent must be between 1 and 100"));
        }
        if self.git.allow_force_push {
            return Err(invalid(
                "force push cannot be enabled by the built-in schema",
            ));
        }
        if self.memory.format != "markdown" {
            return Err(invalid("memory format must remain markdown"));
        }
        Ok(())
    }
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ProviderProfile {
    connection: String,
    provider: String,
    model: String,
    speed: String,
    reasoning: String,
    auth: String,
    base_url: Option<String>,
    configured: bool,
}

fn provider_profile_path() -> Option<std::path::PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(std::path::PathBuf::from)
    } else if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        Some(std::path::PathBuf::from(path))
    } else {
        std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config"))
    }?;
    Some(base.join("medusa").join("provider.toml"))
}

fn merge_provider_profile(base: &mut toml::Value) -> MedusaResult<()> {
    let Some(path) = provider_profile_path() else {
        return Ok(());
    };
    if !path.exists() {
        return Ok(());
    }
    let text = fs::read_to_string(&path)
        .map_err(|error| invalid(format!("read {}: {error}", path.display())))?;
    let profile: ProviderProfile = toml::from_str(&text)
        .map_err(|error| invalid(format!("parse {}: {error}", path.display())))?;
    if !profile.configured {
        return Ok(());
    }
    let protocol = match profile.connection.as_str() {
        "direct"
            if matches!(
                profile.provider.as_str(),
                "minimax" | "anthropic" | "anthropic-compatible"
            ) =>
        {
            "anthropic"
        }
        _ => "openai",
    };
    let overlay = toml::Value::try_from(toml::toml! {
        [model]
        provider = profile.provider
        name = profile.model
        protocol = protocol
        auth = profile.auth
        speed = profile.speed
        reasoning = profile.reasoning
    })
    .map_err(|error| invalid(error.to_string()))?;
    merge(base, overlay);
    if let Some(url) = profile.base_url {
        set_path(base, "model.base_url", toml::Value::String(url))?;
    }
    Ok(())
}

fn merge_file(base: &mut toml::Value, path: &Path) -> MedusaResult<()> {
    let text = fs::read_to_string(path)
        .map_err(|error| invalid(format!("read {}: {error}", path.display())))?;
    let overlay: toml::Value = toml::from_str(&text)
        .map_err(|error| invalid(format!("parse {}: {error}", path.display())))?;
    merge(base, overlay);
    Ok(())
}

fn merge(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                if let Some(existing) = base.get_mut(&key) {
                    merge(existing, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

fn apply_overrides(root: &mut toml::Value, values: &BTreeMap<String, String>) -> MedusaResult<()> {
    for (path, raw) in values {
        set_path(root, path, parse_override_value(raw)?)?;
    }
    Ok(())
}

fn parse_override_value(raw: &str) -> MedusaResult<toml::Value> {
    let document = format!("value = {raw}");
    match toml::from_str::<toml::Value>(&document) {
        Ok(toml::Value::Table(mut table)) => table
            .remove("value")
            .ok_or_else(|| invalid("override parser produced no value")),
        Ok(_) => Err(invalid("override parser produced a non-table document")),
        Err(_) => Ok(toml::Value::String(raw.to_owned())),
    }
}

fn set_path(root: &mut toml::Value, path: &str, value: toml::Value) -> MedusaResult<()> {
    let parts: Vec<_> = path.split('.').collect();
    if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
        return Err(invalid("override path cannot be empty"));
    }
    let mut current = root;
    for part in &parts[..parts.len() - 1] {
        let table = current
            .as_table_mut()
            .ok_or_else(|| invalid("override traverses a scalar"))?;
        current = table
            .entry((*part).to_owned())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    }
    current
        .as_table_mut()
        .ok_or_else(|| invalid("override parent is a scalar"))?
        .insert(parts[parts.len() - 1].to_owned(), value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate() {
        let config = Config::default();
        config.validate().expect("defaults");
        assert_eq!(config.model.context_window_tokens, 1_000_000);
        assert_eq!(config.model.auto_compact_percent, 40);
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
        let cli = BTreeMap::from([
            ("agent.max_turns".into(), "400".into()),
            ("verification.required".into(), "false".into()),
        ]);
        let config = Config::load_layers(Some(&user), Some(&project), &environment, &cli)
            .expect("layered config");
        assert_eq!(config.agent.max_turns, 400);
        assert!(!config.verification.required);
    }

    #[test]
    fn unquoted_override_text_remains_a_string() {
        assert_eq!(
            parse_override_value("only_irreducible").expect("string override"),
            toml::Value::String("only_irreducible".into())
        );
    }

    #[test]
    fn force_push_fails_closed() {
        assert!(Config::from_toml("version = 1\n[git]\nallow_force_push = true\n").is_err());
    }
}

/// Environment-variable overrides for browser and envelope configuration.
///
/// All functions are pure reads of the current process environment; tests
/// are responsible for unsetting the variables they touch so they don't
/// leak state between cases.
pub mod env {
    use std::path::PathBuf;
    use std::time::Duration;

    #[must_use]
    pub fn browser_enabled() -> bool {
        match std::env::var("MEDUSA_BROWSER_ENABLED") {
            Ok(s) => matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"),
            Err(_) => false,
        }
    }

    #[must_use]
    pub fn browser_path() -> Option<PathBuf> {
        std::env::var("MEDUSA_BROWSER_PATH").ok().map(PathBuf::from)
    }

    #[must_use]
    pub fn browser_timeout() -> Duration {
        Duration::from_millis(browser_timeout_ms())
    }

    #[must_use]
    pub fn browser_timeout_ms() -> u64 {
        std::env::var("MEDUSA_BROWSER_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30_000)
    }

    #[must_use]
    pub fn envelope_head_bytes() -> usize {
        std::env::var("MEDUSA_ENVELOPE_HEAD_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4_096)
    }

    #[must_use]
    pub fn envelope_tail_bytes() -> usize {
        std::env::var("MEDUSA_ENVELOPE_TAIL_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4_096)
    }
}

/// Browser-sidecar configuration assembled from the environment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserConfig {
    pub enabled: bool,
    pub path: Option<std::path::PathBuf>,
    pub timeout_ms: u64,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            enabled: env::browser_enabled(),
            path: env::browser_path(),
            timeout_ms: env::browser_timeout_ms(),
        }
    }
}

/// Output-envelope configuration assembled from the environment.
///
/// Note: this struct intentionally shadows nothing — `medusa-agent`
/// defines its own `EnvelopeConfig` with additional fields (artifact cap,
/// session root) used at the engine call site. This struct is the
/// *configuration* shape derived from env vars.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnvelopeSettings {
    pub head_bytes: usize,
    pub tail_bytes: usize,
}

impl Default for EnvelopeSettings {
    fn default() -> Self {
        Self {
            head_bytes: env::envelope_head_bytes(),
            tail_bytes: env::envelope_tail_bytes(),
        }
    }
}

/// Top-level runtime configuration assembled from environment variables.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MedusaConfig {
    pub browser: BrowserConfig,
    pub envelope: EnvelopeSettings,
    pub daemon_max_artifact_bytes: usize,
}

impl MedusaConfig {
    /// Read every supported environment variable and assemble the
    /// runtime config. Returns `Ok` even when variables are missing —
    /// each sub-config falls back to a documented default.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            browser: BrowserConfig::default(),
            envelope: EnvelopeSettings::default(),
            daemon_max_artifact_bytes: std::env::var("MEDUSA_DAEMON_MAX_ARTIFACT_BYTES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(256 * 1024 * 1024),
        }
    }
}
