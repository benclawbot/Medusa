use std::collections::BTreeSet;

use crate::ProviderProfile;

/// Canonical provider/route metadata shared by setup and in-session model selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderCatalogEntry {
    /// Stable UI/runtime provider identifier.
    pub id: &'static str,
    /// Human-readable provider/route name.
    pub display_name: &'static str,
    /// Short non-secret capability/availability description.
    pub description: &'static str,
    /// `ProviderProfile.connection` value used by this route.
    pub connection: &'static str,
    /// `ProviderProfile.provider` value used by this route.
    pub profile_provider: &'static str,
    /// Authentication modes the route can truthfully represent.
    pub auth_methods: &'static [&'static str],
    /// Default authentication mode for a newly selected route.
    pub default_auth: &'static str,
    /// Recommended static fallback model.
    pub default_model: &'static str,
    /// Known non-billable fallback choices when live discovery is unavailable.
    pub known_models: &'static [&'static str],
    /// Canonical endpoint when the route owns one.
    pub base_url: Option<&'static str>,
    /// Whether a first-class browser OAuth action is available.
    pub browser_oauth: bool,
    /// Whether model discovery can be performed without a completion request.
    pub discover_models: bool,
    /// Whether the route allows a user-supplied endpoint/model escape hatch.
    pub custom_values: bool,
    /// Static unavailable reason, if this build intentionally disables the route.
    pub disabled_reason: Option<&'static str>,
}

pub const PROVIDER_CATALOG_IDS: [&str; 8] = [
    "minimax",
    "anthropic",
    "anthropic-compatible",
    "openai",
    "openai-oauth",
    "openai-compatible",
    "omniroute",
    "local",
];

const CATALOG: [ProviderCatalogEntry; 8] = [
    ProviderCatalogEntry {
        id: "minimax",
        display_name: "MiniMax direct",
        description: "Direct MiniMax route using MINIMAX_API_KEY",
        connection: "direct",
        profile_provider: "minimax",
        auth_methods: &["api-key"],
        default_auth: "api-key",
        default_model: "MiniMax-M3",
        known_models: &[
            "MiniMax-M3",
            "MiniMax-M2.7",
            "MiniMax-M2.7-highspeed",
            "MiniMax-M2.5",
        ],
        base_url: None,
        browser_oauth: false,
        discover_models: false,
        custom_values: false,
        disabled_reason: None,
    },
    ProviderCatalogEntry {
        id: "anthropic",
        display_name: "Anthropic",
        description: "Direct Anthropic route using ANTHROPIC_API_KEY",
        connection: "direct",
        profile_provider: "anthropic",
        auth_methods: &["api-key"],
        default_auth: "api-key",
        default_model: "claude-sonnet-4-6",
        known_models: &["claude-opus-4-6", "claude-sonnet-4-6", "claude-haiku-4-5"],
        base_url: None,
        browser_oauth: false,
        discover_models: false,
        custom_values: false,
        disabled_reason: None,
    },
    ProviderCatalogEntry {
        id: "anthropic-compatible",
        display_name: "Anthropic-compatible",
        description: "Custom Anthropic-compatible route using brokered MEDUSA_API_KEY",
        connection: "direct",
        profile_provider: "anthropic-compatible",
        auth_methods: &["api-key", "existing", "none"],
        default_auth: "api-key",
        default_model: "custom-model",
        known_models: &["custom-model"],
        base_url: None,
        browser_oauth: false,
        discover_models: false,
        custom_values: true,
        disabled_reason: None,
    },
    ProviderCatalogEntry {
        id: "openai",
        display_name: "OpenAI API",
        description: "Official OpenAI API route using OPENAI_API_KEY",
        connection: "openai-api",
        profile_provider: "openai",
        auth_methods: &["api-key"],
        default_auth: "api-key",
        default_model: "gpt-5.1-codex",
        known_models: &["gpt-5.1-codex", "gpt-5.1", "gpt-5-mini"],
        base_url: Some("https://api.openai.com/v1"),
        browser_oauth: false,
        discover_models: false,
        custom_values: false,
        disabled_reason: None,
    },
    ProviderCatalogEntry {
        id: "openai-oauth",
        display_name: "ChatGPT OAuth",
        description: "ChatGPT/Codex OAuth through the local openai-oauth gateway",
        connection: "chatgpt-oauth",
        profile_provider: "openai-oauth",
        auth_methods: &["none"],
        default_auth: "none",
        default_model: "gpt-5",
        known_models: &["gpt-5"],
        base_url: Some("http://127.0.0.1:10531/v1"),
        browser_oauth: true,
        discover_models: true,
        custom_values: false,
        disabled_reason: None,
    },
    ProviderCatalogEntry {
        id: "openai-compatible",
        display_name: "OpenAI-compatible",
        description: "Custom OpenAI-compatible endpoint",
        connection: "openai-compatible",
        profile_provider: "openai-compatible",
        auth_methods: &["api-key", "existing", "none"],
        default_auth: "existing",
        default_model: "custom-model",
        known_models: &["custom-model"],
        base_url: Some("http://127.0.0.1:8000/v1"),
        browser_oauth: false,
        discover_models: false,
        custom_values: true,
        disabled_reason: None,
    },
    ProviderCatalogEntry {
        id: "omniroute",
        display_name: "OmniRoute",
        description: "Managed or existing OmniRoute gateway (recommended)",
        connection: "omniroute",
        profile_provider: "auto/coding",
        auth_methods: &["existing", "oauth", "none"],
        default_auth: "existing",
        default_model: "auto/coding",
        known_models: &["auto/coding"],
        base_url: Some("http://127.0.0.1:20128/v1"),
        browser_oauth: false,
        discover_models: false,
        custom_values: false,
        disabled_reason: None,
    },
    ProviderCatalogEntry {
        id: "local",
        display_name: "Local runtime",
        description: "OpenAI-compatible local runtime on 127.0.0.1:11434",
        connection: "local",
        profile_provider: "local",
        auth_methods: &["none", "existing"],
        default_auth: "none",
        default_model: "MiniMax-M3",
        known_models: &["MiniMax-M3", "local-model"],
        base_url: Some("http://127.0.0.1:11434/v1"),
        browser_oauth: false,
        discover_models: false,
        custom_values: true,
        disabled_reason: None,
    },
];

