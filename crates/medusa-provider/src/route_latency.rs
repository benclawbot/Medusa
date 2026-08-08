use serde::{Deserialize, Serialize};

use crate::ProviderRouteProfile;

/// Rolling route measurements used to choose the lowest expected verified latency.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RouteLatencyStats {
    pub samples: u64,
    pub successes: u64,
    pub failures: u64,
    pub total_duration_ms: u64,
    pub total_first_token_ms: u64,
    #[serde(default)]
    pub first_token_samples: u64,
    pub cancellation_total_ms: u64,
    pub cancellation_samples: u64,
    pub cached_input_tokens: u64,
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub generation_total_ms: u64,
    #[serde(default)]
    pub retry_attempts: u64,
    #[serde(default)]
    pub retry_recoveries: u64,
}

impl RouteLatencyStats {
    #[must_use]
    pub fn average_duration_ms(self) -> Option<u64> {
        (self.samples > 0).then(|| self.total_duration_ms / self.samples)
    }

    #[must_use]
    pub fn average_first_token_ms(self) -> Option<u64> {
        (self.first_token_samples > 0).then(|| self.total_first_token_ms / self.first_token_samples)
    }

    #[must_use]
    pub fn average_cancellation_ms(self) -> Option<u64> {
        (self.cancellation_samples > 0)
            .then(|| self.cancellation_total_ms / self.cancellation_samples)
    }

    #[must_use]
    pub fn cache_reuse_milli(self) -> u16 {
        if self.input_tokens == 0 {
            return 0;
        }
        let reuse = u128::from(self.cached_input_tokens.min(self.input_tokens)) * 1_000
            / u128::from(self.input_tokens);
        reuse as u16
    }

    #[must_use]
    pub fn success_milli(self) -> u16 {
        let attempts = self.successes.saturating_add(self.failures);
        if attempts == 0 {
            return 1_000;
        }
        ((u128::from(self.successes) * 1_000 / u128::from(attempts)) as u16).min(1_000)
    }

    /// Observed output throughput in milli-tokens per second.
    #[must_use]
    pub fn output_tokens_per_second_milli(self) -> Option<u64> {
        (self.generation_total_ms > 0 && self.output_tokens > 0).then(|| {
            let scaled = u128::from(self.output_tokens).saturating_mul(1_000_000)
                / u128::from(self.generation_total_ms);
            scaled.min(u128::from(u64::MAX)) as u64
        })
    }

    /// Share of retry attempts that eventually recovered on the same route.
    #[must_use]
    pub fn retry_recovery_milli(self) -> u16 {
        if self.retry_attempts == 0 {
            return 1_000;
        }
        ((u128::from(self.retry_recoveries.min(self.retry_attempts)) * 1_000
            / u128::from(self.retry_attempts)) as u16)
            .min(1_000)
    }
}

/// Deterministic score inputs for one provider route.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RouteLatencyPolicy {
    /// Cold routes receive this conservative duration until real measurements exist.
    pub cold_start_duration_ms: u64,
    /// Failure penalty added per missing success-rate permille.
    pub failure_penalty_ms_per_mille: u64,
    /// Cache reuse reduces expected latency by at most this many milliseconds.
    pub max_cache_credit_ms: u64,
}

impl RouteLatencyPolicy {
    #[must_use]
    pub const fn production_default() -> Self {
        Self {
            cold_start_duration_ms: 30_000,
            failure_penalty_ms_per_mille: 10,
            max_cache_credit_ms: 2_000,
        }
    }
}

impl Default for RouteLatencyPolicy {
    fn default() -> Self {
        Self::production_default()
    }
}

