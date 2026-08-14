use medusa_config::{DiscoveredModel, ModelRegistry, model_registry};

/// Build the canonical model registry for TUI/CLI surfaces from provider-discovered model IDs.
#[must_use]
pub fn registry_for_model_ids(
    provider: &str,
    current_model: &str,
    discovered_model_ids: &[String],
) -> ModelRegistry {
    let discovered = discovered_model_ids
        .iter()
        .map(|id| DiscoveredModel {
            id: id.clone(),
            display_name: None,
        })
        .collect::<Vec<_>>();
    model_registry(provider, current_model, Ok(&discovered), None, 0)
}

/// Return model IDs in the canonical registry ordering used by model pickers.
#[must_use]
pub fn model_ids(
    provider: &str,
    current_model: &str,
    discovered_model_ids: &[String],
) -> Vec<String> {
    registry_for_model_ids(provider, current_model, discovered_model_ids)
        .models
        .into_iter()
        .map(|model| model.id)
        .collect()
}
