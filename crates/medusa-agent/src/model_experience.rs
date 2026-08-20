//! Versioned context and cache-shape contract for model-facing components.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MODEL_EXPERIENCE_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentStability {
    Static,
    SessionStable,
    TurnStable,
    RequestDynamic,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyClass {
    Public,
    UserContent,
    Sensitive,
    SecretExcluded,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelExperienceComponentV1 {
    pub id: String,
    pub version: String,
    pub insertion_order: u32,
    pub location: String,
    pub stability: ComponentStability,
    pub estimated_tokens: Option<u64>,
    pub actual_tokens: Option<u64>,
    pub cache_eligible: Option<bool>,
    pub cache_breaking_dimensions: Vec<String>,
    pub privacy_class: PrivacyClass,
    pub max_bytes: Option<u64>,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelExperienceContractV1 {
    pub schema_version: u16,
    pub phase: String,
    pub components: Vec<ModelExperienceComponentV1>,
    pub tool_schema_fingerprint: String,
    pub stable_prefix_fingerprint: String,
    pub request_dynamic_fingerprint: String,
    pub estimated_total_tokens: Option<u64>,
    pub actual_total_tokens: Option<u64>,
}

impl ModelExperienceContractV1 {
    pub fn new(
        phase: impl Into<String>,
        mut components: Vec<ModelExperienceComponentV1>,
        tool_schema_fingerprint: impl Into<String>,
    ) -> Self {
        components.sort_by_key(|component| component.insertion_order);
        let stable_prefix = components
            .iter()
            .filter(|component| {
                matches!(
                    component.stability,
                    ComponentStability::Static | ComponentStability::SessionStable
                )
            })
            .map(component_material)
            .collect::<Vec<_>>()
            .join("\n");
        let dynamic = components
            .iter()
            .filter(|component| {
                matches!(
                    component.stability,
                    ComponentStability::TurnStable | ComponentStability::RequestDynamic
                )
            })
            .map(component_material)
            .collect::<Vec<_>>()
            .join("\n");
        let estimated_total_tokens =
            sum_known(components.iter().filter_map(|item| item.estimated_tokens));
        let actual_total_tokens =
            sum_known(components.iter().filter_map(|item| item.actual_tokens));
        Self {
            schema_version: MODEL_EXPERIENCE_SCHEMA_VERSION,
            phase: phase.into(),
            components,
            tool_schema_fingerprint: tool_schema_fingerprint.into(),
            stable_prefix_fingerprint: digest(stable_prefix.as_bytes()),
            request_dynamic_fingerprint: digest(dynamic.as_bytes()),
            estimated_total_tokens,
            actual_total_tokens,
        }
    }

    #[must_use]
    pub fn component_fingerprints(&self) -> BTreeMap<String, String> {
        self.components
            .iter()
            .map(|component| (component.id.clone(), component.fingerprint.clone()))
            .collect()
    }
}

fn component_material(component: &ModelExperienceComponentV1) -> String {
    serde_json::to_string(&(
        &component.id,
        &component.version,
        component.insertion_order,
        &component.location,
        component.stability,
        &component.fingerprint,
    ))
    .unwrap_or_default()
}

fn sum_known(values: impl Iterator<Item = u64>) -> Option<u64> {
    let values = values.collect::<Vec<_>>();
    (!values.is_empty()).then(|| values.into_iter().sum())
}

fn digest(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(
        id: &str,
        order: u32,
        stability: ComponentStability,
    ) -> ModelExperienceComponentV1 {
        ModelExperienceComponentV1 {
            id: id.to_owned(),
            version: "1".to_owned(),
            insertion_order: order,
            location: "system".to_owned(),
            stability,
            estimated_tokens: Some(10),
            actual_tokens: None,
            cache_eligible: Some(true),
            cache_breaking_dimensions: vec!["tool_schema".to_owned()],
            privacy_class: PrivacyClass::Public,
            max_bytes: Some(1024),
            fingerprint: format!("fingerprint-{id}"),
        }
    }

    #[test]
    fn stable_prefix_is_ordered_and_dynamic_changes_do_not_rewrite_it() {
        let first = ModelExperienceContractV1::new(
            "implementation",
            vec![
                component("dynamic", 2, ComponentStability::RequestDynamic),
                component("system", 1, ComponentStability::Static),
            ],
            "tools-1",
        );
        let second = ModelExperienceContractV1::new(
            "implementation",
            vec![
                component("system", 1, ComponentStability::Static),
                component("dynamic-2", 2, ComponentStability::RequestDynamic),
            ],
            "tools-1",
        );
        assert_eq!(
            first.stable_prefix_fingerprint,
            second.stable_prefix_fingerprint
        );
        assert_ne!(
            first.request_dynamic_fingerprint,
            second.request_dynamic_fingerprint
        );
    }
}
