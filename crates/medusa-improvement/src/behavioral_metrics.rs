//! Rebuildable task-aware cohort and drift projections over canonical outcomes.
//!
//! This module contains measurement only. It does not admit routes, activate refinements, or
//! decide correctness; those authorities remain in the runtime, verifier, and learning stores.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    behavioral_outcome::{
        BehavioralComplexityBand, BehavioralOutcomeV1, BehavioralRiskClass, BehavioralTaskIntent,
        BehavioralTerminalStatus, BehavioralWorkspaceMode,
    },
    encode,
};

pub const BEHAVIORAL_METRICS_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct BehavioralCohortKey {
    pub classification_version: u16,
    pub workspace_mode: BehavioralWorkspaceMode,
    pub task_intent: BehavioralTaskIntent,
    pub language_families: Vec<String>,
    pub risk_class: BehavioralRiskClass,
    pub complexity_band: BehavioralComplexityBand,
    pub model: String,
    pub provider: String,
    pub route_fingerprint: String,
    pub harness_version: String,
    pub tool_fingerprint: String,
    pub repository_revision_family: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BehavioralCohortReport {
    pub schema_version: u16,
    pub key: BehavioralCohortKey,
    pub sample_count: usize,
    pub verified_successes: usize,
    pub verified_failures: usize,
    pub cancelled: usize,
    pub partial: usize,
    pub inconclusive: usize,
    pub first_pass_verified_successes: usize,
    pub verification_attempts: u64,
    pub recovery_count: u64,
    pub tool_failure_count: u64,
    pub verified_success_rate_milli: Option<u16>,
    pub first_pass_success_rate_milli: Option<u16>,
    pub median_latency_millis: Option<u64>,
    pub p95_latency_millis: Option<u64>,
    pub tokens_per_verified_success: Option<u64>,
    pub cost_per_verified_success_microunits: Option<u64>,
    pub latency_coverage_milli: u16,
    pub token_coverage_milli: u16,
    pub cost_coverage_milli: u16,
    pub uncertainty_milli: u16,
    pub outcome_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftClassification {
    Improvement,
    Regression,
    NoMaterialChange,
    InsufficientEvidence,
    Confounded,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DriftPolicy {
    pub minimum_samples: usize,
    pub minimum_effect_milli: u16,
    pub maximum_uncertainty_milli: u16,
}

impl Default for DriftPolicy {
    fn default() -> Self {
        Self {
            minimum_samples: 3,
            minimum_effect_milli: 50,
            maximum_uncertainty_milli: 500,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BehavioralDriftReport {
    pub schema_version: u16,
    pub key: BehavioralCohortKey,
    pub baseline_samples: usize,
    pub current_samples: usize,
    pub baseline_success_rate_milli: Option<u16>,
    pub current_success_rate_milli: Option<u16>,
    pub delta_success_rate_milli: Option<i16>,
    pub combined_uncertainty_milli: u16,
    pub classification: DriftClassification,
    pub correlational_only: bool,
    pub baseline_outcome_ids: Vec<String>,
    pub current_outcome_ids: Vec<String>,
}

#[must_use]
pub fn cohort_key(outcome: &BehavioralOutcomeV1) -> BehavioralCohortKey {
    let execution = outcome
        .contributing_execution()
        .or_else(|| outcome.model_executions.last());
    let mut tools = outcome
        .tool_executions
        .iter()
        .map(|tool| tool.tool.as_str())
        .collect::<Vec<_>>();
    tools.sort_unstable();
    tools.dedup();
    let tool_fingerprint = digest(tools.join("\n").as_bytes());
    BehavioralCohortKey {
        classification_version: outcome.task_classification.schema_version,
        workspace_mode: outcome.task_classification.workspace_mode,
        task_intent: outcome.task_classification.intent,
        language_families: outcome.task_classification.language_families.clone(),
        risk_class: outcome.task_classification.risk_class,
        complexity_band: outcome.task_classification.complexity_band,
        model: execution
            .map(|value| value.model.clone())
            .unwrap_or_else(|| "unknown-model".to_owned()),
        provider: execution
            .map(|value| value.provider.clone())
            .unwrap_or_else(|| "unknown-provider".to_owned()),
        route_fingerprint: execution
            .and_then(|value| value.request_fingerprint.clone())
            .unwrap_or_else(|| "unknown-route".to_owned()),
        harness_version: outcome.harness_version.clone(),
        tool_fingerprint,
        repository_revision_family: revision_family(outcome.repository_revision.as_deref()),
    }
}

#[must_use]
pub fn build_cohort_reports(outcomes: &[BehavioralOutcomeV1]) -> Vec<BehavioralCohortReport> {
    let mut groups = BTreeMap::<BehavioralCohortKey, Vec<&BehavioralOutcomeV1>>::new();
    for outcome in outcomes {
        groups.entry(cohort_key(outcome)).or_default().push(outcome);
    }
    groups
        .into_iter()
        .map(|(key, outcomes)| report_for(key, outcomes))
        .collect()
}

#[must_use]
pub fn detect_verified_success_drift(
    baseline: &BehavioralCohortReport,
    current: &BehavioralCohortReport,
    policy: DriftPolicy,
) -> BehavioralDriftReport {
    let same_key = baseline.key == current.key;
    let combined_uncertainty = baseline
        .uncertainty_milli
        .saturating_add(current.uncertainty_milli);
    let delta = baseline
        .verified_success_rate_milli
        .zip(current.verified_success_rate_milli)
        .map(|(before, after)| after as i16 - before as i16);
    let classification = if !same_key {
        DriftClassification::Confounded
    } else if baseline.sample_count < policy.minimum_samples
        || current.sample_count < policy.minimum_samples
        || baseline.verified_success_rate_milli.is_none()
        || current.verified_success_rate_milli.is_none()
        || combined_uncertainty > policy.maximum_uncertainty_milli
    {
        DriftClassification::InsufficientEvidence
    } else if let Some(delta) = delta {
        if delta.unsigned_abs() < policy.minimum_effect_milli
            || delta.unsigned_abs() <= combined_uncertainty
        {
            DriftClassification::NoMaterialChange
        } else if delta > 0 {
            DriftClassification::Improvement
        } else {
            DriftClassification::Regression
        }
    } else {
        DriftClassification::InsufficientEvidence
    };

    BehavioralDriftReport {
        schema_version: BEHAVIORAL_METRICS_SCHEMA_VERSION,
        key: current.key.clone(),
        baseline_samples: baseline.sample_count,
        current_samples: current.sample_count,
        baseline_success_rate_milli: baseline.verified_success_rate_milli,
        current_success_rate_milli: current.verified_success_rate_milli,
        delta_success_rate_milli: delta,
        combined_uncertainty_milli: combined_uncertainty,
        classification,
        // Window comparisons cannot claim causality when policy, model, or task mix changes.
        correlational_only: true,
        baseline_outcome_ids: baseline.outcome_ids.clone(),
        current_outcome_ids: current.outcome_ids.clone(),
    }
}

fn report_for(
    key: BehavioralCohortKey,
    outcomes: Vec<&BehavioralOutcomeV1>,
) -> BehavioralCohortReport {
    let sample_count = outcomes.len();
    let verified_successes = outcomes.iter().filter(|o| o.verified_success).count();
    let verified_failures = outcomes
        .iter()
        .filter(|o| o.terminal_status == BehavioralTerminalStatus::VerifiedFailure)
        .count();
    let cancelled = outcomes
        .iter()
        .filter(|o| o.terminal_status == BehavioralTerminalStatus::Cancelled)
        .count();
    let partial = outcomes
        .iter()
        .filter(|o| o.terminal_status == BehavioralTerminalStatus::Partial)
        .count();
    let inconclusive =
        sample_count.saturating_sub(verified_successes + verified_failures + cancelled + partial);
    let classified = verified_successes + verified_failures;
    let first_pass_verified_successes = outcomes
        .iter()
        .filter(|o| o.verified_success && o.verification_attempts <= 1 && o.recovery_count == 0)
        .count();
    let latencies = outcomes
        .iter()
        .filter_map(|o| o.latency_millis)
        .collect::<Vec<_>>();
    let tokens = outcomes
        .iter()
        .filter(|o| o.verified_success)
        .filter_map(|o| o.observed_token_usage)
        .collect::<Vec<_>>();
    let costs = outcomes
        .iter()
        .filter(|o| o.verified_success)
        .filter_map(|o| o.monetary_cost_microunits)
        .collect::<Vec<_>>();
    let outcome_ids = outcomes
        .iter()
        .map(|o| o.outcome_id.clone())
        .collect::<Vec<_>>();
    BehavioralCohortReport {
        schema_version: BEHAVIORAL_METRICS_SCHEMA_VERSION,
        key,
        sample_count,
        verified_successes,
        verified_failures,
        cancelled,
        partial,
        inconclusive,
        first_pass_verified_successes,
        verification_attempts: outcomes
            .iter()
            .map(|o| u64::from(o.verification_attempts))
            .sum(),
        recovery_count: outcomes.iter().map(|o| u64::from(o.recovery_count)).sum(),
        tool_failure_count: outcomes
            .iter()
            .map(|o| {
                o.tool_executions
                    .iter()
                    .filter(|tool| tool.denied || tool.exit_code.is_some_and(|code| code != 0))
                    .count() as u64
            })
            .sum(),
        verified_success_rate_milli: rate_milli(verified_successes, classified),
        first_pass_success_rate_milli: rate_milli(first_pass_verified_successes, classified),
        median_latency_millis: percentile(&latencies, 50),
        p95_latency_millis: percentile(&latencies, 95),
        tokens_per_verified_success: average_if_complete(tokens, verified_successes),
        cost_per_verified_success_microunits: average_if_complete(costs, verified_successes),
        latency_coverage_milli: coverage_milli(latencies.len(), sample_count),
        token_coverage_milli: coverage_milli(
            outcomes
                .iter()
                .filter(|o| o.observed_token_usage.is_some())
                .count(),
            sample_count,
        ),
        cost_coverage_milli: coverage_milli(
            outcomes
                .iter()
                .filter(|o| o.monetary_cost_microunits.is_some())
                .count(),
            sample_count,
        ),
        uncertainty_milli: uncertainty_milli(classified),
        outcome_ids,
    }
}

fn rate_milli(numerator: usize, denominator: usize) -> Option<u16> {
    (denominator > 0).then(|| ((numerator as u128 * 1_000) / denominator as u128) as u16)
}

fn coverage_milli(observed: usize, total: usize) -> u16 {
    rate_milli(observed, total).unwrap_or(0)
}

fn average_if_complete(values: Vec<u64>, expected: usize) -> Option<u64> {
    (expected > 0 && values.len() == expected)
        .then(|| values.iter().copied().sum::<u64>() / values.len() as u64)
}

fn percentile(values: &[u64], percentile: usize) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() - 1) * percentile / 100).min(sorted.len() - 1);
    sorted.get(index).copied()
}

fn uncertainty_milli(classified: usize) -> u16 {
    if classified == 0 {
        1_000
    } else {
        (1_000 / integer_sqrt(classified)).clamp(25, 1_000) as u16
    }
}

fn integer_sqrt(value: usize) -> usize {
    let mut root = 0usize;
    while root
        .saturating_add(1)
        .saturating_mul(root.saturating_add(1))
        <= value
    {
        root = root.saturating_add(1);
    }
    root.max(1)
}

fn revision_family(revision: Option<&str>) -> String {
    revision
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(12).collect())
        .unwrap_or_else(|| "unknown-revision".to_owned())
}

fn digest(value: &[u8]) -> String {
    encode(Sha256::digest(value))
}
