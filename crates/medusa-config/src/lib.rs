//! Typed configuration with deterministic precedence.

use std::{collections::BTreeMap, fs, path::Path};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};

mod config_doctor;
mod configuration_state;
mod provider_profile;
mod provider_profiles;
mod staged_profile;

pub use config_doctor::{
    ConfigDoctorCheck, ConfigDoctorRepair, ConfigDoctorReport, ConfigDoctorStatus,
    diagnose_config_catalog, repair_config_check,
};
pub use configuration_state::{
    ConfigurationApplyTiming, ConfigurationChangeOrigin, ConfigurationChanged,
};
pub use provider_profile::{
    PROVIDER_PROFILE_KEYS, ProviderProfile, ProviderProfileStore, ProviderProfileValue,
    credential_environment,
};
pub use provider_profiles::{
    ProviderProfileCatalog, ProviderProfileSnapshot, ProviderProfileSummary, ProviderProfileUpdate,
};
pub use staged_profile::{
    ProviderProfileDiffEntry, ProviderProfileHistoryEntry, ProviderProfileSection,
    StagedProviderProfile,
};

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

/// Root configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub version: u16,
    pub agent: AgentConfig,
    pub model: ModelConfig,
    pub memory: MemoryConfig,
    pub verification: VerificationConfig,
}

/// Agent settings with production runtime effects.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    pub mode: Mode,
    pub max_turns: u32,
    pub parallel_workers: u16,
}

/// Model settings with production provider effects.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelConfig {
    pub provider: String,
    pub fallback_providers: Vec<FallbackProviderConfig>,
    /// Optional role/phase to existing route-profile bindings. Empty preserves the single-route
    /// behavior; values are route ids such as `primary` or `fallback[0]`.
    pub role_routes: BTreeMap<String, String>,
    pub name: String,
    pub protocol: String,
    pub temperature_milli: u16,
    pub max_output_tokens: u32,
    pub context_window_tokens: u64,
    pub auto_compact_percent: u8,
    pub base_url: Option<String>,
    pub auth: String,
    pub tool_calling: bool,
    #[serde(default = "default_true")]
    pub streaming: bool,
    pub max_retries: u8,
    pub retry_base_delay_ms: u64,
    pub retry_max_delay_ms: u64,
    pub retry_jitter_ms: u64,
}

/// A complete, independently resolved fallback route.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FallbackProviderConfig {
    pub provider: String,
    pub name: String,
    pub protocol: String,
    pub base_url: Option<String>,
    pub auth: String,
    #[serde(default = "default_true")]
    pub tool_calling: bool,
    #[serde(default = "default_true")]
    pub streaming: bool,
    #[serde(default = "default_max_retries")]
    pub max_retries: u8,
    #[serde(default = "default_retry_base_delay_ms")]
    pub retry_base_delay_ms: u64,
    #[serde(default = "default_retry_max_delay_ms")]
    pub retry_max_delay_ms: u64,
    #[serde(default = "default_retry_jitter_ms")]
    pub retry_jitter_ms: u64,
}

fn default_true() -> bool {
    true
}

fn default_max_retries() -> u8 {
    1
}

fn default_retry_base_delay_ms() -> u64 {
    250
}

fn default_retry_max_delay_ms() -> u64 {
    8_000
}

fn default_retry_jitter_ms() -> u64 {
    100
}

/// Memory settings with production persistence effects.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryConfig {
    pub enabled: bool,
    pub format: String,
}

/// Verification settings with production execution effects.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct VerificationConfig {
    pub required: bool,
    /// Automatically run browser verification for effective UI changes.
    pub browser_on_ui_change: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            agent: AgentConfig::default(),
            model: ModelConfig::default(),
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
        }
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: "minimax".into(),
            fallback_providers: Vec::new(),
            role_routes: BTreeMap::new(),
            name: "MiniMax-M3".into(),
            protocol: "openai".into(),
            temperature_milli: 200,
            max_output_tokens: 32_768,
            context_window_tokens: 1_000_000,
            auto_compact_percent: 40,
            base_url: None,
            auth: "api-key".into(),
            tool_calling: true,
            // OpenAI-compatible routes advertise streaming and should expose the
            // first response tokens immediately instead of buffering the whole turn.
            // Providers that do not support streaming still fail closed through
            // their capability contract.
            streaming: true,
            max_retries: default_max_retries(),
            retry_base_delay_ms: default_retry_base_delay_ms(),
            retry_max_delay_ms: default_retry_max_delay_ms(),
            retry_jitter_ms: default_retry_jitter_ms(),
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            format: "markdown".into(),
        }
    }
}

