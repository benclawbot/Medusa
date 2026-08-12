use std::collections::BTreeMap;

use medusa_config::{
    Config, ConfigurationApplyTiming, ConfigurationChangeOrigin, ConfigurationChanged,
    ProviderProfile, ProviderProfileCatalog, ProviderProfileUpdate, credential_environment,
};
use serde::Serialize;

use crate::credentials::{CredentialStore, SystemCredentialStore};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSharedConfiguration {
    pub revision: u64,
    pub active_profile: String,
    pub connection: String,
    pub provider: String,
    pub model: String,
    pub effort: String,
    pub auth: String,
    pub base_url: Option<String>,
    pub configured: bool,
    pub credential_configured: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopConfigurationChanged {
    pub revision: u64,
    pub active_profile: String,
    pub changed_keys: Vec<String>,
    pub origin: String,
    pub apply_timing: String,
}

impl From<ConfigurationChanged> for DesktopConfigurationChanged {
    fn from(change: ConfigurationChanged) -> Self {
        Self {
            revision: change.revision,
            active_profile: change.active_profile,
            changed_keys: change.changed_keys,
            origin: change.origin.label().to_owned(),
            apply_timing: change.apply_timing.label().to_owned(),
        }
    }
}

/// Validated active-profile candidate; persistence is deferred until runtime acceptance.
pub(crate) struct PreparedProviderProfile {
    update: ProviderProfileUpdate,
    profile: ProviderProfile,
}

impl PreparedProviderProfile {
    pub(crate) fn previous_profile(&self) -> &ProviderProfile {
        self.update.profile()
    }

    pub(crate) fn is_changed(&self) -> bool {
        self.update.profile() != &self.profile
    }

    pub(crate) fn commit(self) -> Result<ConfigurationChanged, String> {
        self.update
            .commit(
                &self.profile,
                ConfigurationChangeOrigin::Desktop,
                [
                    "connection".to_owned(),
                    "provider".to_owned(),
                    "model".to_owned(),
                    "reasoning".to_owned(),
                    "configured".to_owned(),
                ],
                ConfigurationApplyTiming::Immediate,
            )
            .map_err(|error| error.to_string())
    }
}

#[tauri::command]
pub fn desktop_shared_configuration() -> Result<DesktopSharedConfiguration, String> {
    let catalog = ProviderProfileCatalog::user().map_err(|error| error.to_string())?;
    let credentials = SystemCredentialStore;
    shared_configuration(&catalog, |provider| credentials.load(provider))
}

pub(crate) fn active_config() -> Result<Config, String> {
    let catalog = ProviderProfileCatalog::user().map_err(|error| error.to_string())?;
    let profile = catalog
        .snapshot()
        .map_err(|error| error.to_string())?
        .profile;
    Config::load_layers_with_provider_profile(
        &profile,
        None,
        None,
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn prepare_provider_profile(
    provider: &str,
    model: &str,
    effort: &str,
    expected_revision: u64,
) -> Result<PreparedProviderProfile, String> {
    let catalog = ProviderProfileCatalog::user().map_err(|error| error.to_string())?;
    prepare_with_catalog(catalog, provider, model, effort, expected_revision)
}

fn prepare_with_catalog(
    catalog: ProviderProfileCatalog,
    provider: &str,
    model: &str,
    effort: &str,
    expected_revision: u64,
) -> Result<PreparedProviderProfile, String> {
    let update = catalog
        .begin_active_profile_update(expected_revision)
        .map_err(|error| error.to_string())?;
    let mut profile = update.profile().clone();
    profile
        .set_value("connection", connection_for_provider(provider))
        .map_err(|error| error.to_string())?;
    profile
        .set_value("provider", provider)
        .map_err(|error| error.to_string())?;
    profile
        .set_value("model", model)
        .map_err(|error| error.to_string())?;
    profile
        .set_value("reasoning", reasoning_for_effort(effort))
        .map_err(|error| error.to_string())?;
    profile
        .set_value("configured", "true")
        .map_err(|error| error.to_string())?;
    Config::load_layers_with_provider_profile(
        &profile,
        None,
        None,
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .map_err(|error| error.to_string())?;
    Ok(PreparedProviderProfile { update, profile })
}

fn shared_configuration(
    catalog: &ProviderProfileCatalog,
    load_credential: impl FnOnce(&str) -> Result<Option<String>, String>,
) -> Result<DesktopSharedConfiguration, String> {
    let snapshot = catalog.snapshot().map_err(|error| error.to_string())?;
    let revision = snapshot.revision;
    let active_profile = snapshot.active_profile;
    let profile = snapshot.profile;
    let environment_credential = credential_environment(&profile.provider)
        .and_then(std::env::var_os)
        .is_some_and(|value| !value.is_empty());
    let credential_configured = profile.auth == "none"
        || environment_credential
        || load_credential(&profile.provider)?.is_some();
    Ok(DesktopSharedConfiguration {
        revision,
        active_profile,
        connection: profile.connection,
        provider: profile.provider,
        model: profile.model,
        effort: effort_for_reasoning(&profile.reasoning).to_owned(),
        auth: profile.auth,
        base_url: profile.base_url,
        configured: profile.configured,
        credential_configured,
    })
}

fn connection_for_provider(provider: &str) -> &'static str {
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai-oauth" => "chatgpt-oauth",
        "openai" => "openai-api",
        "openai-compatible" => "openai-compatible",
        "omniroute" => "omniroute",
        "local" => "local",
        _ => "direct",
    }
}

fn reasoning_for_effort(effort: &str) -> &'static str {
    match effort.trim().to_ascii_lowercase().as_str() {
        "low" => "low",
        "high" => "high",
        "auto" | "medium" => "medium",
        _ => "medium",
    }
}

fn effort_for_reasoning(reasoning: &str) -> &'static str {
    match reasoning {
        "low" => "low",
        "high" | "maximum" => "high",
        _ => "medium",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_reads_and_writes_the_shared_active_profile() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path());
        prepare_with_catalog(catalog.clone(), "openai", "gpt-5", "high", 0)
            .expect("prepare")
            .commit()
            .expect("commit");

        let configuration = shared_configuration(&catalog, |_| Ok(Some("secret".to_owned())))
            .expect("shared configuration");
        assert_eq!(configuration.revision, 1);
        assert_eq!(configuration.active_profile, "default");
        assert_eq!(configuration.connection, "openai-api");
        assert_eq!(configuration.provider, "openai");
        assert_eq!(configuration.model, "gpt-5");
        assert_eq!(configuration.effort, "high");
        assert!(configuration.configured);
        assert!(configuration.credential_configured);
    }

    #[test]
    fn desktop_rejects_a_stale_revision_before_persistence() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path());
        prepare_with_catalog(catalog.clone(), "openai", "gpt-5", "high", 0)
            .expect("first prepare")
            .commit()
            .expect("first commit");

        let error = prepare_with_catalog(catalog, "anthropic", "claude", "medium", 0)
            .err()
            .expect("stale revision");
        assert!(error.contains("current revision is 1"));
    }

    #[test]
    fn desktop_configuration_response_never_contains_credentials() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path());
        let configuration = shared_configuration(&catalog, |_| Ok(Some("top-secret".to_owned())))
            .expect("shared configuration");
        let encoded = serde_json::to_string(&configuration).expect("serialize");
        assert!(!encoded.contains("top-secret"));
        assert!(!encoded.to_ascii_lowercase().contains("api_key"));
        assert!(!encoded.to_ascii_lowercase().contains("token"));
    }
}
