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
#[serde(rename_all = "snake_case", tag = "status")]
pub enum CacheObservationV1 {
    Unknown,
    Observed {
        read_tokens: Option<u64>,
        write_tokens: Option<u64>,
        hit: Option<bool>,
    },
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
    #[serde(default)]
    pub estimated_bytes: Option<u64>,
    #[serde(default)]
    pub actual_bytes: Option<u64>,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelExperienceDiffV1 {
    pub schema_version: u16,
    pub from_contract_fingerprint: String,
    pub to_contract_fingerprint: String,
    pub stable_prefix_changed: bool,
    pub request_dynamic_changed: bool,
    pub changed_components: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelExperienceMeasurementV1 {
    pub schema_version: u16,
    pub contract_fingerprint: String,
    pub component_bytes: BTreeMap<String, u64>,
    pub total_bytes: Option<u64>,
    pub stable_prefix_bytes: Option<u64>,
    pub tool_schema_bytes: Option<u64>,
    pub transcript_bytes: Option<u64>,
    pub tool_result_bytes: Option<u64>,
    pub compaction_bytes: Option<u64>,
    pub cache: CacheObservationV1,
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

    #[must_use]
    pub fn fingerprint(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        digest(&bytes)
    }

    #[must_use]
    pub fn diff(&self, next: &Self) -> ModelExperienceDiffV1 {
        let before = self
            .components
            .iter()
            .map(|component| (component.id.as_str(), component))
            .collect::<BTreeMap<_, _>>();
        let after = next
            .components
            .iter()
            .map(|component| (component.id.as_str(), component))
            .collect::<BTreeMap<_, _>>();
        let ids = before
            .keys()
            .chain(after.keys())
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let changed_components = ids
            .into_iter()
            .filter(|id| match (before.get(id), after.get(id)) {
                (Some(left), Some(right)) => component_shape(left) != component_shape(right),
                _ => true,
            })
            .map(str::to_owned)
            .collect::<Vec<_>>();
        ModelExperienceDiffV1 {
            schema_version: MODEL_EXPERIENCE_SCHEMA_VERSION,
            from_contract_fingerprint: self.fingerprint(),
            to_contract_fingerprint: next.fingerprint(),
            stable_prefix_changed: self.stable_prefix_fingerprint != next.stable_prefix_fingerprint,
            request_dynamic_changed: self.request_dynamic_fingerprint
                != next.request_dynamic_fingerprint,
            changed_components,
        }
    }

    #[must_use]
    pub fn measurement(&self, cache: CacheObservationV1) -> ModelExperienceMeasurementV1 {
        let component_bytes = self
            .components
            .iter()
            .filter_map(|component| {
                component
                    .actual_bytes
                    .or(component.estimated_bytes)
                    .map(|bytes| (component.id.clone(), bytes))
            })
            .collect::<BTreeMap<_, _>>();
        let total_bytes = sum_bytes(component_bytes.values().copied());
        let stable_prefix_bytes = sum_bytes(
            self.components
                .iter()
                .filter(|component| {
                    matches!(
                        component.stability,
                        ComponentStability::Static | ComponentStability::SessionStable
                    )
                })
                .filter_map(|component| component.actual_bytes.or(component.estimated_bytes)),
        );
        let tool_schema_bytes = component_bytes.get("tools").copied();
        let transcript_bytes = component_bytes.get("messages").copied();
        let tool_result_bytes = component_bytes.get("tool_results").copied();
        let compaction_bytes = component_bytes.get("compaction").copied();
        ModelExperienceMeasurementV1 {
            schema_version: MODEL_EXPERIENCE_SCHEMA_VERSION,
            contract_fingerprint: self.fingerprint(),
            component_bytes,
            total_bytes,
            stable_prefix_bytes,
            tool_schema_bytes,
            transcript_bytes,
            tool_result_bytes,
            compaction_bytes,
            cache,
        }
    }
}

fn component_shape(
    component: &ModelExperienceComponentV1,
) -> (&str, &str, u32, &str, ComponentStability, &str) {
    (
        &component.id,
        &component.version,
        component.insertion_order,
        &component.location,
        component.stability,
        &component.fingerprint,
    )
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

fn sum_bytes(values: impl Iterator<Item = u64>) -> Option<u64> {
    let values = values.collect::<Vec<_>>();
    (!values.is_empty()).then(|| values.into_iter().sum())
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
            estimated_bytes: Some(10),
            actual_bytes: None,
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

    #[test]
    fn adjacent_diff_identifies_dynamic_changes_without_false_stable_prefix_regressions() {
        let first = ModelExperienceContractV1::new(
            "implementation",
            vec![
                component("system", 1, ComponentStability::Static),
                component("messages", 2, ComponentStability::RequestDynamic),
            ],
            "tools-1",
        );
        let second = ModelExperienceContractV1::new(
            "implementation",
            vec![
                component("system", 1, ComponentStability::Static),
                component("messages-2", 2, ComponentStability::RequestDynamic),
            ],
            "tools-1",
        );

        let diff = first.diff(&second);
        assert!(!diff.stable_prefix_changed);
        assert!(diff.request_dynamic_changed);
        assert_eq!(diff.changed_components, vec!["messages", "messages-2"]);
    }

    #[test]
    fn measurement_reports_provider_cache_as_unknown_until_observed() {
        let contract = ModelExperienceContractV1::new(
            "implementation",
            vec![component("system", 1, ComponentStability::Static)],
            "tools-1",
        );
        let measurement = contract.measurement(CacheObservationV1::Unknown);
        assert_eq!(measurement.cache, CacheObservationV1::Unknown);
        assert_eq!(measurement.total_bytes, Some(10));
        assert_eq!(measurement.stable_prefix_bytes, Some(10));
        assert_eq!(measurement.component_bytes["system"], 10);
    }

    #[test]
    fn observed_cache_metrics_round_trip_without_claiming_more_than_reported() {
        let observation = CacheObservationV1::Observed {
            read_tokens: Some(120),
            write_tokens: None,
            hit: Some(true),
        };
        let encoded = serde_json::to_string(&observation).expect("cache observation");
        let decoded: CacheObservationV1 = serde_json::from_str(&encoded).expect("cache decode");
        assert_eq!(decoded, observation);
    }
}