/// Returns route indices ordered by expected verified completion latency.
///
/// Capability-incompatible routes are excluded before scoring. Ties retain configured route order.
#[must_use]
pub fn latency_aware_route_order(
    profiles: &[ProviderRouteProfile],
    stats: &[RouteLatencyStats],
    require_tools: bool,
    require_streaming: bool,
    policy: RouteLatencyPolicy,
) -> Vec<usize> {
    let mut candidates = profiles
        .iter()
        .enumerate()
        .filter(|(_, profile)| !require_tools || profile.tool_calling)
        .filter(|(_, profile)| !require_streaming || profile.streaming)
        .map(|(index, _)| {
            let stats = stats.get(index).copied().unwrap_or_default();
            (
                index,
                expected_latency_ms(stats, policy),
                stats.output_tokens_per_second_milli().unwrap_or_default(),
                stats.retry_recovery_milli(),
            )
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(index, score, throughput, recovery)| {
        (
            *score,
            std::cmp::Reverse(*throughput),
            std::cmp::Reverse(*recovery),
            *index,
        )
    });
    candidates
        .into_iter()
        .map(|(index, _, _, _)| index)
        .collect()
}

#[must_use]
pub fn expected_latency_ms(stats: RouteLatencyStats, policy: RouteLatencyPolicy) -> u64 {
    let base = stats
        .average_duration_ms()
        .unwrap_or(policy.cold_start_duration_ms);
    let failure_penalty = u64::from(1_000_u16.saturating_sub(stats.success_milli()))
        .saturating_mul(policy.failure_penalty_ms_per_mille);
    let cache_credit = policy
        .max_cache_credit_ms
        .saturating_mul(u64::from(stats.cache_reuse_milli()))
        / 1_000;
    base.saturating_add(failure_penalty)
        .saturating_sub(cache_credit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RouteRetryPolicy;

    fn profile(id: &str, tool_calling: bool, streaming: bool) -> ProviderRouteProfile {
        ProviderRouteProfile {
            id: id.to_owned(),
            provider: id.to_owned(),
            model: "model".to_owned(),
            protocol: "test".to_owned(),
            endpoint: None,
            auth_source: "test".to_owned(),
            tool_calling,
            streaming,
            retry: RouteRetryPolicy::default(),
        }
    }

    #[test]
    fn lower_expected_latency_is_preferred_over_configured_order() {
        let profiles = vec![profile("slow", true, true), profile("fast", true, true)];
        let stats = vec![
            RouteLatencyStats {
                samples: 10,
                successes: 10,
                total_duration_ms: 20_000,
                ..RouteLatencyStats::default()
            },
            RouteLatencyStats {
                samples: 10,
                successes: 10,
                total_duration_ms: 5_000,
                ..RouteLatencyStats::default()
            },
        ];
        assert_eq!(
            latency_aware_route_order(&profiles, &stats, true, true, RouteLatencyPolicy::default()),
            vec![1, 0]
        );
    }

    #[test]
    fn capability_incompatible_routes_are_excluded() {
        let profiles = vec![
            profile("no-tools", false, true),
            profile("no-stream", true, false),
            profile("eligible", true, true),
        ];
        assert_eq!(
            latency_aware_route_order(&profiles, &[], true, true, RouteLatencyPolicy::default()),
            vec![2]
        );
    }

    #[test]
    fn repeated_failure_can_make_nominally_fast_route_slower() {
        let policy = RouteLatencyPolicy {
            failure_penalty_ms_per_mille: 10,
            ..RouteLatencyPolicy::default()
        };
        let unreliable = RouteLatencyStats {
            samples: 10,
            successes: 5,
            failures: 5,
            total_duration_ms: 1_000,
            ..RouteLatencyStats::default()
        };
        let reliable = RouteLatencyStats {
            samples: 10,
            successes: 10,
            total_duration_ms: 20_000,
            ..RouteLatencyStats::default()
        };
        assert!(expected_latency_ms(unreliable, policy) > expected_latency_ms(reliable, policy));
    }

    #[test]
    fn stable_cache_reuse_receives_bounded_latency_credit() {
        let policy = RouteLatencyPolicy::default();
        let uncached = RouteLatencyStats {
            samples: 1,
            successes: 1,
            total_duration_ms: 5_000,
            input_tokens: 1_000,
            ..RouteLatencyStats::default()
        };
        let cached = RouteLatencyStats {
            cached_input_tokens: 950,
            ..uncached
        };
        assert!(expected_latency_ms(cached, policy) < expected_latency_ms(uncached, policy));
        assert_eq!(cached.cache_reuse_milli(), 950);
    }

    #[test]
    fn cold_route_ties_preserve_configured_order() {
        let profiles = vec![profile("a", true, false), profile("b", true, false)];
        assert_eq!(
            latency_aware_route_order(&profiles, &[], true, false, RouteLatencyPolicy::default()),
            vec![0, 1]
        );
    }

    #[test]
    fn equal_latency_prefers_throughput_then_retry_recovery() {
        let profiles = vec![
            profile("slow-output", true, true),
            profile("fast-output", true, true),
            profile("recovered", true, true),
        ];
        let base = RouteLatencyStats {
            samples: 10,
            successes: 10,
            total_duration_ms: 10_000,
            output_tokens: 1_000,
            generation_total_ms: 10_000,
            retry_attempts: 10,
            retry_recoveries: 5,
            ..RouteLatencyStats::default()
        };
        let stats = vec![
            base,
            RouteLatencyStats {
                output_tokens: 2_000,
                ..base
            },
            RouteLatencyStats {
                retry_recoveries: 10,
                ..base
            },
        ];
        assert_eq!(
            latency_aware_route_order(&profiles, &stats, true, true, RouteLatencyPolicy::default()),
            vec![1, 2, 0]
        );
        assert_eq!(stats[1].output_tokens_per_second_milli(), Some(200_000));
        assert_eq!(stats[2].retry_recovery_milli(), 1_000);
    }
}