impl Default for VerificationConfig {
    fn default() -> Self {
        Self {
            required: true,
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
        let profile = ProviderProfileCatalog::user()?.active_store()?.load()?;
        Self::load_layers_with_provider_profile(&profile, user, project, environment, cli)
    }

    /// Resolves and validates all layers against an explicit provider-profile candidate.
    ///
    /// Frontends use this before persisting a profile mutation or selection so an invalid
    /// effective configuration never replaces the prior valid state.
    pub fn load_layers_with_provider_profile(
        profile: &ProviderProfile,
        user: Option<&Path>,
        project: Option<&Path>,
        environment: &BTreeMap<String, String>,
        cli: &BTreeMap<String, String>,
    ) -> MedusaResult<Self> {
        profile.validate()?;
        let mut value =
            toml::Value::try_from(Self::default()).map_err(|error| invalid(error.to_string()))?;
        merge_provider_profile(&mut value, profile)?;
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
        validate_route(
            "primary",
            &self.model.provider,
            &self.model.name,
            &self.model.protocol,
            &self.model.auth,
            self.model.max_retries,
            self.model.retry_base_delay_ms,
            self.model.retry_max_delay_ms,
            self.model.retry_jitter_ms,
        )?;
        for (index, fallback) in self.model.fallback_providers.iter().enumerate() {
            validate_route(
                &format!("fallback[{index}]"),
                &fallback.provider,
                &fallback.name,
                &fallback.protocol,
                &fallback.auth,
                fallback.max_retries,
                fallback.retry_base_delay_ms,
                fallback.retry_max_delay_ms,
                fallback.retry_jitter_ms,
            )?;
        }
        for (role, route) in &self.model.role_routes {
            if !matches!(
                role.as_str(),
                "default"
                    | "planning"
                    | "planner"
                    | "research"
                    | "implementation"
                    | "implementer"
                    | "high_risk_review"
                    | "reviewer"
                    | "repair"
                    | "debugger"
                    | "verifier"
                    | "summarization"
                    | "summarizer"
                    | "formatting"
                    | "formatter"
            ) {
                return Err(invalid(format!(
                    "unsupported model role route key `{role}`"
                )));
            }
            if route != "primary" {
                let Some(index) = route
                    .strip_prefix("fallback[")
                    .and_then(|value| value.strip_suffix(']'))
                    .and_then(|value| value.parse::<usize>().ok())
                else {
                    return Err(invalid(format!(
                        "model role route `{role}` must reference `primary` or `fallback[index]`"
                    )));
                };
                if index >= self.model.fallback_providers.len() {
                    return Err(invalid(format!(
                        "model role route `{role}` references missing fallback[{index}]"
                    )));
                }
            }
        }
        if !(1..=100).contains(&self.model.auto_compact_percent) {
            return Err(invalid("auto_compact_percent must be between 1 and 100"));
        }
        if self.memory.format != "markdown" {
            return Err(invalid("memory format must remain markdown"));
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_route(
    label: &str,
    provider: &str,
    model: &str,
    protocol: &str,
    auth: &str,
    max_retries: u8,
    base_delay_ms: u64,
    max_delay_ms: u64,
    jitter_ms: u64,
) -> MedusaResult<()> {
    if provider.trim().is_empty() || model.trim().is_empty() {
        return Err(invalid(format!(
            "{label} provider and model must be explicit"
        )));
    }
    if !matches!(
        protocol.trim().to_ascii_lowercase().as_str(),
        "anthropic" | "openai"
    ) {
        return Err(invalid(format!(
            "{label} protocol must be anthropic or openai"
        )));
    }
    if !matches!(
        auth.trim().to_ascii_lowercase().as_str(),
        "api-key" | "none"
    ) {
        return Err(invalid(format!("{label} auth must be api-key or none")));
    }
    if max_retries > 8 {
        return Err(invalid(format!("{label} max_retries must be at most 8")));
    }
    if base_delay_ms == 0 || max_delay_ms < base_delay_ms || jitter_ms > max_delay_ms {
        return Err(invalid(format!(
            "{label} retry policy is invalid or unbounded"
        )));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

fn merge_provider_profile(base: &mut toml::Value, profile: &ProviderProfile) -> MedusaResult<()> {
    if !profile.configured {
        return Ok(());
    }
    let protocol = profile.protocol().to_owned();
    let ProviderProfile {
        provider,
        model: model_name,
        auth,
        base_url,
        ..
    } = profile.clone();
    let mut model = toml::map::Map::new();
    model.insert("provider".to_owned(), toml::Value::String(provider));
    model.insert("name".to_owned(), toml::Value::String(model_name));
    model.insert("protocol".to_owned(), toml::Value::String(protocol));
    model.insert("auth".to_owned(), toml::Value::String(auth));
    let mut root = toml::map::Map::new();
    root.insert("model".to_owned(), toml::Value::Table(model));
    merge(base, toml::Value::Table(root));
    if let Some(url) = base_url {
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
    fn defaults_validate_and_are_documented_contract() {
        let config = Config::default();
        config.validate().expect("defaults");
        assert_eq!(config.agent.mode, Mode::Yolo);
        assert_eq!(config.agent.max_turns, 500);
        assert_eq!(config.agent.parallel_workers, 4);
        assert_eq!(config.model.provider, "minimax");
        assert!(config.model.role_routes.is_empty());
        assert_eq!(config.model.name, "MiniMax-M3");
        assert!(config.model.streaming);
        assert_eq!(config.model.temperature_milli, 200);
        assert_eq!(config.model.max_output_tokens, 32_768);
        assert_eq!(config.model.context_window_tokens, 1_000_000);
        assert_eq!(config.model.auto_compact_percent, 40);
        assert!(config.memory.enabled);
        assert_eq!(config.memory.format, "markdown");
        assert!(config.verification.required);
    }

    #[test]
    fn omitted_streaming_settings_keep_openai_routes_streaming() {
        let config = Config::from_toml(
            "version = 1\n[model]\nprovider = 'minimax'\nname = 'MiniMax-M3'\nprotocol = 'openai'\nauth = 'api-key'\n",
        )
        .expect("partial provider configuration");
        assert!(config.model.streaming);

        let fallback = Config::from_toml(
            "version = 1\n[model]\nfallback_providers = [{ provider = 'openai', name = 'gpt-test', protocol = 'openai', auth = 'api-key' }]\n",
        )
        .expect("fallback provider configuration");
        assert!(fallback.model.fallback_providers[0].streaming);
    }

    #[test]
    fn unknown_fields_fail_closed() {
        assert!(Config::from_toml("version = 1\nunknown = true").is_err());
    }

    #[test]
    fn role_routes_bind_roles_to_existing_fallback_profiles() {
        let config = Config::from_toml(
            "version = 1\n[model]\nrole_routes = { planner = 'primary', implementer = 'fallback[0]' }\n[[model.fallback_providers]]\nprovider = 'openai'\nname = 'gpt-test'\nprotocol = 'openai'\nauth = 'api-key'\n",
        )
        .expect("role route config");
        assert_eq!(config.model.role_routes["planner"], "primary");
        assert_eq!(config.model.role_routes["implementer"], "fallback[0]");
    }

    #[test]
    fn role_routes_reject_unknown_or_missing_profiles() {
        for document in [
            "version = 1\n[model]\nrole_routes = { auditor = 'primary' }\n",
            "version = 1\n[model]\nrole_routes = { planner = 'fallback[0]' }\n",
            "version = 1\n[model]\nrole_routes = { planner = 'other' }\n",
        ] {
            assert!(Config::from_toml(document).is_err(), "accepted {document}");
        }
    }

    #[test]
    fn removed_no_effect_fields_fail_closed() {
        for document in [
            "version = 1\n[agent]\nask_policy = 'only_irreducible'\n",
            "version = 1\n[model]\nspeed = 'balanced'\n",
            "version = 1\n[model]\nreasoning = 'medium'\n",
            "version = 1\n[runtime]\nbackend = 'auto'\n",
            "version = 1\n[runtime]\nnetwork = 'allowlist'\n",
            "version = 1\n[runtime]\nprocess_limit = 512\n",
            "version = 1\n[git]\nauto_commit = true\n",
            "version = 1\n[git]\nprotect_dirty_tree = true\n",
            "version = 1\n[git]\nallow_force_push = false\n",
            "version = 1\n[memory]\nauto_promote_low_risk = true\n",
            "version = 1\n[verification]\nindependent_review = true\n",
        ] {
            assert!(Config::from_toml(document).is_err(), "accepted {document}");
        }
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
