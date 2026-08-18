use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{ProviderProfile, provider_catalog_entry, provider_catalog_entry_for_profile};

pub const MODEL_DISCOVERY_CACHE_TTL_SECONDS: u64 = 15 * 60;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelAvailability {
    Available,
    NotDiscovered,
    NotAuthorized,
    Unsupported,
    TemporarilyUnavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelSource {
    Curated,
    Discovered,
    Cached,
    Custom,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCapabilities {
    pub tool_calling: bool,
    pub image_input: bool,
    pub audio_input: bool,
    pub realtime: bool,
    pub reasoning: bool,
    pub reasoning_effort_levels: Vec<String>,
    pub streaming: bool,
    pub structured_output: bool,
    pub prompt_caching: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelMetadata {
    pub id: String,
    pub display_name: String,
    pub provider_id: String,
    pub profile_provider: String,
    pub availability: ModelAvailability,
    pub source: ModelSource,
    pub context_limit: Option<u64>,
    pub output_limit: Option<u64>,
    pub capabilities: ModelCapabilities,
    pub deprecated: bool,
    pub replacement: Option<String>,
    pub recommended: bool,
    pub use_case_hint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveredModel {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDiscoveryCache {
    pub provider_id: String,
    pub fetched_at_unix_seconds: u64,
    pub models: Vec<DiscoveredModel>,
}

impl ModelDiscoveryCache {
    #[must_use]
    pub fn fresh_at(&self, now_unix_seconds: u64) -> bool {
        now_unix_seconds.saturating_sub(self.fetched_at_unix_seconds)
            <= MODEL_DISCOVERY_CACHE_TTL_SECONDS
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryFailure {
    NotAuthorized,
    Unsupported,
    TemporarilyUnavailable,
    Offline,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRegistry {
    pub provider_id: String,
    pub models: Vec<ModelMetadata>,
    pub discovery_available: bool,
    pub used_cached_discovery: bool,
}

impl ModelRegistry {
    #[must_use]
    pub fn find(&self, model_id: &str) -> Option<&ModelMetadata> {
        self.models.iter().find(|model| model.id == model_id)
    }

    pub fn recommended(&self) -> impl Iterator<Item = &ModelMetadata> {
        self.models.iter().filter(|model| model.recommended)
    }

    #[must_use]
    pub fn search(&self, query: &str, show_all: bool) -> Vec<&ModelMetadata> {
        let needle = query.trim().to_ascii_lowercase();
        self.models
            .iter()
            .filter(|model| show_all || model.recommended || model.source == ModelSource::Custom)
            .filter(|model| {
                needle.is_empty()
                    || model.id.to_ascii_lowercase().contains(&needle)
                    || model.display_name.to_ascii_lowercase().contains(&needle)
                    || model
                        .use_case_hint
                        .as_deref()
                        .is_some_and(|hint| hint.to_ascii_lowercase().contains(&needle))
            })
            .collect()
    }
}

#[must_use]
pub fn model_registry_for_profile(
    profile: &ProviderProfile,
    discovered: Result<&[DiscoveredModel], DiscoveryFailure>,
    cache: Option<&ModelDiscoveryCache>,
    now_unix_seconds: u64,
) -> ModelRegistry {
    let provider_id = provider_catalog_entry_for_profile(profile)
        .map_or(profile.provider.as_str(), |entry| entry.id);
    model_registry(
        provider_id,
        &profile.model,
        discovered,
        cache,
        now_unix_seconds,
    )
}

#[must_use]
pub fn model_registry(
    provider: &str,
    current_model: &str,
    discovered: Result<&[DiscoveredModel], DiscoveryFailure>,
    cache: Option<&ModelDiscoveryCache>,
    now_unix_seconds: u64,
) -> ModelRegistry {
    let catalog = provider_catalog_entry(provider);
    let provider_id = catalog.map_or(provider, |entry| entry.id).to_owned();
    let profile_provider = catalog
        .map_or(provider, |entry| entry.profile_provider)
        .to_owned();
    let supports_discovery = catalog.is_some_and(|entry| entry.discover_models);

    let (live_models, availability, used_cached_discovery) = match discovered {
        Ok(models) => (models, ModelAvailability::Available, false),
        Err(failure) => {
            let cached = cache.filter(|cached| {
                cached.provider_id == provider_id && cached.fresh_at(now_unix_seconds)
            });
            if let Some(cached) = cached {
                (cached.models.as_slice(), ModelAvailability::Available, true)
            } else {
                let availability = match failure {
                    DiscoveryFailure::NotAuthorized => ModelAvailability::NotAuthorized,
                    DiscoveryFailure::Unsupported => ModelAvailability::Unsupported,
                    DiscoveryFailure::TemporarilyUnavailable | DiscoveryFailure::Offline => {
                        ModelAvailability::TemporarilyUnavailable
                    }
                };
                (&[][..], availability, false)
            }
        }
    };

    let mut by_id = BTreeMap::<String, ModelMetadata>::new();
    if let Some(entry) = catalog {
        for known in entry.known_models {
            let mut model = curated_model(entry.id, entry.profile_provider, known);
            model.availability = if supports_discovery && live_models.is_empty() {
                availability
            } else {
                ModelAvailability::Available
            };
            model.recommended = *known == entry.default_model;
            by_id.insert(model.id.clone(), model);
        }
    }

    for discovered_model in live_models {
        if discovered_model.id.trim().is_empty() {
            continue;
        }
        let entry = by_id.entry(discovered_model.id.clone()).or_insert_with(|| {
            curated_model(&provider_id, &profile_provider, &discovered_model.id)
        });
        entry.display_name = discovered_model
            .display_name
            .clone()
            .unwrap_or_else(|| discovered_model.id.clone());
        entry.availability = ModelAvailability::Available;
        entry.source = if used_cached_discovery {
            ModelSource::Cached
        } else {
            ModelSource::Discovered
        };
    }

    if !current_model.trim().is_empty() && !by_id.contains_key(current_model) {
        let mut current = curated_model(&provider_id, &profile_provider, current_model);
        current.source = ModelSource::Custom;
        current.availability = ModelAvailability::Available;
        current.use_case_hint = Some("Currently configured custom model".to_owned());
        by_id.insert(current_model.to_owned(), current);
    }

    let mut models = by_id.into_values().collect::<Vec<_>>();
    models.sort_by_key(|model| {
        let discovered_rank = live_models
            .iter()
            .position(|discovered| discovered.id == model.id)
            .unwrap_or(usize::MAX);
        let curated_rank = catalog
            .and_then(|entry| {
                entry
                    .known_models
                    .iter()
                    .position(|known| *known == model.id)
            })
            .unwrap_or(usize::MAX);
        (
            model.id != current_model,
            discovered_rank,
            curated_rank,
            model.display_name.to_ascii_lowercase(),
        )
    });

    ModelRegistry {
        provider_id,
        models,
        discovery_available: supports_discovery,
        used_cached_discovery,
    }
}

#[must_use]
pub fn model_capabilities(provider: &str, model: &str) -> ModelCapabilities {
    curated_model(provider, provider, model).capabilities
}

/// Returns the authoritative context limit for a fixed vendor provider/model pair.
///
/// Provider identity is part of the key so local, compatible, managed, discovered, and custom
/// routes can reuse a vendor model label without inheriting that vendor's context budget.
#[must_use]
pub fn model_context_limit(provider: &str, model: &str) -> Option<u64> {
    let normalized = model.to_ascii_lowercase();
    match (provider, normalized.as_str()) {
        ("minimax", "minimax-m3") => Some(1_000_000),
        ("minimax", "minimax-m2.7" | "minimax-m2.7-highspeed" | "minimax-m2.5") => Some(204_800),
        ("anthropic", "claude-opus-4-6" | "claude-sonnet-4-6") => Some(1_000_000),
        ("anthropic", "claude-haiku-4-5") => Some(200_000),
        ("openai", "gpt-5.1-codex" | "gpt-5.1" | "gpt-5-mini") | ("openai-oauth", "gpt-5") => {
            Some(400_000)
        }
        _ => None,
    }
}

fn curated_model(provider_id: &str, profile_provider: &str, model: &str) -> ModelMetadata {
    let normalized = model.to_ascii_lowercase();
    let is_openai = provider_id == "openai" || provider_id == "openai-oauth";
    let is_anthropic = provider_id == "anthropic" || provider_id == "anthropic-compatible";
    let is_minimax = provider_id == "minimax";
    let reasoning = is_openai || normalized.contains("opus") || normalized.contains("sonnet");
    let image_input = is_openai || is_anthropic;
    ModelMetadata {
        id: model.to_owned(),
        display_name: model.to_owned(),
        provider_id: provider_id.to_owned(),
        profile_provider: profile_provider.to_owned(),
        availability: ModelAvailability::NotDiscovered,
        source: ModelSource::Curated,
        context_limit: model_context_limit(provider_id, model),
        output_limit: None,
        capabilities: ModelCapabilities {
            tool_calling: is_openai || is_anthropic || is_minimax,
            image_input,
            audio_input: is_openai,
            realtime: is_openai && normalized.contains("realtime"),
            reasoning,
            reasoning_effort_levels: if reasoning {
                vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()]
            } else {
                Vec::new()
            },
            streaming: true,
            structured_output: is_openai,
            prompt_caching: is_anthropic || is_openai,
        },
        deprecated: false,
        replacement: None,
        recommended: false,
        use_case_hint: None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn merges_curated_discovered_and_current_custom_models() {
        let discovered = [DiscoveredModel {
            id: "gpt-live".to_owned(),
            display_name: Some("GPT Live".to_owned()),
        }];
        let registry = model_registry("openai-oauth", "private-model", Ok(&discovered), None, 10);
        assert_eq!(registry.models[0].id, "private-model");
        assert_eq!(
            registry.find("gpt-live").unwrap().source,
            ModelSource::Discovered
        );
        assert_eq!(
            registry.find("private-model").unwrap().source,
            ModelSource::Custom
        );
        assert!(registry.find("gpt-5").is_some());
    }

    #[test]
    fn discovery_failure_uses_fresh_cache_then_curated_fallback() {
        let cache = ModelDiscoveryCache {
            provider_id: "openai-oauth".to_owned(),
            fetched_at_unix_seconds: 100,
            models: vec![DiscoveredModel {
                id: "cached-model".to_owned(),
                display_name: None,
            }],
        };
        let cached = model_registry(
            "openai-oauth",
            "gpt-5",
            Err(DiscoveryFailure::Offline),
            Some(&cache),
            200,
        );
        assert!(cached.used_cached_discovery);
        assert_eq!(
            cached.find("cached-model").unwrap().source,
            ModelSource::Cached
        );

        let stale = model_registry(
            "openai-oauth",
            "gpt-5",
            Err(DiscoveryFailure::Offline),
            Some(&cache),
            100 + MODEL_DISCOVERY_CACHE_TTL_SECONDS + 1,
        );
        assert!(!stale.used_cached_discovery);
        assert!(stale.find("gpt-5").is_some());
    }

    #[test]
    fn auth_failure_is_distinct_and_never_drops_current_model() {
        let registry = model_registry(
            "openai-oauth",
            "configured-private",
            Err(DiscoveryFailure::NotAuthorized),
            None,
            1,
        );
        assert_eq!(
            registry.find("gpt-5").unwrap().availability,
            ModelAvailability::NotAuthorized
        );
        assert_eq!(
            registry.find("configured-private").unwrap().availability,
            ModelAvailability::Available
        );
    }

    #[test]
    fn search_hides_non_recommended_models_until_show_all() {
        let registry = model_registry("openai", "gpt-5.1-codex", Ok(&[]), None, 1);
        assert_eq!(registry.search("", false).len(), 1);
        assert!(
            registry
                .search("mini", true)
                .iter()
                .any(|model| model.id == "gpt-5-mini")
        );
    }

    #[test]
    fn capabilities_are_model_metadata_not_frontend_provider_checks() {
        let registry = model_registry("anthropic", "claude-sonnet-4-6", Ok(&[]), None, 1);
        let model = registry.find("claude-sonnet-4-6").unwrap();
        assert!(model.capabilities.image_input);
        assert!(model.capabilities.tool_calling);
        assert!(model.capabilities.reasoning);
    }

    #[test]
    fn fixed_vendor_catalog_models_have_authoritative_context_limits() {
        for provider in ["minimax", "anthropic", "openai", "openai-oauth"] {
            let catalog = provider_catalog_entry(provider).expect("fixed vendor catalog");
            let registry = model_registry(provider, catalog.default_model, Ok(&[]), None, 1);
            for model_id in catalog.known_models {
                let expected = model_context_limit(provider, model_id).unwrap_or_else(|| {
                    panic!(
                        "catalogued fixed-vendor model {provider}/{model_id} needs context metadata"
                    )
                });
                let metadata = registry.find(model_id).expect("catalog model metadata");
                assert_eq!(metadata.context_limit, Some(expected));
            }
        }

        assert_eq!(
            model_context_limit("minimax", "MiniMax-M3"),
            Some(1_000_000)
        );
        assert_eq!(
            model_context_limit("minimax", "MiniMax-M2.7"),
            Some(204_800)
        );
        assert_eq!(
            model_context_limit("minimax", "MiniMax-M2.7-highspeed"),
            Some(204_800)
        );
        assert_eq!(
            model_context_limit("minimax", "MiniMax-M2.5"),
            Some(204_800)
        );
        assert_eq!(
            model_context_limit("anthropic", "claude-sonnet-4-6"),
            Some(1_000_000)
        );
        assert_eq!(
            model_context_limit("anthropic", "claude-haiku-4-5"),
            Some(200_000)
        );
        assert_eq!(
            model_context_limit("openai", "gpt-5.1-codex"),
            Some(400_000)
        );
        assert_eq!(model_context_limit("openai-oauth", "gpt-5"), Some(400_000));
    }

    #[test]
    fn context_limits_require_the_authoritative_provider_route() {
        assert_eq!(model_context_limit("local", "MiniMax-M3"), None);
        assert_eq!(model_context_limit("openai-compatible", "gpt-5.1"), None);
        assert_eq!(
            model_context_limit("anthropic-compatible", "claude-opus-4-6"),
            None
        );
        assert_eq!(model_context_limit("omniroute", "gpt-5"), None);
        assert_eq!(model_context_limit("openai", "custom-model"), None);

        let local = model_registry("local", "MiniMax-M3", Ok(&[]), None, 1);
        assert_eq!(local.find("MiniMax-M3").unwrap().context_limit, None);
    }

    #[test]
    fn cache_provider_must_match() {
        let cache = ModelDiscoveryCache {
            provider_id: "different-provider".to_owned(),
            fetched_at_unix_seconds: 100,
            models: vec![DiscoveredModel {
                id: "wrong".to_owned(),
                display_name: None,
            }],
        };
        let registry = model_registry(
            "openai-oauth",
            "gpt-5",
            Err(DiscoveryFailure::Offline),
            Some(&cache),
            100,
        );
        assert!(registry.find("wrong").is_none());
    }

    #[test]
    fn sources_are_unique_after_merge() {
        let discovered = [
            DiscoveredModel {
                id: "gpt-5".to_owned(),
                display_name: None,
            },
            DiscoveredModel {
                id: "gpt-5".to_owned(),
                display_name: None,
            },
        ];
        let registry = model_registry("openai-oauth", "gpt-5", Ok(&discovered), None, 1);
        let ids = registry
            .models
            .iter()
            .map(|model| &model.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), registry.models.len());
    }
}
