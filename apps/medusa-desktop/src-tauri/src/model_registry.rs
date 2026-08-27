use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use medusa_config::{
    Config, DiscoveredModel, DiscoveryFailure, ModelDiscoveryCache, ModelRegistry, ProviderProfile,
    ProviderProfileCatalog, apply_provider_defaults, model_registry_for_profile,
    provider_catalog_entry, provider_catalog_entry_for_profile,
};
use medusa_provider::discover_models;

use crate::credentials::{CredentialStore, SystemCredentialStore};

static DISCOVERY_CACHE: OnceLock<Mutex<BTreeMap<String, ModelDiscoveryCache>>> = OnceLock::new();

fn profile_for_discovery(requested_provider: Option<&str>) -> Result<ProviderProfile, String> {
    let catalog = ProviderProfileCatalog::user().map_err(|error| error.to_string())?;
    let snapshot = catalog.snapshot().map_err(|error| error.to_string())?;
    let current = snapshot.profile;
    let Some(provider) = requested_provider
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(current);
    };

    if provider == current.provider
        || provider_catalog_entry_for_profile(&current).is_some_and(|entry| entry.id == provider)
    {
        return Ok(current);
    }

    let entry = provider_catalog_entry(provider)
        .ok_or_else(|| format!("unknown provider `{provider}` for model discovery"))?;
    if let Some(reason) = entry.disabled_reason {
        return Err(format!(
            "provider {} is unavailable: {reason}",
            entry.display_name
        ));
    }
    let mut profile = ProviderProfile::default();
    apply_provider_defaults(entry, &mut profile);
    profile.configured = true;
    Ok(profile)
}

#[tauri::command]
pub fn desktop_model_registry(
    refresh: Option<bool>,
    provider: Option<String>,
) -> Result<ModelRegistry, String> {
    let profile = profile_for_discovery(provider.as_deref())?;
    let provider_id = provider_catalog_entry_for_profile(&profile)
        .map_or(profile.provider.as_str(), |entry| entry.id)
        .to_owned();
    let config = Config::load_layers_with_provider_profile(
        &profile,
        None,
        None,
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .map_err(|error| error.to_string())?;
    let now = now_unix_seconds();
    let cache = DISCOVERY_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));

    if refresh != Some(true) {
        let guard = cache
            .lock()
            .map_err(|_| "model discovery cache is poisoned")?;
        if let Some(cached) = guard
            .get(&provider_id)
            .filter(|cached| cached.fresh_at(now))
        {
            return Ok(model_registry_for_profile(
                &profile,
                Err(medusa_config::DiscoveryFailure::Offline),
                Some(cached),
                now,
            ));
        }
    }

    let discovery = if profile.provider == "openai-oauth" {
        medusa_runtime::discover_openai_oauth_models()
            .map(|models| {
                models
                    .into_iter()
                    .map(|id| DiscoveredModel {
                        id,
                        display_name: None,
                    })
                    .collect::<Vec<_>>()
            })
            .map_err(|error| oauth_discovery_failure(&error))
    } else {
        let credential = SystemCredentialStore.load(&profile.provider)?;
        discover_models(&config, credential.as_deref()).map_err(|error| error.fallback_kind())
    };
    match discovery {
        Ok(models) => {
            let registry = model_registry_for_profile(&profile, Ok(&models), None, now);
            let mut guard = cache
                .lock()
                .map_err(|_| "model discovery cache is poisoned")?;
            guard.insert(
                provider_id.clone(),
                ModelDiscoveryCache {
                    provider_id,
                    fetched_at_unix_seconds: now,
                    models,
                },
            );
            Ok(registry)
        }
        Err(error) => {
            let guard = cache
                .lock()
                .map_err(|_| "model discovery cache is poisoned")?;
            Ok(model_registry_for_profile(
                &profile,
                Err(error),
                guard.get(&provider_id),
                now,
            ))
        }
    }
}

fn oauth_discovery_failure(error: &str) -> DiscoveryFailure {
    let normalized = error.to_ascii_lowercase();
    if normalized.contains("not signed in")
        || normalized.contains("not authenticated")
        || normalized.contains("authenticated account")
    {
        DiscoveryFailure::NotAuthorized
    } else if normalized.contains("timed out")
        || normalized.contains("closed")
        || normalized.contains("launch")
        || normalized.contains("protocol")
    {
        DiscoveryFailure::Offline
    } else {
        DiscoveryFailure::TemporarilyUnavailable
    }
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use medusa_config::{ModelAvailability, ModelSource};

    use super::*;

    #[test]
    fn cache_is_bounded_by_the_shared_registry_ttl() {
        let cached = ModelDiscoveryCache {
            provider_id: "openai".to_owned(),
            fetched_at_unix_seconds: 100,
            models: Vec::new(),
        };
        assert!(cached.fresh_at(100));
        assert!(!cached.fresh_at(100 + medusa_config::MODEL_DISCOVERY_CACHE_TTL_SECONDS + 1));
    }

    #[test]
    fn desktop_serializes_the_same_model_contract_as_other_consumers() {
        let registry = medusa_config::model_registry("openai", "gpt-5.1-codex", Ok(&[]), None, 1);
        let model = registry.find("gpt-5.1-codex").expect("model");
        assert_eq!(model.source, ModelSource::Curated);
        assert_eq!(model.availability, ModelAvailability::Available);
        assert!(model.capabilities.tool_calling);
        assert!(model.capabilities.image_input);
    }
}
