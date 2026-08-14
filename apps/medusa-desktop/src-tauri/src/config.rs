use std::collections::BTreeMap;

use medusa_config::{
    Config, ConfigurationApplyTiming, ConfigurationChangeOrigin, ConfigurationChanged,
    ProviderCatalogEntry, ProviderProfile, ProviderProfileCatalog, ProviderProfileUpdate,
    apply_provider_defaults, credential_environment, provider_catalog, provider_catalog_entry,
    provider_catalog_entry_for_profile, provider_model_options,
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
pub struct DesktopProviderCatalogEntry {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub connection: String,
    pub profile_provider: String,
    pub auth_methods: Vec<String>,
    pub default_auth: String,
    pub default_model: String,
    pub model_options: Vec<String>,
    pub base_url: Option<String>,
    pub browser_oauth: bool,
    pub discover_models: bool,
    pub custom_values: bool,
    pub disabled_reason: Option<String>,
    pub current_custom: bool,
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
                    "auth".to_owned(),
                    "base_url".to_owned(),
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

#[tauri::command]
pub fn desktop_provider_catalog() -> Result<Vec<DesktopProviderCatalogEntry>, String> {
    let catalog = ProviderProfileCatalog::user().map_err(|error| error.to_string())?;
    provider_catalog_for(&catalog)
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
    let previous = update.profile();
    let mut profile = previous.clone();

    if let Some(entry) = provider_catalog_entry(provider) {
        if let Some(reason) = entry.disabled_reason {
            return Err(format!("provider {} is unavailable: {reason}", entry.display_name));
        }
        let same_route = provider_catalog_entry_for_profile(previous)
            .is_some_and(|current| current.id == entry.id);
        if !same_route {
            apply_provider_defaults(entry, &mut profile);
        } else {
            profile.connection = entry.connection.to_owned();
            profile.provider = entry.profile_provider.to_owned();
            if !entry.auth_methods.contains(&profile.auth.as_str()) {
                profile.auth = entry.default_auth.to_owned();
            }
            if !entry.custom_values {
                profile.base_url = entry.base_url.map(str::to_owned);
            }
        }
        if !model.trim().is_empty() {
            profile.model = model.trim().to_owned();
        }
    } else {
        let current_provider = previous.provider.trim();
        if provider.trim() != current_provider {
            return Err(format!(
                "unknown provider `{provider}`; only an already configured custom provider may be preserved"
            ));
        }
        if !model.trim().is_empty() {
            profile.model = model.trim().to_owned();
        }
    }

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

fn provider_catalog_for(
    catalog: &ProviderProfileCatalog,
) -> Result<Vec<DesktopProviderCatalogEntry>, String> {
    let snapshot = catalog.snapshot().map_err(|error| error.to_string())?;
    let current = snapshot.profile;
    let current_entry = provider_catalog_entry_for_profile(&current);
    let mut entries = provider_catalog()
        .iter()
        .map(|entry| desktop_catalog_entry(entry, &current, current_entry))
        .collect::<Vec<_>>();

    if current_entry.is_none() && !current.provider.trim().is_empty() {
        entries.insert(
            0,
            DesktopProviderCatalogEntry {
                id: current.provider.clone(),
                display_name: current.provider.clone(),
                description: "Current custom provider profile".to_owned(),
                connection: current.connection.clone(),
                profile_provider: current.provider.clone(),
                auth_methods: vec![current.auth.clone()],
                default_auth: current.auth.clone(),
                default_model: current.model.clone(),
                model_options: vec![current.model.clone()],
                base_url: current.base_url.clone(),
                browser_oauth: false,
                discover_models: false,
                custom_values: true,
                disabled_reason: None,
                current_custom: true,
            },
        );
    }
    Ok(entries)
}

fn desktop_catalog_entry(
    entry: &ProviderCatalogEntry,
    current: &ProviderProfile,
    current_entry: Option<&ProviderCatalogEntry>,
) -> DesktopProviderCatalogEntry {
    let current_model = current_entry
        .filter(|candidate| candidate.id == entry.id)
        .map_or("", |_| current.model.as_str());
    DesktopProviderCatalogEntry {
        id: entry.id.to_owned(),
        display_name: entry.display_name.to_owned(),
        description: entry.description.to_owned(),
        connection: entry.connection.to_owned(),
        profile_provider: entry.profile_provider.to_owned(),
        auth_methods: entry.auth_methods.iter().map(|value| (*value).to_owned()).collect(),
        default_auth: entry.default_auth.to_owned(),
        default_model: entry.default_model.to_owned(),
        model_options: provider_model_options(entry.id, current_model, &[]),
        base_url: entry.base_url.map(str::to_owned),
        browser_oauth: entry.browser_oauth,
        discover_models: entry.discover_models,
        custom_values: entry.custom_values,
        disabled_reason: entry.disabled_reason.map(str::to_owned),
        current_custom: false,
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
    fn desktop_uses_catalog_mapping_for_omniroute() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path());
        prepare_with_catalog(catalog.clone(), "auto/coding", "auto/coding", "medium", 0)
            .expect("prepare")
            .commit()
            .expect("commit");

        let configuration = shared_configuration(&catalog, |_| Ok(None)).expect("configuration");
        assert_eq!(configuration.connection, "omniroute");
        assert_eq!(configuration.provider, "auto/coding");
        assert_eq!(configuration.auth, "none");
    }

    #[test]
    fn desktop_catalog_serialization_matches_canonical_provider_metadata() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path());
        let entries = provider_catalog_for(&catalog).expect("catalog");
        assert_eq!(entries.len(), provider_catalog().len());
        for canonical in provider_catalog() {
            let desktop = entries
                .iter()
                .find(|entry| entry.id == canonical.id)
                .expect(canonical.id);
            assert_eq!(desktop.connection, canonical.connection);
            assert_eq!(desktop.profile_provider, canonical.profile_provider);
            assert_eq!(desktop.default_auth, canonical.default_auth);
            assert_eq!(desktop.default_model, canonical.default_model);
            assert_eq!(desktop.base_url.as_deref(), canonical.base_url);
            assert_eq!(desktop.disabled_reason.as_deref(), canonical.disabled_reason);
        }
    }

    #[test]
    fn desktop_preserves_an_existing_custom_provider() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path());
        let update = catalog.begin_active_profile_update(0).expect("update");
        let mut profile = update.profile().clone();
        profile.connection = "direct".to_owned();
        profile.provider = "private-gateway".to_owned();
        profile.model = "private-model".to_owned();
        profile.auth = "none".to_owned();
        profile.configured = true;
        update
            .commit(
                &profile,
                ConfigurationChangeOrigin::System,
                ["provider".to_owned()],
                ConfigurationApplyTiming::Immediate,
            )
            .expect("commit");

        let entries = provider_catalog_for(&catalog).expect("catalog");
        let custom = entries.first().expect("custom provider");
        assert!(custom.current_custom);
        assert_eq!(custom.profile_provider, "private-gateway");
        assert_eq!(custom.model_options, vec!["private-model"]);
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

    #[test]
    fn selecting_a_catalog_provider_applies_canonical_defaults() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path());
        let prepared = prepare_with_catalog(catalog, "openai-oauth", "", "medium", 0)
            .expect("prepare");
        assert_eq!(prepared.profile.connection, "chatgpt-oauth");
        assert_eq!(prepared.profile.provider, "openai-oauth");
        assert_eq!(prepared.profile.model, "gpt-5");
        assert_eq!(prepared.profile.auth, "none");
        assert_eq!(
            prepared.profile.base_url.as_deref(),
            Some("http://127.0.0.1:10531/v1")
        );
    }
}
