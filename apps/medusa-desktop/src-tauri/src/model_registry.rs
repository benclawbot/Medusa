use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use medusa_config::{
    Config, ModelDiscoveryCache, ModelRegistry, ProviderProfileCatalog, model_registry_for_profile,
    provider_catalog_entry_for_profile,
};
use medusa_provider::discover_models;

use crate::credentials::{CredentialStore, SystemCredentialStore};

static DISCOVERY_CACHE: OnceLock<Mutex<BTreeMap<String, ModelDiscoveryCache>>> = OnceLock::new();

#[tauri::command]
pub fn desktop_model_registry(refresh: Option<bool>) -> Result<ModelRegistry, String> {
    let catalog = ProviderProfileCatalog::user().map_err(|error| error.to_string())?;
    let snapshot = catalog.snapshot().map_err(|error| error.to_string())?;
    let profile = snapshot.profile;
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
        let guard = cache.lock().map_err(|_| "model discovery cache is poisoned")?;
        if let Some(cached) = guard.get(&provider_id).filter(|cached| cached.fresh_at(now)) {
            return Ok(model_registry_for_profile(
                &profile,
                Err(medusa_config::DiscoveryFailure::Offline),
                Some(cached),
                now,
            ));
        }
    }

    let credential = SystemCredentialStore.load(&profile.provider)?;
    match discover_models(&config, credential.as_deref()) {
        Ok(models) => {
            let registry = model_registry_for_profile(&profile, Ok(&models), None, now);
            let mut guard = cache.lock().map_err(|_| "model discovery cache is poisoned")?;
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
            let guard = cache.lock().map_err(|_| "model discovery cache is poisoned")?;
            Ok(model_registry_for_profile(
                &profile,
                Err(error.fallback_kind()),
                guard.get(&provider_id),
                now,
            ))
        }
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
