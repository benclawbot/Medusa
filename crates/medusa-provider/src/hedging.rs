use serde::{Deserialize, Serialize};

use crate::{ProviderRouteProfile, RouteLatencyPolicy, RouteLatencyStats, expected_latency_ms};

/// Explicit, bounded policy for deciding whether a secondary provider request may be started.
///
/// This type is deliberately deterministic and side-effect free. Production racing consumes the
/// decision; callers can disable hedging globally or cap duplicate-generation exposure by output
/// budget before any second request is launched.
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
}

impl HedgePolicy {
    #[must_use]
    pub const fn production_default() -> Self {
        Self {
            enabled: true,
            min_primary_samples: 8,
            delay_multiplier_milli: 1_500,
            max_delay_ms: 8_000,
            max_duplicate_output_tokens: 8_192,
        }
    }
}

impl Default for HedgePolicy {
    fn default() -> Self {
        Self::production_default()
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
}

/// Selects at most one compatible secondary route for tail-latency recovery.
///
/// The decision is intentionally conservative:
/// - requires explicit policy enablement and enough primary telemetry;
/// - refuses requests whose duplicate output budget exceeds the configured waste cap;
/// - requires two capability-compatible routes;
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
    let secondary_index = route_order[1];
    let primary_profile = profiles.get(primary_index)?;
    let secondary_profile = profiles.get(secondary_index)?;
    if primary_profile.tool_calling != secondary_profile.tool_calling {
        return None;
    }

    let primary_stats = stats.get(primary_index).copied().unwrap_or_default();
    if primary_stats.samples < policy.min_primary_samples {
        return None;
    }
    let primary_observed_ms = primary_stats.average_duration_ms()?;
    let launch_after_ms =
        primary_observed_ms.saturating_mul(u64::from(policy.delay_multiplier_milli)) / 1_000;
    let launch_after_ms = launch_after_ms.min(policy.max_delay_ms).max(1);
    let primary_expected_ms = expected_latency_ms(primary_stats, latency_policy);

    let secondary_stats = stats.get(secondary_index).copied().unwrap_or_default();
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
            HedgePolicy::default(),
            RouteLatencyPolicy::default(),
        )
        .expect("hedge decision");
        assert_eq!(decision.primary_index, 0);
        assert_eq!(decision.secondary_index, 1);
        assert_eq!(decision.launch_after_ms, 1_500);
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
                HedgePolicy::default(),
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
                HedgePolicy::default(),
                RouteLatencyPolicy::default(),
            )
            .is_none()
        );
        assert!(
            hedge_decision(
                &[0, 1],
                &profiles,
                &[stats(4_000, 10), stats(100, 10)],
                HedgePolicy::default().max_duplicate_output_tokens + 1,
                HedgePolicy::default(),
                RouteLatencyPolicy::default(),
            )
            .is_none()
        );
    }

    #[test]
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
                    ..HedgePolicy::default()
                },
                RouteLatencyPolicy::default(),
            )
            .is_none()
        );
    }
}
