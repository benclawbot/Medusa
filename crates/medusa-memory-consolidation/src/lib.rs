//! Deterministic memory consolidation with explicit conflict handling.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Preference,
    Decision,
    Constraint,
    Fact,
    Procedure,
    FailureLesson,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MemoryObservation {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    pub value: String,
    pub kind: MemoryKind,
    pub source: String,
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    pub confidence_basis_points: u16,
}

impl MemoryObservation {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.id.trim().is_empty()
            || self.subject.trim().is_empty()
            || self.predicate.trim().is_empty()
        {
            return Err("observation identity fields cannot be empty");
        }
        if self.value.trim().is_empty() || self.source.trim().is_empty() {
            return Err("observation value and source cannot be empty");
        }
        if self.confidence_basis_points > 10_000 {
            return Err("observation confidence cannot exceed 10000 basis points");
        }
        Ok(())
    }

    #[must_use]
    pub fn key(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{:?}",
            normalize(&self.subject),
            normalize(&self.predicate),
            self.kind
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConsolidationPolicy {
    pub minimum_support: usize,
    pub minimum_confidence_basis_points: u16,
    pub conflict_margin_basis_points: u16,
}

impl Default for ConsolidationPolicy {
    fn default() -> Self {
        Self {
            minimum_support: 2,
            minimum_confidence_basis_points: 6_000,
            conflict_margin_basis_points: 1_000,
        }
    }
}

impl ConsolidationPolicy {
    pub fn validate(self) -> Result<Self, &'static str> {
        if self.minimum_support == 0 {
            return Err("minimum_support must be greater than zero");
        }
        if self.minimum_confidence_basis_points > 10_000
            || self.conflict_margin_basis_points > 10_000
        {
            return Err("policy basis points cannot exceed 10000");
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConsolidatedMemory {
    pub key: String,
    pub subject: String,
    pub predicate: String,
    pub value: String,
    pub kind: MemoryKind,
    pub support_ids: Vec<String>,
    pub confidence_basis_points: u16,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MemoryConflict {
    pub key: String,
    pub candidate_values: Vec<String>,
    pub supporting_observation_ids: Vec<String>,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConsolidationResult {
    pub memories: Vec<ConsolidatedMemory>,
    pub conflicts: Vec<MemoryConflict>,
    pub deferred_observation_ids: Vec<String>,
    pub source_fingerprint: String,
    pub result_fingerprint: String,
}

pub fn consolidate(
    observations: &[MemoryObservation],
    policy: ConsolidationPolicy,
) -> Result<ConsolidationResult, &'static str> {
    let policy = policy.validate()?;
    if observations.is_empty() {
        return Err("at least one observation is required");
    }

    let mut ids = BTreeSet::new();
    for observation in observations {
        observation.validate()?;
        if !ids.insert(observation.id.as_str()) {
            return Err("observation ids must be unique");
        }
    }

    let mut ordered = observations.to_vec();
    ordered.sort_by(|left, right| {
        left.key()
            .cmp(&right.key())
            .then_with(|| normalize(&left.value).cmp(&normalize(&right.value)))
            .then_with(|| left.observed_at.cmp(&right.observed_at))
            .then_with(|| left.id.cmp(&right.id))
    });

    let source_fingerprint = fingerprint(&ordered)?;
    let mut groups: BTreeMap<String, Vec<MemoryObservation>> = BTreeMap::new();
    for observation in ordered {
        groups
            .entry(observation.key())
            .or_default()
            .push(observation);
    }

    let mut memories = Vec::new();
    let mut conflicts = Vec::new();
    let mut deferred = Vec::new();

    for (key, group) in groups {
        let mut candidates: BTreeMap<String, Vec<&MemoryObservation>> = BTreeMap::new();
        for observation in &group {
            candidates
                .entry(normalize(&observation.value))
                .or_default()
                .push(observation);
        }

        let mut ranked: Vec<(String, Vec<&MemoryObservation>, u32)> = candidates
            .into_iter()
            .map(|(value, support)| {
                let score = support
                    .iter()
                    .map(|item| u32::from(item.confidence_basis_points))
                    .sum();
                (value, support, score)
            })
            .collect();
        ranked.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| left.0.cmp(&right.0)));

        let Some(winner) = ranked.first() else {
            return Err("memory candidate group cannot be empty");
        };
        let runner_up_score = ranked.get(1).map_or(0, |candidate| candidate.2);
        let support_count = winner.1.len();
        let support_count_u32 =
            u32::try_from(support_count).map_err(|_| "support count overflow")?;
        let mean_confidence = (winner.2 / support_count_u32).min(10_000) as u16;
        let margin = winner.2.saturating_sub(runner_up_score);

        if ranked.len() > 1 && margin < u32::from(policy.conflict_margin_basis_points) {
            let mut values = ranked
                .iter()
                .map(|entry| entry.0.clone())
                .collect::<Vec<_>>();
            values.sort();
            let mut supporting_ids = group.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
            supporting_ids.sort();
            let conflict_fingerprint = fingerprint(&(key.as_str(), &values, &supporting_ids))?;
            conflicts.push(MemoryConflict {
                key,
                candidate_values: values,
                supporting_observation_ids: supporting_ids,
                fingerprint: conflict_fingerprint,
            });
            continue;
        }

        if support_count < policy.minimum_support
            || mean_confidence < policy.minimum_confidence_basis_points
        {
            deferred.extend(group.iter().map(|item| item.id.clone()));
            continue;
        }

        let Some(exemplar) = winner.1.first().copied() else {
            return Err("winning memory candidate has no supporting observation");
        };
        let mut support_ids = winner
            .1
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        support_ids.sort();
        let memory_fingerprint = fingerprint(&(
            key.as_str(),
            exemplar.subject.as_str(),
            exemplar.predicate.as_str(),
            winner.0.as_str(),
            exemplar.kind,
            &support_ids,
            mean_confidence,
        ))?;
        memories.push(ConsolidatedMemory {
            key,
            subject: exemplar.subject.clone(),
            predicate: exemplar.predicate.clone(),
            value: exemplar.value.clone(),
            kind: exemplar.kind,
            support_ids,
            confidence_basis_points: mean_confidence,
            fingerprint: memory_fingerprint,
        });
    }

    deferred.sort();
    memories.sort_by(|left, right| left.key.cmp(&right.key));
    conflicts.sort_by(|left, right| left.key.cmp(&right.key));
    let result_fingerprint = fingerprint(&(&memories, &conflicts, &deferred))?;

    Ok(ConsolidationResult {
        memories,
        conflicts,
        deferred_observation_ids: deferred,
        source_fingerprint,
        result_fingerprint,
    })
}

fn normalize(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn fingerprint<T: Serialize>(value: &T) -> Result<String, &'static str> {
    let bytes = serde_json::to_vec(value).map_err(|_| "memory state serialization failed")?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn observation(id: &str, value: &str, confidence: u16) -> MemoryObservation {
        MemoryObservation {
            id: id.into(),
            subject: "runtime".into(),
            predicate: "preferred_shell".into(),
            value: value.into(),
            kind: MemoryKind::Preference,
            source: "session.md".into(),
            observed_at: datetime!(2026-07-24 15:00 UTC),
            confidence_basis_points: confidence,
        }
    }

    #[test]
    fn repeated_evidence_consolidates_deterministically() {
        let input = vec![
            observation("b", "bash", 8_000),
            observation("a", "bash", 9_000),
        ];
        let first = consolidate(&input, ConsolidationPolicy::default()).unwrap();
        let second = consolidate(&input, ConsolidationPolicy::default()).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.memories.len(), 1);
        assert_eq!(first.memories[0].support_ids, vec!["a", "b"]);
    }

    #[test]
    fn close_competing_values_create_conflict() {
        let input = vec![
            observation("a", "bash", 8_000),
            observation("b", "zsh", 7_500),
        ];
        let result = consolidate(
            &input,
            ConsolidationPolicy {
                minimum_support: 1,
                ..ConsolidationPolicy::default()
            },
        )
        .unwrap();
        assert!(result.memories.is_empty());
        assert_eq!(result.conflicts.len(), 1);
    }

    #[test]
    fn weak_single_observation_is_deferred() {
        let result = consolidate(
            &[observation("a", "bash", 4_000)],
            ConsolidationPolicy::default(),
        )
        .unwrap();
        assert_eq!(result.deferred_observation_ids, vec!["a"]);
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let input = vec![
            observation("same", "bash", 8_000),
            observation("same", "bash", 8_000),
        ];
        assert!(consolidate(&input, ConsolidationPolicy::default()).is_err());
    }

    #[test]
    fn empty_input_is_rejected() {
        assert!(consolidate(&[], ConsolidationPolicy::default()).is_err());
    }
}
