use std::collections::BTreeSet;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_PROTECTED_BYTES: usize = 96 * 1024;
const MAX_AUXILIARY_INPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub(crate) enum ContextTier {
    UserConstraint,
    Decision,
    EditAnchor,
    Failure,
    Verification,
    RecentTurn,
    RepositoryDiscovery,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ContextItem {
    pub tier: ContextTier,
    pub reference: String,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ProtectedContext {
    pub stable_prefix: String,
    pub retained: Vec<ContextItem>,
    pub pruned_references: Vec<String>,
    pub cache_key: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum AuxiliaryWorkload {
    RepositoryDiscoverySummary,
    SkillCatalogExtraction,
    ConversationCompaction,
    RetrievalReranking,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct AuxiliaryRoute {
    pub workload: AuxiliaryWorkload,
    pub provider: String,
    pub cache_compatible: bool,
    pub estimated_cache_break_bytes: usize,
    pub fallback_provider: Option<String>,
    pub reason: String,
}

pub(crate) fn build(
    stable_prefix: &str,
    items: impl IntoIterator<Item = ContextItem>,
) -> ProtectedContext {
    let mut retained = Vec::new();
    let mut pruned_references = Vec::new();
    let mut seen = BTreeSet::new();
    let mut used = stable_prefix.len();
    for item in items {
        let normalized = item.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() || !seen.insert((item.tier, normalized.clone())) {
            pruned_references.push(item.reference);
            continue;
        }
        let cost = normalized
            .len()
            .saturating_add(item.reference.len())
            .saturating_add(32);
        if used.saturating_add(cost) > MAX_PROTECTED_BYTES && item.tier >= ContextTier::RecentTurn {
            pruned_references.push(item.reference);
            continue;
        }
        used = used.saturating_add(cost);
        retained.push(ContextItem {
            text: normalized,
            ..item
        });
    }
    let mut hasher = Sha256::new();
    hasher.update(stable_prefix.as_bytes());
    for item in &retained {
        hasher.update([item.tier as u8]);
        hasher.update(item.reference.as_bytes());
        hasher.update(item.text.as_bytes());
    }
    ProtectedContext {
        stable_prefix: stable_prefix.to_owned(),
        retained,
        pruned_references,
        cache_key: hex::encode(&hasher.finalize()[..12]),
    }
}

pub(crate) fn render(context: &ProtectedContext) -> String {
    let mut output = context.stable_prefix.clone();
    for item in &context.retained {
        output.push_str("\n\n[protected-context tier=");
        output.push_str(match item.tier {
            ContextTier::UserConstraint => "user-constraint",
            ContextTier::Decision => "decision",
            ContextTier::EditAnchor => "edit-anchor",
            ContextTier::Failure => "failure",
            ContextTier::Verification => "verification",
            ContextTier::RecentTurn => "recent-turn",
            ContextTier::RepositoryDiscovery => "repository-discovery",
        });
        output.push_str(" ref=");
        output.push_str(&item.reference);
        output.push_str("]\n");
        output.push_str(&item.text);
    }
    output.push_str("\n\n[context-cache key=");
    output.push_str(&context.cache_key);
    output.push_str(" retained=");
    output.push_str(&context.retained.len().to_string());
    output.push_str(" pruned=");
    output.push_str(&context.pruned_references.len().to_string());
    output.push(']');
    output
}

pub(crate) fn route(workload: AuxiliaryWorkload, input_bytes: usize) -> MedusaResult<AuxiliaryRoute> {
    let provider = std::env::var("MEDUSA_AUXILIARY_PROVIDER").unwrap_or_else(|_| "primary".into());
    let primary = std::env::var("MEDUSA_PROVIDER").unwrap_or_else(|_| "primary".into());
    route_for_providers(workload, input_bytes, &provider, &primary)
}

fn route_for_providers(
    workload: AuxiliaryWorkload,
    input_bytes: usize,
    provider: &str,
    primary: &str,
) -> MedusaResult<AuxiliaryRoute> {
    if input_bytes > MAX_AUXILIARY_INPUT_BYTES {
        return Err(MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            format!("auxiliary workload {workload:?} exceeds its protected context allowance"),
        ));
    }
    let cache_compatible = provider == primary;
    Ok(AuxiliaryRoute {
        workload,
        provider: provider.to_owned(),
        cache_compatible,
        estimated_cache_break_bytes: if cache_compatible { 0 } else { input_bytes },
        fallback_provider: (provider != primary).then(|| primary.to_owned()),
        reason: if cache_compatible {
            "selected provider preserves the stable prompt prefix and prompt-cache reuse".into()
        } else {
            "provider switch is explicit; cache-break cost is recorded before execution and fallback preserves task state".into()
        },
    })
}

pub(crate) fn format_route(route: &AuxiliaryRoute) -> String {
    format!(
        "[aux-route workload={:?}; provider={}; cache_compatible={}; cache_break_bytes={}; fallback={}; reason={}]",
        route.workload,
        route.provider,
        route.cache_compatible,
        route.estimated_cache_break_bytes,
        route.fallback_provider.as_deref().unwrap_or("none"),
        route.reason
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constraints_survive_low_value_pruning() {
        let context = build(
            "stable",
            [
                ContextItem {
                    tier: ContextTier::UserConstraint,
                    reference: "user:1".into(),
                    text: "Do not modify tests".into(),
                },
                ContextItem {
                    tier: ContextTier::RecentTurn,
                    reference: "turn:1".into(),
                    text: "   ".into(),
                },
            ],
        );
        assert!(render(&context).contains("Do not modify tests"));
        assert_eq!(context.pruned_references, vec!["turn:1"]);
    }

    #[test]
    fn provider_switch_costs_cache_break_before_work() {
        let decision = route_for_providers(
            AuxiliaryWorkload::RetrievalReranking,
            512,
            "secondary",
            "primary",
        )
        .expect("route");
        assert!(!decision.cache_compatible);
        assert_eq!(decision.estimated_cache_break_bytes, 512);
        assert_eq!(decision.fallback_provider.as_deref(), Some("primary"));
    }
}