#[must_use]
pub const fn provider_catalog() -> &'static [ProviderCatalogEntry] {
    &CATALOG
}

#[must_use]
pub fn provider_catalog_entry(id: &str) -> Option<&'static ProviderCatalogEntry> {
    CATALOG
        .iter()
        .find(|entry| entry.id == id || entry.profile_provider == id)
}

#[must_use]
pub fn provider_catalog_entry_for_profile(
    profile: &ProviderProfile,
) -> Option<&'static ProviderCatalogEntry> {
    CATALOG.iter().find(|entry| {
        entry.connection == profile.connection
            && (entry.profile_provider == profile.provider || entry.id == profile.provider)
    })
}

/// Returns catalog provider ids while preserving an already configured custom provider.
#[must_use]
pub fn provider_ids_with_current(current_provider: &str) -> Vec<String> {
    let mut providers = PROVIDER_CATALOG_IDS
        .iter()
        .map(|provider| (*provider).to_owned())
        .collect::<Vec<_>>();
    if !current_provider.trim().is_empty()
        && !providers.iter().any(|provider| provider == current_provider)
    {
        providers.insert(0, current_provider.to_owned());
    }
    providers
}

/// Merges discovered, known, and current model values without silently dropping custom state.
#[must_use]
pub fn provider_model_options(
    provider: &str,
    current_model: &str,
    discovered: &[String],
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut models = Vec::new();
    let mut push = |model: &str| {
        if !model.trim().is_empty() && seen.insert(model.to_owned()) {
            models.push(model.to_owned());
        }
    };

    // A configured current value is always retained, even if live discovery is unavailable.
    push(current_model);
    for model in discovered {
        push(model);
    }
    if let Some(entry) = provider_catalog_entry(provider) {
        for model in entry.known_models {
            push(model);
        }
        push(entry.default_model);
    }
    if models.is_empty() {
        push("custom-model");
    }
    models
}

/// Applies the catalog defaults for a newly selected provider route.
pub fn apply_provider_defaults(entry: &ProviderCatalogEntry, profile: &mut ProviderProfile) {
    profile.connection = entry.connection.to_owned();
    profile.provider = entry.profile_provider.to_owned();
    profile.model = entry.default_model.to_owned();
    profile.auth = entry.default_auth.to_owned();
    profile.base_url = entry.base_url.map(str::to_owned);
    profile.configured = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_unique_and_resolve() {
        let ids = PROVIDER_CATALOG_IDS.into_iter().collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), provider_catalog().len());
        for id in ids {
            assert_eq!(provider_catalog_entry(id).map(|entry| entry.id), Some(id));
        }
    }

    #[test]
    fn every_catalog_default_is_a_valid_configured_profile() {
        for entry in provider_catalog() {
            let mut profile = ProviderProfile::default();
            apply_provider_defaults(entry, &mut profile);
            profile.configured = true;
            profile.validate().expect(entry.id);
        }
    }

    #[test]
    fn model_options_preserve_current_and_merge_discovery() {
        let options = provider_model_options(
            "openai-oauth",
            "custom-current",
            &["gpt-live".to_owned(), "custom-current".to_owned()],
        );
        assert_eq!(options[0], "custom-current");
        assert!(options.contains(&"gpt-live".to_owned()));
        assert_eq!(options.iter().filter(|model| *model == "custom-current").count(), 1);
    }

    #[test]
    fn unknown_custom_provider_remains_representable() {
        let providers = provider_ids_with_current("private-gateway");
        assert_eq!(providers[0], "private-gateway");
    }
}
