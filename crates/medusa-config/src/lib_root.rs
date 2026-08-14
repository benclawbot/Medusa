#[path = "lib.rs"]
mod implementation;

pub use implementation::*;

pub mod model_registry;
pub mod provider_catalog;

pub use model_registry::{
    DiscoveredModel, DiscoveryFailure, MODEL_DISCOVERY_CACHE_TTL_SECONDS, ModelAvailability,
    ModelCapabilities, ModelDiscoveryCache, ModelMetadata, ModelRegistry, ModelSource,
    model_capabilities, model_registry, model_registry_for_profile,
};
pub use provider_catalog::{
    PROVIDER_CATALOG_IDS, ProviderCatalogEntry, ProviderSupportTier, apply_provider_defaults,
    provider_catalog, provider_catalog_entry, provider_catalog_entry_for_profile,
    provider_ids_with_current, provider_model_options, provider_runtime_protocol,
    provider_support_tier,
};
