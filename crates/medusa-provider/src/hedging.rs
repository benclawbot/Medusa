use std::env;

use serde::{Deserialize, Serialize};

use crate::{ProviderRouteProfile, RouteLatencyPolicy, RouteLatencyStats, expected_latency_ms};

const HEDGE_ENABLED_ENV: &str = "MEDUSA_PROVIDER_HEDGE_ENABLED";
const HEDGE_MAX_DUPLICATE_OUTPUT_TOKENS_ENV: &str =
    "MEDUSA_PROVIDER_HEDGE_MAX_DUPLICATE_OUTPUT_TOKENS";
const HEDGE_MAX_DUPLICATE_COST_MICROUSD_ENV: &str =
    "MEDUSA_PROVIDER_HEDGE_MAX_DUPLICATE_COST_MICROUSD";

/// Explicit, bounded policy for deciding whether a secondary provider request may be started.
///
/// This type is deliberately deterministic and side-effect free. Production racing consumes the
/// decision; callers can disable hedging globally or cap duplicate-generation exposure by output
/// and authoritative monetary cost budgets before any second request is launched.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HedgePolicy {
    /// Master operator gate. Disabled policies never launch duplicate generation.
    pub enabled: bool,
    /// Minimum successful/failed latency samples required for the primary route.
    pub min_primary_samples: u64,
    /// Launch threshold as a multiplier of the primary route's observed mean duration.
    pub delay_multiplier_milli: u16,
    /// Absolute upper bound for the learned launch delay.
    pub max_delay_ms: u64,
    /// Requests above this output-token budget are ineligible for hedging.
    pub max_duplicate_output_tokens: u32,
    /// Optional authoritative monetary cap for the duplicate route, in millionths of a US dollar.
    ///
    /// `None` preserves existing behavior when no billing/pricing authority has configured a
    /// budget. When a cap is configured, the secondary route must have authoritative cost
    /// observations and its observed average cost must fit within the cap.
    pub max_duplicate_cost_microusd: Option<u64>,
}

impl HedgePolicy {
    #[must_use]
    pub const fn production_default() -> Self {
        Self {
            enabled: false,
            min_primary_samples: 8,
            delay_multiplier_milli: 1_500,
            max_delay_ms: 8_000,
            max_duplicate_output_tokens: 8_192,
            max_duplicate_cost_microusd: None,
        }
    }

    /// Applies production operator gates from the process environment.
    ///
    /// `MEDUSA_PROVIDER_HEDGE_ENABLED` accepts `1`, `true`, `yes`, or `on` (case-insensitive).
    /// Any other explicitly supplied value disables hedging fail-closed. The duplicate-output cap
    /// can be overridden with `MEDUSA_PROVIDER_HEDGE_MAX_DUPLICATE_OUTPUT_TOKENS`; an invalid
    /// explicit value becomes zero, also failing closed for non-zero-output requests. When
    /// `MEDUSA_PROVIDER_HEDGE_MAX_DUPLICATE_COST_MICROUSD` is present, it enables the authoritative
    /// monetary gate; an invalid explicit value becomes zero and therefore fails closed unless the
    /// observed duplicate route cost is exactly zero.
    #[must_use]
    pub fn from_environment() -> Self {
        let mut policy = Self::production_default();
        if let Ok(value) = env::var(HEDGE_ENABLED_ENV) {
            policy.enabled = matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            );
        }
        if let Ok(value) = env::var(HEDGE_MAX_DUPLICATE_OUTPUT_TOKENS_ENV) {
            policy.max_duplicate_output_tokens = value.trim().parse::<u32>().unwrap_or(0);
        }
        if let Ok(value) = env::var(HEDGE_MAX_DUPLICATE_COST_MICROUSD_ENV) {
            policy.max_duplicate_cost_microusd = Some(value.trim().parse::<u64>().unwrap_or(0));
        }
        policy
    }
}

impl Default for HedgePolicy {
    fn default() -> Self {
        Self::from_environment()
    }
}

/// One bounded secondary-request decision derived from existing route telemetry.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HedgeDecision {
    pub primary_index: usize,
    pub secondary_index: usize,
    pub launch_after_ms: u64,
    pub primary_expected_ms: u64,
    pub secondary_expected_ms: u64,
    /// Authoritative observed average cost for the duplicate route, when available.
    pub secondary_cost_microusd: Option<u64>,
}

