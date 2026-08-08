//! Stable prompt-prefix construction and cache observability.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptSegment {
    pub name: String,
    pub content: String,
    pub stable: bool,
}

impl PromptSegment {
    pub fn new(
        name: impl Into<String>,
        content: impl Into<String>,
        stable: bool,
    ) -> Result<Self, &'static str> {
        let name = name.into();
        let content = content.into();
        if name.trim().is_empty() {
            return Err("prompt segment name cannot be empty");
        }
        if content.trim().is_empty() {
            return Err("prompt segment content cannot be empty");
        }
        Ok(Self {
            name,
            content,
            stable,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptEnvelope {
    pub schema_version: u32,
    pub provider: String,
    pub model: String,
    pub segments: Vec<PromptSegment>,
}

impl PromptEnvelope {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version == 0 {
            return Err("prompt schema version must be greater than zero");
        }
        if self.provider.trim().is_empty() {
            return Err("provider cannot be empty");
        }
        if self.model.trim().is_empty() {
            return Err("model cannot be empty");
        }
        if self.segments.is_empty() {
            return Err("prompt must contain at least one segment");
        }
        let mut seen_dynamic = false;
        let mut names = std::collections::BTreeSet::new();
        for segment in &self.segments {
            if !names.insert(segment.name.as_str()) {
                return Err("prompt segment names must be unique");
            }
            if !segment.stable {
                seen_dynamic = true;
            } else if seen_dynamic {
                return Err("stable prompt segments must form a contiguous prefix");
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn stable_prefix(&self) -> String {
        self.segments
            .iter()
            .take_while(|segment| segment.stable)
            .map(|segment| {
                format!(
                    "<{}>\n{}\n</{}>",
                    segment.name, segment.content, segment.name
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[must_use]
    pub fn rendered(&self) -> String {
        self.segments
            .iter()
            .map(|segment| {
                format!(
                    "<{}>\n{}\n</{}>",
                    segment.name, segment.content, segment.name
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[must_use]
    pub fn stable_prefix_fingerprint(&self) -> String {
        fingerprint(&self.stable_prefix())
    }

    #[must_use]
    pub fn full_prompt_fingerprint(&self) -> String {
        fingerprint(&self.rendered())
    }
}

fn fingerprint(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheOutcome {
    Hit,
    PartialHit,
    Miss,
    Bypassed,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CacheObservation {
    pub sequence: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    pub provider: String,
    pub model: String,
    pub prefix_fingerprint: String,
    pub prompt_fingerprint: String,
    pub stable_prefix_bytes: u64,
    pub prompt_bytes: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub outcome: CacheOutcome,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_metadata: BTreeMap<String, String>,
}

impl CacheObservation {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.sequence == 0 {
            return Err("cache observation sequence must start at one");
        }
        if self.provider.trim().is_empty() || self.model.trim().is_empty() {
            return Err("cache observation provider and model are required");
        }
        if self.prefix_fingerprint.len() != 64 || self.prompt_fingerprint.len() != 64 {
            return Err("cache fingerprints must be sha256 hex strings");
        }
        if self.stable_prefix_bytes > self.prompt_bytes {
            return Err("stable prefix cannot exceed full prompt size");
        }
        if self.cached_input_tokens > self.input_tokens {
            return Err("cached input tokens cannot exceed input tokens");
        }
        Ok(())
    }

    #[must_use]
    pub fn reuse_basis_points(&self) -> u16 {
        if self.input_tokens == 0 {
            return 0;
        }
        ((self.cached_input_tokens.saturating_mul(10_000) / self.input_tokens).min(10_000)) as u16
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CacheTelemetry {
    observations: Vec<CacheObservation>,
}

impl CacheTelemetry {
    pub fn append(&mut self, observation: CacheObservation) -> Result<(), &'static str> {
        observation.validate()?;
        let expected = self
            .observations
            .last()
            .map_or(1, |item| item.sequence.saturating_add(1));
        if observation.sequence != expected {
            return Err("cache observation sequence must be contiguous");
        }
        if self
            .observations
            .last()
            .is_some_and(|item| observation.recorded_at < item.recorded_at)
        {
            return Err("cache observation timestamps must be monotonic");
        }
        self.observations.push(observation);
        Ok(())
    }

    #[must_use]
    pub fn observations(&self) -> &[CacheObservation] {
        &self.observations
    }

    #[must_use]
    pub fn summary(&self) -> CacheSummary {
        let requests = self.observations.len() as u64;
        let hits = self
            .observations
            .iter()
            .filter(|item| item.outcome == CacheOutcome::Hit)
            .count() as u64;
        let partial_hits = self
            .observations
            .iter()
            .filter(|item| item.outcome == CacheOutcome::PartialHit)
            .count() as u64;
        let input_tokens = self.observations.iter().map(|item| item.input_tokens).sum();
        let cached_input_tokens = self
            .observations
            .iter()
            .map(|item| item.cached_input_tokens)
            .sum();
        let prefix_changes = self
            .observations
            .windows(2)
            .filter(|window| window[0].prefix_fingerprint != window[1].prefix_fingerprint)
            .count() as u64;
        CacheSummary {
            requests,
            hits,
            partial_hits,
            input_tokens,
            cached_input_tokens,
            prefix_changes,
        }
    }

    /// Evaluates warm-cache reuse and stable-prefix churn using route-scoped evidence.
    ///
    /// The first observation for each provider/model/prefix tuple is treated as the cold prime and
    /// excluded from the warm reuse ratio. Prefix churn is counted only when the stable prefix
    /// changes on the same provider/model route, so normal route switches cannot create false
    /// churn failures.
    #[must_use]
    pub fn performance_assessment(
        &self,
        policy: CachePerformancePolicy,
    ) -> CachePerformanceAssessment {
        let mut seen_prefixes = BTreeSet::<(String, String, String)>::new();
        let mut last_route_prefix = BTreeMap::<(String, String), String>::new();
        let mut warm_requests = 0_u64;
        let mut warm_input_tokens = 0_u64;
        let mut warm_cached_input_tokens = 0_u64;
        let mut route_prefix_changes = 0_u64;

        for observation in &self.observations {
            let route = (observation.provider.clone(), observation.model.clone());
            if let Some(previous) = last_route_prefix.get(&route)
                && previous != &observation.prefix_fingerprint
            {
                route_prefix_changes = route_prefix_changes.saturating_add(1);
            }
            last_route_prefix.insert(route.clone(), observation.prefix_fingerprint.clone());

            let warm_key = (route.0, route.1, observation.prefix_fingerprint.clone());
            if !seen_prefixes.insert(warm_key) {
                warm_requests = warm_requests.saturating_add(1);
                warm_input_tokens = warm_input_tokens.saturating_add(observation.input_tokens);
                warm_cached_input_tokens =
                    warm_cached_input_tokens.saturating_add(observation.cached_input_tokens);
            }
        }

        let warm_reuse_basis_points = if warm_input_tokens == 0 {
            0
        } else {
            ((warm_cached_input_tokens.saturating_mul(10_000) / warm_input_tokens).min(10_000))
                as u16
        };
        let mut failures = Vec::new();
        if policy.min_warm_reuse_basis_points > 10_000 {
            failures.push("minimum warm reuse cannot exceed 10000 basis points".to_owned());
        }
        if warm_requests < policy.min_warm_requests {
            failures.push(format!(
                "warm cache evidence is insufficient: {warm_requests} requests observed, {} required",
                policy.min_warm_requests
            ));
        }
        if warm_reuse_basis_points < policy.min_warm_reuse_basis_points {
            failures.push(format!(
                "warm cache reuse is {warm_reuse_basis_points} basis points, below required {}",
                policy.min_warm_reuse_basis_points
            ));
        }
        if route_prefix_changes > policy.max_route_prefix_changes {
            failures.push(format!(
                "stable prefix changed {route_prefix_changes} times within a route, above allowed {}",
                policy.max_route_prefix_changes
            ));
        }

        CachePerformanceAssessment {
            warm_requests,
            warm_input_tokens,
            warm_cached_input_tokens,
            warm_reuse_basis_points,
            route_prefix_changes,
            failures,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CacheSummary {
    pub requests: u64,
    pub hits: u64,
    pub partial_hits: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub prefix_changes: u64,
}

impl CacheSummary {
    #[must_use]
    pub fn reuse_basis_points(self) -> u16 {
        if self.input_tokens == 0 {
            return 0;
        }
        ((self.cached_input_tokens.saturating_mul(10_000) / self.input_tokens).min(10_000)) as u16
    }
}

/// Acceptance thresholds for repeated warm prompt-cache measurements.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CachePerformancePolicy {
    /// Minimum number of warm requests required before the gate may pass.
    pub min_warm_requests: u64,
    /// Minimum cached-input reuse ratio, in basis points, across warm requests.
    pub min_warm_reuse_basis_points: u16,
    /// Maximum allowed stable-prefix changes within a single provider/model route.
    pub max_route_prefix_changes: u64,
}

impl Default for CachePerformancePolicy {
    fn default() -> Self {
        Self {
            min_warm_requests: 3,
            min_warm_reuse_basis_points: 9_000,
            max_route_prefix_changes: 0,
        }
    }
}

/// Evidence produced by the warm prompt-cache acceptance gate.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CachePerformanceAssessment {
    pub warm_requests: u64,
    pub warm_input_tokens: u64,
    pub warm_cached_input_tokens: u64,
    pub warm_reuse_basis_points: u16,
    pub route_prefix_changes: u64,
    pub failures: Vec<String>,
}

impl CachePerformanceAssessment {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn envelope(dynamic: &str) -> PromptEnvelope {
        PromptEnvelope {
            schema_version: 1,
            provider: "provider".to_owned(),
            model: "model".to_owned(),
            segments: vec![
                PromptSegment::new("system", "stable system", true).expect("segment"),
                PromptSegment::new("tools", "stable tools", true).expect("segment"),
                PromptSegment::new("task", dynamic, false).expect("segment"),
            ],
        }
    }

    fn observation(
        sequence: u64,
        provider: &str,
        model: &str,
        prefix: char,
        input_tokens: u64,
        cached_input_tokens: u64,
    ) -> CacheObservation {
        CacheObservation {
            sequence,
            recorded_at: datetime!(2026-07-24 12:00 UTC),
            provider: provider.to_owned(),
            model: model.to_owned(),
            prefix_fingerprint: prefix.to_string().repeat(64),
            prompt_fingerprint: format!("{sequence:0<64}"),
            stable_prefix_bytes: 100,
            prompt_bytes: 200,
            input_tokens,
            cached_input_tokens,
            outcome: if cached_input_tokens >= input_tokens && input_tokens > 0 {
                CacheOutcome::Hit
            } else if cached_input_tokens > 0 {
                CacheOutcome::PartialHit
            } else {
                CacheOutcome::Miss
            },
            provider_metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn dynamic_tail_does_not_change_prefix_fingerprint() {
        assert_eq!(
            envelope("task one").stable_prefix_fingerprint(),
            envelope("task two").stable_prefix_fingerprint()
        );
        assert_ne!(
            envelope("task one").full_prompt_fingerprint(),
            envelope("task two").full_prompt_fingerprint()
        );
    }

    #[test]
    fn stable_segment_after_dynamic_is_rejected() {
        let mut value = envelope("task");
        value
            .segments
            .push(PromptSegment::new("late", "stable", true).expect("segment"));
        assert_eq!(
            value.validate(),
            Err("stable prompt segments must form a contiguous prefix")
        );
    }

    #[test]
    fn telemetry_detects_prefix_changes_and_reuse() {
        let mut telemetry = CacheTelemetry::default();
        for (sequence, prefix, cached) in [(1, 'a', 80), (2, 'a', 100), (3, 'b', 0)] {
            telemetry
                .append(observation(
                    sequence, "provider", "model", prefix, 100, cached,
                ))
                .expect("append");
        }
        let summary = telemetry.summary();
        assert_eq!(summary.prefix_changes, 1);
        assert_eq!(summary.reuse_basis_points(), 6_000);
    }

    #[test]
    fn warm_gate_excludes_cold_primes_and_route_switches() {
        let mut telemetry = CacheTelemetry::default();
        for observation in [
            observation(1, "provider-a", "model", 'a', 100, 0),
            observation(2, "provider-a", "model", 'a', 100, 95),
            observation(3, "provider-b", "model", 'a', 100, 0),
            observation(4, "provider-b", "model", 'a', 100, 100),
            observation(5, "provider-a", "model", 'a', 100, 100),
        ] {
            telemetry.append(observation).expect("append");
        }

        let assessment = telemetry.performance_assessment(CachePerformancePolicy::default());
        assert!(assessment.passed(), "{:?}", assessment.failures);
        assert_eq!(assessment.warm_requests, 3);
        assert_eq!(assessment.warm_input_tokens, 300);
        assert_eq!(assessment.warm_cached_input_tokens, 295);
        assert_eq!(assessment.warm_reuse_basis_points, 9_833);
        assert_eq!(assessment.route_prefix_changes, 0);
    }

    #[test]
    fn route_scoped_prefix_churn_fails_the_gate() {
        let mut telemetry = CacheTelemetry::default();
        for observation in [
            observation(1, "provider", "model", 'a', 100, 0),
            observation(2, "provider", "model", 'a', 100, 100),
            observation(3, "provider", "model", 'b', 100, 0),
            observation(4, "provider", "model", 'b', 100, 100),
            observation(5, "provider", "model", 'b', 100, 100),
        ] {
            telemetry.append(observation).expect("append");
        }

        let assessment = telemetry.performance_assessment(CachePerformancePolicy {
            min_warm_requests: 3,
            min_warm_reuse_basis_points: 9_000,
            max_route_prefix_changes: 0,
        });
        assert!(!assessment.passed());
        assert_eq!(assessment.route_prefix_changes, 1);
        assert!(
            assessment
                .failures
                .iter()
                .any(|failure| failure.contains("stable prefix changed"))
        );
    }

    #[test]
    fn insufficient_warm_evidence_fails_closed() {
        let mut telemetry = CacheTelemetry::default();
        telemetry
            .append(observation(1, "provider", "model", 'a', 100, 0))
            .expect("append");
        telemetry
            .append(observation(2, "provider", "model", 'a', 100, 100))
            .expect("append");

        let assessment = telemetry.performance_assessment(CachePerformancePolicy::default());
        assert!(!assessment.passed());
        assert_eq!(assessment.warm_requests, 1);
        assert!(
            assessment
                .failures
                .iter()
                .any(|failure| failure.contains("evidence is insufficient"))
        );
    }

    #[test]
    fn invalid_cached_token_count_is_rejected() {
        let observation = CacheObservation {
            sequence: 1,
            recorded_at: datetime!(2026-07-24 12:00 UTC),
            provider: "provider".to_owned(),
            model: "model".to_owned(),
            prefix_fingerprint: "a".repeat(64),
            prompt_fingerprint: "b".repeat(64),
            stable_prefix_bytes: 10,
            prompt_bytes: 20,
            input_tokens: 10,
            cached_input_tokens: 11,
            outcome: CacheOutcome::Hit,
            provider_metadata: BTreeMap::new(),
        };
        assert_eq!(
            observation.validate(),
            Err("cached input tokens cannot exceed input tokens")
        );
    }
}
