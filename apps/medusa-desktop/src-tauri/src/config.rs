use std::collections::BTreeMap;

use medusa_config::{
    Config, ProviderProfile, ProviderProfileCatalog, ProviderProfileStore, credential_environment,
};
use serde::Serialize;

use crate::credentials::{CredentialStore, SystemCredentialStore};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSharedConfiguration {
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

/// Validated active-profile candidate; persistence is deferred until runtime acceptance.
pub(crate) struct PreparedProviderProfile {
    store: ProviderProfileStore,
    profile: ProviderProfile,
}

impl PreparedProviderProfile {
    pub(crate) fn commit(self) -> Result<(), String> {
        self.store
            .save(&self.profile)
            .map_err(|error| error.to_string())
    }
}

#[tauri::command]
pub fn desktop_shared_configuration() -> Result<DesktopSharedConfiguration, String> {
    let catalog = ProviderProfileCatalog::user().map_err(|error| error.to_string())?;
    let credentials = SystemCredentialStore;
    shared_configuration(&catalog, |provider| credentials.load(provider))
}

pub(crate) fn prepare_provider_profile(
    provider: &str,
    model: &str,
    effort: &str,
) -> Result<PreparedProviderProfile, String> {
    let catalog = ProviderProfileCatalog::user().map_err(|error| error.to_string())?;
    prepare_with_catalog(&catalog, provider, model, effort)
}

fn prepare_with_catalog(
    catalog: &ProviderProfileCatalog,
    provider: &str,
    model: &str,
    effort: &str,
) -> Result<PreparedProviderProfile, String> {
    let store = catalog.active_store().map_err(|error| error.to_string())?;
    let mut profile = store.load().map_err(|error| error.to_string())?;
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
    Ok(PreparedProviderProfile { store, profile })
}

fn shared_configuration(
    catalog: &ProviderProfileCatalog,
    load_credential: impl FnOnce(&str) -> Result<Option<String>, String>,
) -> Result<DesktopSharedConfiguration, String> {
    let active_profile = catalog.active_name().map_err(|error| error.to_string())?;
    let profile = catalog
        .active_store()
        .map_err(|error| error.to_string())?
        .load()
        .map_err(|error| error.to_string())?;
    let environment_credential = credential_environment(&profile.provider)
        .and_then(std::env::var_os)
        .is_some_and(|value| !value.is_empty());
    let credential_configured = profile.auth == "none"
        || environment_credential
        || load_credential(&profile.provider)?.is_some();
    Ok(DesktopSharedConfiguration {
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
        prepare_with_catalog(&catalog, "openai", "gpt-5", "high")
            .expect("prepare")
            .commit()
            .expect("commit");

        let configuration = shared_configuration(&catalog, |_| Ok(Some("secret".to_owned())))
            .expect("shared configuration");
        assert_eq!(configuration.active_profile, "default");
        assert_eq!(configuration.connection, "openai-api");
        assert_eq!(configuration.provider, "openai");
        assert_eq!(configuration.model, "gpt-5");
        assert_eq!(configuration.effort, "high");
        assert!(configuration.configured);
        assert!(configuration.credential_configured);
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