/// Selects at most one compatible secondary route for tail-latency recovery.
///
/// The decision is intentionally conservative:
/// - requires explicit policy enablement and enough primary telemetry;
/// - refuses requests whose duplicate output budget exceeds the configured waste cap;
/// - when a monetary cap is configured, requires authoritative secondary-route cost evidence and
///   refuses duplicates whose observed average cost exceeds that cap;
/// - scans latency-ranked fallbacks and selects the first route satisfying capability, cost, and
///   tail-recovery constraints;
/// - waits until the latency-ranked primary breaches a learned tail threshold;
/// - launches only when the secondary is expected to complete within one learned tail-recovery
///   window from launch.
///
/// The secondary is intentionally *not* required to have a lower unconditional latency score than
/// the primary. `latency_aware_route_order` already places the lowest-score route first, so such a
/// requirement would make hedging unreachable exactly when the selected primary becomes a tail
/// outlier on the current request.
#[must_use]
pub fn hedge_decision(
    route_order: &[usize],
    profiles: &[ProviderRouteProfile],
    stats: &[RouteLatencyStats],
    request_max_tokens: u32,
    policy: HedgePolicy,
    latency_policy: RouteLatencyPolicy,
) -> Option<HedgeDecision> {
    if !policy.enabled
        || policy.delay_multiplier_milli < 1_000
        || policy.max_delay_ms == 0
        || request_max_tokens > policy.max_duplicate_output_tokens
        || route_order.len() < 2
    {
        return None;
    }

    let primary_index = route_order[0];
    let primary_profile = profiles.get(primary_index)?;
    let primary_stats = stats.get(primary_index).copied().unwrap_or_default();
    if primary_stats.samples < policy.min_primary_samples {
        return None;
    }
    let primary_observed_ms = primary_stats.average_duration_ms()?;
    let launch_after_ms =
        primary_observed_ms.saturating_mul(u64::from(policy.delay_multiplier_milli)) / 1_000;
    let launch_after_ms = launch_after_ms.min(policy.max_delay_ms).max(1);
    let primary_expected_ms = expected_latency_ms(primary_stats, latency_policy);

    route_order
        .iter()
        .copied()
        .skip(1)
        .find_map(|secondary_index| {
            let secondary_profile = profiles.get(secondary_index)?;
            if primary_profile.tool_calling != secondary_profile.tool_calling {
                return None;
            }
            let secondary_stats = stats.get(secondary_index).copied().unwrap_or_default();
            let secondary_cost_microusd = secondary_stats.average_cost_microusd();
            if let Some(max_cost) = policy.max_duplicate_cost_microusd
                && secondary_cost_microusd.is_none_or(|cost| cost > max_cost)
            {
                return None;
            }
            let secondary_expected_ms = expected_latency_ms(secondary_stats, latency_policy);
            if secondary_expected_ms >= launch_after_ms {
                return None;
            }
            Some(HedgeDecision {
                primary_index,
                secondary_index,
                launch_after_ms,
                primary_expected_ms,
                secondary_expected_ms,
                secondary_cost_microusd,
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RouteRetryPolicy, latency_aware_route_order};

    fn profile(id: &str) -> ProviderRouteProfile {
        ProviderRouteProfile {
            id: id.to_owned(),
            provider: id.to_owned(),
            model: "model".to_owned(),
            protocol: "openai".to_owned(),
            endpoint: None,
            auth_source: "test".to_owned(),
            tool_calling: true,
            streaming: true,
            retry: RouteRetryPolicy::default(),
        }
    }

    fn stats(duration_ms: u64, samples: u64) -> RouteLatencyStats {
        RouteLatencyStats {
            samples,
            successes: samples,
            total_duration_ms: duration_ms.saturating_mul(samples),
            ..RouteLatencyStats::default()
        }
    }

    #[test]
    fn latency_ranked_primary_can_trigger_exactly_one_secondary() {
        let profiles = vec![profile("primary"), profile("secondary"), profile("third")];
        let telemetry = vec![stats(1_000, 10), stats(1_200, 10), stats(2_000, 10)];
        let route_order = latency_aware_route_order(
            &profiles,
            &telemetry,
            false,
            false,
            RouteLatencyPolicy::default(),
        );
        assert_eq!(route_order, vec![0, 1, 2]);

        let decision = hedge_decision(
            &route_order,
            &profiles,
            &telemetry,
            1_024,
            HedgePolicy {
                enabled: true,
                ..HedgePolicy::production_default()
            },
            RouteLatencyPolicy::default(),
        )
        .expect("hedge decision");
        assert_eq!(decision.primary_index, 0);
        assert_eq!(decision.secondary_index, 1);
        assert_eq!(decision.launch_after_ms, 1_500);
        assert_eq!(decision.secondary_cost_microusd, None);
    }

    #[test]
    fn secondary_outside_tail_recovery_window_is_not_launched() {
        let profiles = vec![profile("primary"), profile("secondary")];
        let telemetry = vec![stats(1_000, 10), stats(2_000, 10)];
        let route_order = latency_aware_route_order(
            &profiles,
            &telemetry,
            false,
            false,
            RouteLatencyPolicy::default(),
        );
        assert_eq!(route_order, vec![0, 1]);
        assert!(
            hedge_decision(
                &route_order,
                &profiles,
                &telemetry,
                1_024,
                HedgePolicy::production_default(),
                RouteLatencyPolicy::default(),
            )
            .is_none()
        );
    }

    #[test]
    fn cold_primary_and_large_outputs_fail_closed() {
        let profiles = vec![profile("primary"), profile("secondary")];
        let telemetry = vec![stats(4_000, 2), stats(100, 10)];
        assert!(
            hedge_decision(
                &[0, 1],
                &profiles,
                &telemetry,
                1_024,
                HedgePolicy::production_default(),
                RouteLatencyPolicy::default(),
            )
            .is_none()
        );
        let policy = HedgePolicy::production_default();
        assert!(
            hedge_decision(
                &[0, 1],
                &profiles,
                &[stats(4_000, 10), stats(100, 10)],
                policy.max_duplicate_output_tokens + 1,
                policy,
                RouteLatencyPolicy::default(),
            )
            .is_none()
        );
    }

    #[test]
    fn configured_cost_budget_requires_authoritative_secondary_cost() {
        let profiles = vec![profile("primary"), profile("secondary")];
        let telemetry = vec![stats(4_000, 10), stats(100, 10)];
        let policy = HedgePolicy {
            enabled: true,
            max_duplicate_cost_microusd: Some(500),
            ..HedgePolicy::production_default()
        };
        assert!(
            hedge_decision(
                &[0, 1],
                &profiles,
                &telemetry,
                1_024,
                policy,
                RouteLatencyPolicy::default(),
            )
            .is_none()
        );
    }

    #[test]
    fn configured_cost_budget_blocks_expensive_secondary_and_allows_bounded_cost() {
        let profiles = vec![profile("primary"), profile("secondary")];
        let primary = stats(4_000, 10);
        let secondary = RouteLatencyStats {
            cost_microusd_total: 6_000,
            cost_samples: 10,
            ..stats(100, 10)
        };
        let policy = HedgePolicy {
            enabled: true,
            max_duplicate_cost_microusd: Some(500),
            ..HedgePolicy::production_default()
        };
        assert!(
            hedge_decision(
                &[0, 1],
                &profiles,
                &[primary, secondary],
                1_024,
                policy,
                RouteLatencyPolicy::default(),
            )
            .is_none()
        );

        let bounded_secondary = RouteLatencyStats {
            cost_microusd_total: 4_000,
            ..secondary
        };
        let decision = hedge_decision(
            &[0, 1],
            &profiles,
            &[primary, bounded_secondary],
            1_024,
            policy,
            RouteLatencyPolicy::default(),
        )
        .expect("bounded authoritative cost permits hedge");
        assert_eq!(decision.secondary_cost_microusd, Some(400));
    }

    #[test]
    fn over_budget_secondary_does_not_hide_later_eligible_route() {
        let profiles = vec![profile("primary"), profile("expensive"), profile("bounded")];
        let primary = stats(4_000, 10);
        let expensive = RouteLatencyStats {
            cost_microusd_total: 6_000,
            cost_samples: 10,
            ..stats(100, 10)
        };
        let bounded = RouteLatencyStats {
            cost_microusd_total: 4_000,
            cost_samples: 10,
            ..stats(200, 10)
        };
        let policy = HedgePolicy {
            enabled: true,
            max_duplicate_cost_microusd: Some(500),
            ..HedgePolicy::production_default()
        };
        let decision = hedge_decision(
            &[0, 1, 2],
            &profiles,
            &[primary, expensive, bounded],
            1_024,
            policy,
            RouteLatencyPolicy::default(),
        )
        .expect("later bounded route is eligible");
        assert_eq!(decision.secondary_index, 2);
        assert_eq!(decision.secondary_cost_microusd, Some(400));
    }

    #[test]
    #[test]
    fn production_default_disables_duplicate_generation() {
        assert!(!HedgePolicy::production_default().enabled);
        let profiles = vec![profile("primary"), profile("secondary")];
        assert!(
            hedge_decision(
                &[0, 1],
                &profiles,
                &[stats(4_000, 10), stats(100, 10)],
                1_024,
                HedgePolicy::production_default(),
                RouteLatencyPolicy::default(),
            )
            .is_none()
        );
    }

    fn operator_can_disable_hedging() {
        let profiles = vec![profile("primary"), profile("secondary")];
        assert!(
            hedge_decision(
                &[0, 1],
                &profiles,
                &[stats(4_000, 10), stats(100, 10)],
                1_024,
                HedgePolicy {
                    enabled: false,
                    ..HedgePolicy::production_default()
                },
                RouteLatencyPolicy::default(),
            )
            .is_none()
        );
    }
}
