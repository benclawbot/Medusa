//! Stable prompt-prefix construction and cache observability.

use std::collections::BTreeMap;

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
        for (sequence, prefix, cached) in [(1, "a", 80), (2, "a", 100), (3, "b", 0)] {
            telemetry
                .append(CacheObservation {
                    sequence,
                    recorded_at: datetime!(2026-07-24 12:00 UTC),
                    provider: "provider".to_owned(),
                    model: "model".to_owned(),
                    prefix_fingerprint: format!("{prefix:0<64}"),
                    prompt_fingerprint: format!("{sequence:0<64}"),
                    stable_prefix_bytes: 100,
                    prompt_bytes: 200,
                    input_tokens: 100,
                    cached_input_tokens: cached,
                    outcome: if cached == 100 {
                        CacheOutcome::Hit
                    } else if cached > 0 {
                        CacheOutcome::PartialHit
                    } else {
                        CacheOutcome::Miss
                    },
                    provider_metadata: BTreeMap::new(),
                })
                .expect("append");
        }
        let summary = telemetry.summary();
        assert_eq!(summary.prefix_changes, 1);
        assert_eq!(summary.reuse_basis_points(), 6_000);
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
