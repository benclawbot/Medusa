//! Deterministic task-aware route selection over independently verified evidence.
//!
//! This is an extension of the provider route authority, not a second router. The existing
//! route-global ordering remains the fallback when a comparable cohort is missing, sparse, or
//! below its guardrails. A receipt is returned for every decision so callers can persist the
//! evidence window, exclusions, rationale, and fallback plan alongside the request manifest.

use serde::{Deserialize, Serialize};

use crate::{
    ProviderRouteProfile, RouteLatencyPolicy, RouteLatencyStats, latency_aware_route_order,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerifiedRouteContext {
    pub task_intent: String,
    pub language_family: String,
    pub complexity_band: String,
    pub risk_class: String,
    pub phase: String,
    pub harness_version: String,
    pub routing_policy_version: String,
    pub require_tools: bool,
    pub require_streaming: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerifiedRouteEvidence {
    pub route_id: String,
    pub task_intent: String,
    pub language_family: String,
    pub complexity_band: String,
    pub risk_class: String,
    pub phase: String,
    pub harness_version: String,
    pub routing_policy_version: String,
    pub sample_count: u64,
    pub verified_success_milli: u16,
    pub first_pass_success_milli: u16,
    pub repair_count: u64,
    pub expected_latency_ms: u64,
    pub expected_cost_per_verified_completion_microusd: Option<u64>,
    pub uncertainty_milli: u16,
    pub source_outcome_ids: Vec<String>,
}

impl VerifiedRouteEvidence {
    fn matches(&self, context: &VerifiedRouteContext) -> bool {
        self.task_intent == context.task_intent
            && self.language_family == context.language_family
            && self.complexity_band == context.complexity_band
            && self.risk_class == context.risk_class
            && self.phase == context.phase
            && self.harness_version == context.harness_version
            && self.routing_policy_version == context.routing_policy_version
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum VerifiedRoutingObjective {
    MaxVerifiedSuccess,
    MinCostPerVerifiedCompletion,
    MinTimeToVerifiedCompletion,
}

impl Default for VerifiedRoutingObjective {
    fn default() -> Self {
        Self::MaxVerifiedSuccess
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerifiedRoutingPolicy {
    pub version: u16,
    pub objective: VerifiedRoutingObjective,
    pub minimum_samples: u64,
    pub minimum_verified_success_milli: u16,
    pub maximum_uncertainty_milli: u16,
    pub maximum_latency_ms: Option<u64>,
    pub maximum_cost_per_verified_completion_microusd: Option<u64>,
    pub repair_penalty_ms: u64,
}

impl Default for VerifiedRoutingPolicy {
    fn default() -> Self {
        Self {
            version: 1,
            objective: VerifiedRoutingObjective::MaxVerifiedSuccess,
            minimum_samples: 3,
            minimum_verified_success_milli: 0,
            maximum_uncertainty_milli: 500,
            maximum_latency_ms: None,
            maximum_cost_per_verified_completion_microusd: None,
            repair_penalty_ms: 250,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExcludedVerifiedRoute {
    pub route_id: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RouteSelectionReceipt {
    pub policy_version: u16,
    pub evidence_window: String,
    pub candidate_routes: Vec<String>,
    pub excluded_routes: Vec<ExcludedVerifiedRoute>,
    pub chosen_route: Option<String>,
    pub fallback_order: Vec<usize>,
    pub relaxed_dimensions: Vec<String>,
    pub source_outcome_ids: Vec<String>,
    pub rationale: String,
    pub confident: bool,
    pub pinned: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerifiedRouteDecision {
    pub chosen_index: Option<usize>,
    pub ordered_indices: Vec<usize>,
    pub receipt: RouteSelectionReceipt,
}

/// Selects a route using comparable verified outcomes, falling back to the existing configured
/// route-global order when the evidence cannot safely support an automatic choice.
#[must_use]
pub fn select_verified_route(
    profiles: &[ProviderRouteProfile],
    global_stats: &[RouteLatencyStats],
    evidence: &[VerifiedRouteEvidence],
    context: &VerifiedRouteContext,
    policy: &VerifiedRoutingPolicy,
    pinned_index: Option<usize>,
) -> VerifiedRouteDecision {
    select_verified_route_with_latency_policy(
        profiles,
        global_stats,
        evidence,
        context,
        policy,
        RouteLatencyPolicy::default(),
        pinned_index,
    )
}

/// Variant used by the existing manager so phase-specific route-global policy remains the
/// deterministic fallback when a task cohort is cold or sparse.
#[must_use]
pub fn select_verified_route_with_latency_policy(
    profiles: &[ProviderRouteProfile],
    global_stats: &[RouteLatencyStats],
    evidence: &[VerifiedRouteEvidence],
    context: &VerifiedRouteContext,
    policy: &VerifiedRoutingPolicy,
    latency_policy: RouteLatencyPolicy,
    pinned_index: Option<usize>,
) -> VerifiedRouteDecision {
    let global_order = latency_aware_route_order(
        profiles,
        global_stats,
        context.require_tools,
        context.require_streaming,
        latency_policy,
    );
    let candidate_routes = profiles
        .iter()
        .map(|profile| profile.id.clone())
        .collect::<Vec<_>>();

    let mut receipt = RouteSelectionReceipt {
        policy_version: policy.version,
        evidence_window: format!(
            "exact:{}:{}:{}:{}:{}:{}:{}",
            context.task_intent,
            context.language_family,
            context.complexity_band,
            context.risk_class,
            context.phase,
            context.harness_version,
            context.routing_policy_version
        ),
        candidate_routes,
        excluded_routes: Vec::new(),
        chosen_route: None,
        fallback_order: global_order.clone(),
        relaxed_dimensions: Vec::new(),
        source_outcome_ids: Vec::new(),
        rationale: "configured route-global order used because comparable verified evidence was insufficient".to_owned(),
        confident: false,
        pinned: false,
    };

    for (index, profile) in profiles.iter().enumerate() {
        if global_order.contains(&index) {
            continue;
        }
        let reason = if context.require_tools && !profile.tool_calling {
            "required tool calling capability is unavailable"
        } else if context.require_streaming && !profile.streaming {
            "required streaming capability is unavailable"
        } else {
            "route was excluded by the existing route policy"
        };
        receipt.excluded_routes.push(ExcludedVerifiedRoute {
            route_id: profile.id.clone(),
            reason: reason.to_owned(),
        });
    }

    let compatible_pin = pinned_index.filter(|index| {
        profiles.get(*index).is_some_and(|profile| {
            (!context.require_tools || profile.tool_calling)
                && (!context.require_streaming || profile.streaming)
        })
    });
    if let Some(index) = compatible_pin {
        receipt.chosen_route = profiles.get(index).map(|profile| profile.id.clone());
        receipt.rationale =
            "user-pinned route is authoritative; learned evidence is diagnostic only".to_owned();
        receipt.pinned = true;
        let mut ordered = vec![index];
        ordered.extend(
            global_order
                .into_iter()
                .filter(|candidate| *candidate != index),
        );
        return VerifiedRouteDecision {
            chosen_index: Some(index),
            ordered_indices: ordered,
            receipt,
        };
    }
    if let Some(index) = pinned_index
        && compatible_pin.is_none()
        && let Some(profile) = profiles.get(index)
    {
        receipt.excluded_routes.push(ExcludedVerifiedRoute {
            route_id: profile.id.clone(),
            reason:
                "user-pinned route is unavailable or incompatible; existing policy permits fallback"
                    .to_owned(),
        });
    }

    let mut eligible = Vec::new();
    for index in &global_order {
        let Some(profile) = profiles.get(*index) else {
            continue;
        };
        let Some(observation) = evidence
            .iter()
            .find(|observation| observation.route_id == profile.id && observation.matches(context))
        else {
            receipt.excluded_routes.push(ExcludedVerifiedRoute {
                route_id: profile.id.clone(),
                reason: "no exact comparable cohort evidence".to_owned(),
            });
            continue;
        };
        if observation.sample_count < policy.minimum_samples {
            receipt.excluded_routes.push(ExcludedVerifiedRoute {
                route_id: profile.id.clone(),
                reason: format!("sparse cohort: {} samples", observation.sample_count),
            });
            continue;
        }
        if observation.uncertainty_milli > policy.maximum_uncertainty_milli {
            receipt.excluded_routes.push(ExcludedVerifiedRoute {
                route_id: profile.id.clone(),
                reason: format!(
                    "uncertainty {} exceeds guardrail",
                    observation.uncertainty_milli
                ),
            });
            continue;
        }
        if observation.verified_success_milli < policy.minimum_verified_success_milli {
            receipt.excluded_routes.push(ExcludedVerifiedRoute {
                route_id: profile.id.clone(),
                reason: format!(
                    "verified success {} below guardrail",
                    observation.verified_success_milli
                ),
            });
            continue;
        }
        if policy
            .maximum_latency_ms
            .is_some_and(|ceiling| observation.expected_latency_ms > ceiling)
        {
            receipt.excluded_routes.push(ExcludedVerifiedRoute {
                route_id: profile.id.clone(),
                reason: "latency exceeds configured ceiling".to_owned(),
            });
            continue;
        }
        if policy
            .maximum_cost_per_verified_completion_microusd
            .is_some_and(|ceiling| {
                observation
                    .expected_cost_per_verified_completion_microusd
                    .is_some_and(|cost| cost > ceiling)
            })
        {
            receipt.excluded_routes.push(ExcludedVerifiedRoute {
                route_id: profile.id.clone(),
                reason: "cost per verified completion exceeds configured ceiling".to_owned(),
            });
            continue;
        }
        if matches!(
            policy.objective,
            VerifiedRoutingObjective::MinCostPerVerifiedCompletion
        ) && observation
            .expected_cost_per_verified_completion_microusd
            .is_none()
        {
            receipt.excluded_routes.push(ExcludedVerifiedRoute {
                route_id: profile.id.clone(),
                reason: "authoritative cost is unknown".to_owned(),
            });
            continue;
        }
        receipt
            .source_outcome_ids
            .extend(observation.source_outcome_ids.iter().cloned());
        eligible.push((*index, observation));
    }

    if eligible.is_empty() {
        return VerifiedRouteDecision {
            chosen_index: global_order.first().copied(),
            ordered_indices: global_order,
            receipt,
        };
    }

    eligible.sort_by(|left, right| {
        let left_observation = left.1;
        let right_observation = right.1;
        let left_time = left_observation.expected_latency_ms.saturating_add(
            left_observation
                .repair_count
                .saturating_mul(policy.repair_penalty_ms),
        );
        let right_time = right_observation.expected_latency_ms.saturating_add(
            right_observation
                .repair_count
                .saturating_mul(policy.repair_penalty_ms),
        );
        let left_cost = left_observation
            .expected_cost_per_verified_completion_microusd
            .unwrap_or(u64::MAX);
        let right_cost = right_observation
            .expected_cost_per_verified_completion_microusd
            .unwrap_or(u64::MAX);
        match policy.objective {
            VerifiedRoutingObjective::MaxVerifiedSuccess => right_observation
                .verified_success_milli
                .cmp(&left_observation.verified_success_milli)
                .then_with(|| left_time.cmp(&right_time))
                .then_with(|| left_cost.cmp(&right_cost)),
            VerifiedRoutingObjective::MinCostPerVerifiedCompletion => left_cost
                .cmp(&right_cost)
                .then_with(|| {
                    right_observation
                        .verified_success_milli
                        .cmp(&left_observation.verified_success_milli)
                })
                .then_with(|| left_time.cmp(&right_time)),
            VerifiedRoutingObjective::MinTimeToVerifiedCompletion => left_time
                .cmp(&right_time)
                .then_with(|| {
                    right_observation
                        .verified_success_milli
                        .cmp(&left_observation.verified_success_milli)
                })
                .then_with(|| left_cost.cmp(&right_cost)),
        }
        .then_with(|| left.0.cmp(&right.0))
    });

    let learned_order = eligible.iter().map(|(index, _)| *index).collect::<Vec<_>>();
    let chosen = learned_order.first().copied();
    let mut ordered = learned_order;
    let known = ordered.clone();
    ordered.extend(
        global_order
            .into_iter()
            .filter(|index| !known.contains(index)),
    );
    receipt.chosen_route =
        chosen.and_then(|index| profiles.get(index).map(|profile| profile.id.clone()));
    receipt.rationale = format!(
        "selected by {:?} over an exact verified cohort",
        policy.objective
    );
    receipt.confident = true;
    VerifiedRouteDecision {
        chosen_index: chosen,
        ordered_indices: ordered,
        receipt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RouteRetryPolicy;

    fn profile(id: &str) -> ProviderRouteProfile {
        ProviderRouteProfile {
            id: id.to_owned(),
            provider: id.to_owned(),
            model: id.to_owned(),
            protocol: "test".to_owned(),
            endpoint: None,
            auth_source: "test".to_owned(),
            tool_calling: true,
            streaming: true,
            retry: RouteRetryPolicy::default(),
        }
    }

    fn context(language: &str, intent: &str) -> VerifiedRouteContext {
        VerifiedRouteContext {
            task_intent: intent.to_owned(),
            language_family: language.to_owned(),
            complexity_band: "medium".to_owned(),
            risk_class: "normal".to_owned(),
            phase: "implementation".to_owned(),
            harness_version: "harness-1".to_owned(),
            routing_policy_version: "route-policy-1".to_owned(),
            require_tools: true,
            require_streaming: false,
        }
    }

    fn evidence(
        route_id: &str,
        language: &str,
        intent: &str,
        success: u16,
    ) -> VerifiedRouteEvidence {
        VerifiedRouteEvidence {
            route_id: route_id.to_owned(),
            task_intent: intent.to_owned(),
            language_family: language.to_owned(),
            complexity_band: "medium".to_owned(),
            risk_class: "normal".to_owned(),
            phase: "implementation".to_owned(),
            harness_version: "harness-1".to_owned(),
            routing_policy_version: "route-policy-1".to_owned(),
            sample_count: 10,
            verified_success_milli: success,
            first_pass_success_milli: success,
            repair_count: 0,
            expected_latency_ms: 1_000,
            expected_cost_per_verified_completion_microusd: Some(100),
            uncertainty_milli: 50,
            source_outcome_ids: (0..10).map(|n| format!("{route_id}-{n}")).collect(),
        }
    }

    #[test]
    fn task_cohort_changes_route_choice_without_changing_global_order() {
        let profiles = vec![profile("route-a"), profile("route-b")];
        let global = vec![
            RouteLatencyStats {
                samples: 10,
                successes: 10,
                total_duration_ms: 10_000,
                ..Default::default()
            },
            RouteLatencyStats {
                samples: 10,
                successes: 10,
                total_duration_ms: 10_000,
                ..Default::default()
            },
        ];
        let evidence = vec![
            evidence("route-a", "rust", "debug", 950),
            evidence("route-b", "rust", "debug", 700),
            evidence("route-a", "typescript", "migration", 700),
            evidence("route-b", "typescript", "migration", 950),
        ];
        let policy = VerifiedRoutingPolicy::default();
        let rust = select_verified_route(
            &profiles,
            &global,
            &evidence,
            &context("rust", "debug"),
            &policy,
            None,
        );
        let frontend = select_verified_route(
            &profiles,
            &global,
            &evidence,
            &context("typescript", "migration"),
            &policy,
            None,
        );
        assert_eq!(rust.chosen_index, Some(0));
        assert_eq!(frontend.chosen_index, Some(1));
        assert!(rust.receipt.confident);
        assert!(frontend.receipt.confident);
    }

    #[test]
    fn repair_burden_beats_lower_request_cost() {
        let profiles = vec![profile("cheap"), profile("expensive")];
        let global = vec![RouteLatencyStats::default(); 2];
        let mut cheap = evidence("cheap", "rust", "debug", 800);
        cheap.expected_cost_per_verified_completion_microusd = Some(900);
        cheap.repair_count = 8;
        let mut expensive = evidence("expensive", "rust", "debug", 950);
        expensive.expected_cost_per_verified_completion_microusd = Some(500);
        expensive.repair_count = 1;
        let policy = VerifiedRoutingPolicy {
            objective: VerifiedRoutingObjective::MinCostPerVerifiedCompletion,
            ..Default::default()
        };
        let decision = select_verified_route(
            &profiles,
            &global,
            &[cheap, expensive],
            &context("rust", "debug"),
            &policy,
            None,
        );
        assert_eq!(decision.chosen_index, Some(1));
    }

    #[test]
    fn minimum_success_guardrail_rejects_faster_route() {
        let profiles = vec![profile("fast-unsafe"), profile("safe")];
        let global = vec![RouteLatencyStats::default(); 2];
        let mut fast = evidence("fast-unsafe", "rust", "debug", 600);
        fast.expected_latency_ms = 100;
        let mut safe = evidence("safe", "rust", "debug", 900);
        safe.expected_latency_ms = 1_000;
        let policy = VerifiedRoutingPolicy {
            objective: VerifiedRoutingObjective::MinTimeToVerifiedCompletion,
            minimum_verified_success_milli: 800,
            ..Default::default()
        };
        let decision = select_verified_route(
            &profiles,
            &global,
            &[fast, safe],
            &context("rust", "debug"),
            &policy,
            None,
        );
        assert_eq!(decision.chosen_index, Some(1));
        assert!(
            decision
                .receipt
                .excluded_routes
                .iter()
                .any(|item| item.route_id == "fast-unsafe")
        );
    }

    #[test]
    fn sparse_exact_cohort_preserves_configured_order_and_reports_low_confidence() {
        let profiles = vec![profile("route-a"), profile("route-b")];
        let global = vec![RouteLatencyStats::default(); 2];
        let mut sparse = evidence("route-b", "rust", "debug", 990);
        sparse.sample_count = 1;
        let decision = select_verified_route(
            &profiles,
            &global,
            &[sparse],
            &context("rust", "debug"),
            &VerifiedRoutingPolicy::default(),
            None,
        );
        assert_eq!(decision.chosen_index, Some(0));
        assert!(!decision.receipt.confident);
        assert_eq!(decision.receipt.fallback_order, vec![0, 1]);
    }

    #[test]
    fn pinned_route_remains_authoritative() {
        let profiles = vec![profile("route-a"), profile("route-b")];
        let global = vec![RouteLatencyStats::default(); 2];
        let evidence = vec![
            evidence("route-a", "rust", "debug", 600),
            evidence("route-b", "rust", "debug", 990),
        ];
        let decision = select_verified_route(
            &profiles,
            &global,
            &evidence,
            &context("rust", "debug"),
            &VerifiedRoutingPolicy::default(),
            Some(0),
        );
        assert_eq!(decision.chosen_index, Some(0));
        assert!(decision.receipt.pinned);
    }
}
