use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const TOOL_LEARNING_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolExecutionObservation {
    pub tool_id: String,
    pub capability_family: String,
    pub version: String,
    pub schema_fingerprint: String,
    pub cohort_key: String,
    pub session_id: String,
    pub root_task_id: String,
    pub trajectory_id: String,
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
    pub queue_duration_ms: u64,
    pub outcome: ToolExecutionOutcome,
    pub output: ToolOutputStatus,
    pub retry_attempts: u64,
    pub retry_recoveries: u64,
    pub fallback_chain: Vec<String>,
    pub contribution: ToolContribution,
    pub authoritative_task_outcome: Option<AuthoritativeTaskOutcome>,
    pub resource_cost_units: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionOutcome {
    Success,
    Failure,
    Timeout,
    Denied,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputStatus {
    Valid,
    Invalid,
    Empty,
    Unusable,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ToolContribution {
    None,
    Confounded,
    Eligible { receipt_id: String },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthoritativeTaskOutcome {
    VerifiedSuccess,
    VerifiedFailure,
    Cancelled,
    Inconclusive,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolReliabilityReport {
    pub schema_version: u16,
    pub tool_id: String,
    pub capability_family: String,
    pub cohort_key: String,
    pub sample_count: u64,
    pub invocation_failure_count: u64,
    pub timeout_count: u64,
    pub cancellation_count: u64,
    pub denial_count: u64,
    pub invalid_output_count: u64,
    pub unusable_output_count: u64,
    pub eligible_verified_contributions: u64,
    pub confounded_contributions: u64,
    pub verified_successes: u64,
    pub verified_failures: u64,
    pub retry_attempts: u64,
    pub retry_recoveries: u64,
    pub retry_recovery_milli: u16,
    pub p50_latency_ms: Option<u64>,
    pub p95_latency_ms: Option<u64>,
    pub p99_latency_ms: Option<u64>,
    pub correctness_failure_rate_milli: u16,
    pub latency_drift: bool,
    pub source_receipt_ids: Vec<String>,
}

impl ToolReliabilityReport {
    #[must_use]
    pub fn from_observations(observations: &[ToolExecutionObservation]) -> Self {
        let first = observations.first();
        let mut latencies = observations
            .iter()
            .map(|item| item.ended_at_ms.saturating_sub(item.started_at_ms))
            .collect::<Vec<_>>();
        latencies.sort_unstable();
        let sample_count = observations.len() as u64;
        let invocation_failure_count = observations
            .iter()
            .filter(|item| !matches!(item.outcome, ToolExecutionOutcome::Success))
            .count() as u64;
        let timeout_count = observations
            .iter()
            .filter(|item| matches!(item.outcome, ToolExecutionOutcome::Timeout))
            .count() as u64;
        let cancellation_count = observations
            .iter()
            .filter(|item| matches!(item.outcome, ToolExecutionOutcome::Cancelled))
            .count() as u64;
        let denial_count = observations
            .iter()
            .filter(|item| matches!(item.outcome, ToolExecutionOutcome::Denied))
            .count() as u64;
        let invalid_output_count = observations
            .iter()
            .filter(|item| matches!(item.output, ToolOutputStatus::Invalid))
            .count() as u64;
        let unusable_output_count = observations
            .iter()
            .filter(|item| {
                matches!(
                    item.output,
                    ToolOutputStatus::Empty | ToolOutputStatus::Unusable
                )
            })
            .count() as u64;
        let eligible_verified_contributions = observations
            .iter()
            .filter(|item| {
                matches!(
                    (&item.contribution, item.authoritative_task_outcome),
                    (
                        ToolContribution::Eligible { .. },
                        Some(AuthoritativeTaskOutcome::VerifiedSuccess)
                    )
                )
            })
            .count() as u64;
        let confounded_contributions = observations
            .iter()
            .filter(|item| matches!(item.contribution, ToolContribution::Confounded))
            .count() as u64;
        let verified_successes = observations
            .iter()
            .filter(|item| {
                matches!(
                    item.authoritative_task_outcome,
                    Some(AuthoritativeTaskOutcome::VerifiedSuccess)
                )
            })
            .count() as u64;
        let verified_failures = observations
            .iter()
            .filter(|item| {
                matches!(
                    item.authoritative_task_outcome,
                    Some(AuthoritativeTaskOutcome::VerifiedFailure)
                )
            })
            .count() as u64;
        let retry_attempts = observations.iter().map(|item| item.retry_attempts).sum();
        let retry_recoveries = observations.iter().map(|item| item.retry_recoveries).sum();
        let p50_latency_ms = percentile(&latencies, 500);
        let p95_latency_ms = percentile(&latencies, 950);
        let p99_latency_ms = percentile(&latencies, 990);
        let correctness_denominator = verified_successes.saturating_add(verified_failures);
        let correctness_failure_rate_milli = if correctness_denominator == 0 {
            0
        } else {
            ((verified_failures.saturating_mul(1_000) / correctness_denominator) as u16).min(1_000)
        };
        let latency_drift = p50_latency_ms
            .zip(p95_latency_ms)
            .is_some_and(|(p50, p95)| p50 > 0 && p95 >= p50.saturating_mul(2));
        let mut source_receipt_ids = observations
            .iter()
            .filter_map(|item| match &item.contribution {
                ToolContribution::Eligible { receipt_id } => Some(receipt_id.clone()),
                ToolContribution::None | ToolContribution::Confounded => None,
            })
            .collect::<Vec<_>>();
        source_receipt_ids.sort();
        source_receipt_ids.dedup();
        Self {
            schema_version: TOOL_LEARNING_SCHEMA_VERSION,
            tool_id: first.map_or_else(|| "unknown".to_owned(), |item| item.tool_id.clone()),
            capability_family: first.map_or_else(
                || "unknown".to_owned(),
                |item| item.capability_family.clone(),
            ),
            cohort_key: first.map_or_else(|| "unknown".to_owned(), |item| item.cohort_key.clone()),
            sample_count,
            invocation_failure_count,
            timeout_count,
            cancellation_count,
            denial_count,
            invalid_output_count,
            unusable_output_count,
            eligible_verified_contributions,
            confounded_contributions,
            verified_successes,
            verified_failures,
            retry_attempts,
            retry_recoveries,
            retry_recovery_milli: ratio_milli(retry_recoveries, retry_attempts),
            p50_latency_ms,
            p95_latency_ms,
            p99_latency_ms,
            correctness_failure_rate_milli,
            latency_drift,
            source_receipt_ids,
        }
    }
}

/// Builds deterministic tool × cohort reports from canonical execution observations.
#[must_use]
pub fn reports_by_tool_and_cohort(
    observations: &[ToolExecutionObservation],
) -> Vec<ToolReliabilityReport> {
    let mut groups = BTreeMap::<(String, String), Vec<ToolExecutionObservation>>::new();
    for observation in observations {
        groups
            .entry((observation.tool_id.clone(), observation.cohort_key.clone()))
            .or_default()
            .push(observation.clone());
    }
    groups
        .into_values()
        .map(|group| ToolReliabilityReport::from_observations(&group))
        .collect()
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolLearningPolicy {
    pub version: u16,
    pub minimum_samples: u64,
    pub invalid_output_quarantine_threshold: u64,
    pub retry_recovery_prefer_threshold_milli: u16,
    pub retry_recovery_fallback_threshold_milli: u16,
}

impl Default for ToolLearningPolicy {
    fn default() -> Self {
        Self {
            version: 1,
            minimum_samples: 3,
            invalid_output_quarantine_threshold: 3,
            retry_recovery_prefer_threshold_milli: 700,
            retry_recovery_fallback_threshold_milli: 300,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolAdaptiveAction {
    NoChange,
    Quarantine,
    PreferRetry,
    PreferFallback,
    RestorePreference,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolAdaptiveDecision {
    pub schema_version: u16,
    pub policy_version: u16,
    pub tool_id: String,
    pub capability_family: String,
    pub action: ToolAdaptiveAction,
    pub fallback_tool: Option<String>,
    pub rationale: String,
    pub source_outcome_ids: Vec<String>,
    pub expands_capability_authority: bool,
    pub bypasses_approval: bool,
}

#[must_use]
pub fn adapt_tool_policy(
    observations: &[ToolExecutionObservation],
    ordered_candidates: &[String],
    policy: &ToolLearningPolicy,
) -> ToolAdaptiveDecision {
    let first = observations.first();
    let tool_id = first.map_or_else(|| "unknown".to_owned(), |item| item.tool_id.clone());
    let capability_family = first.map_or_else(
        || "unknown".to_owned(),
        |item| item.capability_family.clone(),
    );
    let report = ToolReliabilityReport::from_observations(observations);
    let fallback_tool = ordered_candidates
        .iter()
        .find(|candidate| **candidate != tool_id)
        .cloned();
    let mut versions = BTreeMap::<String, Vec<ToolExecutionObservation>>::new();
    for item in observations {
        versions
            .entry(item.version.clone())
            .or_default()
            .push(item.clone());
    }
    let latest_version = versions.keys().next_back().cloned();
    let latest_healthy = latest_version.as_ref().is_some_and(|version| {
        let items = &versions[version];
        items.len() as u64 >= policy.minimum_samples
            && items.iter().all(|item| {
                matches!(item.outcome, ToolExecutionOutcome::Success)
                    && matches!(item.output, ToolOutputStatus::Valid)
            })
    });
    let older_quarantined = versions.iter().any(|(version, items)| {
        latest_version.as_ref() != Some(version)
            && items
                .iter()
                .filter(|item| matches!(item.output, ToolOutputStatus::Invalid))
                .count() as u64
                >= policy.invalid_output_quarantine_threshold
    });
    let (action, rationale) = if latest_healthy && older_quarantined {
        (
            ToolAdaptiveAction::RestorePreference,
            "new version has stable valid output after an older deterministic-invalid version",
        )
    } else if report.invalid_output_count >= policy.invalid_output_quarantine_threshold
        && report.sample_count >= policy.minimum_samples
    {
        (
            ToolAdaptiveAction::Quarantine,
            "repeated deterministic invalid output reached the quarantine threshold",
        )
    } else if report.retry_attempts >= policy.minimum_samples
        && report.retry_recovery_milli >= policy.retry_recovery_prefer_threshold_milli
    {
        (
            ToolAdaptiveAction::PreferRetry,
            "retry recovery evidence exceeds the bounded retry threshold",
        )
    } else if report.retry_attempts >= policy.minimum_samples
        && report.retry_recovery_milli <= policy.retry_recovery_fallback_threshold_milli
    {
        (
            ToolAdaptiveAction::PreferFallback,
            "retry recovery evidence is below the bounded recovery threshold",
        )
    } else {
        (
            ToolAdaptiveAction::NoChange,
            "insufficient evidence for an adaptive tool policy change",
        )
    };
    ToolAdaptiveDecision {
        schema_version: TOOL_LEARNING_SCHEMA_VERSION,
        policy_version: policy.version,
        tool_id,
        capability_family,
        action,
        fallback_tool,
        rationale: rationale.to_owned(),
        source_outcome_ids: observations
            .iter()
            .map(|item| item.root_task_id.clone())
            .collect(),
        expands_capability_authority: false,
        bypasses_approval: false,
    }
}

fn ratio_milli(numerator: u64, denominator: u64) -> u16 {
    if denominator == 0 {
        return 1_000;
    }
    ((numerator.saturating_mul(1_000) / denominator) as u16).min(1_000)
}

fn percentile(values: &[u64], percentile_milli: u16) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let rank = ((values.len() as u128 * u128::from(percentile_milli)).saturating_add(999)) / 1_000;
    let index = rank.saturating_sub(1).min(values.len() as u128 - 1) as usize;
    values.get(index).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(tool_id: &str) -> ToolExecutionObservation {
        ToolExecutionObservation {
            tool_id: tool_id.to_owned(),
            capability_family: "search".to_owned(),
            version: "v1".to_owned(),
            schema_fingerprint: "schema-1".to_owned(),
            cohort_key: "rust-debugging".to_owned(),
            session_id: format!("session-{tool_id}"),
            root_task_id: "task-1".to_owned(),
            trajectory_id: "trajectory-1".to_owned(),
            started_at_ms: 0,
            ended_at_ms: 100,
            queue_duration_ms: 0,
            outcome: ToolExecutionOutcome::Success,
            output: ToolOutputStatus::Valid,
            retry_attempts: 0,
            retry_recoveries: 0,
            fallback_chain: Vec::new(),
            contribution: ToolContribution::None,
            authoritative_task_outcome: None,
            resource_cost_units: None,
        }
    }

    #[test]
    fn p95_latency_drift_is_not_reported_as_correctness_failure() {
        let mut observations = Vec::new();
        for index in 0..20 {
            let mut item = observation("slow");
            item.ended_at_ms = if index >= 18 { 2_000 } else { 100 };
            observations.push(item);
        }
        let report = ToolReliabilityReport::from_observations(&observations);
        assert_eq!(report.p95_latency_ms, Some(2_000));
        assert_eq!(report.correctness_failure_rate_milli, 0);
        assert!(report.latency_drift);
    }

    #[test]
    fn repeated_invalid_output_quarantines_tool_after_threshold() {
        let mut observations = Vec::new();
        for _ in 0..4 {
            let mut item = observation("broken");
            item.output = ToolOutputStatus::Invalid;
            observations.push(item);
        }
        let decision = adapt_tool_policy(
            &observations,
            &["broken".to_owned(), "fallback".to_owned()],
            &ToolLearningPolicy::default(),
        );
        assert_eq!(decision.action, ToolAdaptiveAction::Quarantine);
        assert_eq!(decision.fallback_tool.as_deref(), Some("fallback"));
    }

    #[test]
    fn retry_recovery_rate_selects_retry_or_fallback_deterministically() {
        let mut healthy = observation("recovering");
        healthy.retry_attempts = 10;
        healthy.retry_recoveries = 9;
        let retry = adapt_tool_policy(
            &[healthy.clone()],
            &["recovering".to_owned(), "fallback".to_owned()],
            &ToolLearningPolicy::default(),
        );
        assert_eq!(retry.action, ToolAdaptiveAction::PreferRetry);

        healthy.retry_recoveries = 1;
        let fallback = adapt_tool_policy(
            &[healthy],
            &["recovering".to_owned(), "fallback".to_owned()],
            &ToolLearningPolicy::default(),
        );
        assert_eq!(fallback.action, ToolAdaptiveAction::PreferFallback);
    }

    #[test]
    fn confounded_multi_tool_success_receives_no_individual_credit() {
        let mut item = observation("ambiguous");
        item.contribution = ToolContribution::Confounded;
        item.authoritative_task_outcome = Some(AuthoritativeTaskOutcome::VerifiedSuccess);
        let report = ToolReliabilityReport::from_observations(&[item]);
        assert_eq!(report.eligible_verified_contributions, 0);
        assert_eq!(report.confounded_contributions, 1);
    }

    #[test]
    fn sole_verification_artifact_receives_exact_contribution_credit() {
        let mut item = observation("verifier");
        item.contribution = ToolContribution::Eligible {
            receipt_id: "receipt-42".to_owned(),
        };
        item.authoritative_task_outcome = Some(AuthoritativeTaskOutcome::VerifiedSuccess);
        let report = ToolReliabilityReport::from_observations(&[item]);
        assert_eq!(report.eligible_verified_contributions, 1);
        assert_eq!(report.source_receipt_ids, vec!["receipt-42"]);
    }

    #[test]
    fn healthy_new_version_restores_preference_after_quarantine() {
        let mut observations = Vec::new();
        for _ in 0..4 {
            let mut item = observation("tool");
            item.version = "v1".to_owned();
            item.output = ToolOutputStatus::Invalid;
            observations.push(item);
        }
        for _ in 0..5 {
            let mut item = observation("tool");
            item.version = "v2".to_owned();
            observations.push(item);
        }
        let decision = adapt_tool_policy(
            &observations,
            &["tool".to_owned(), "fallback".to_owned()],
            &ToolLearningPolicy::default(),
        );
        assert_eq!(decision.action, ToolAdaptiveAction::RestorePreference);
    }

    #[test]
    fn learned_policy_never_expands_authority_and_replay_is_stable() {
        let observations = vec![observation("tool")];
        let policy = ToolLearningPolicy::default();
        let first = adapt_tool_policy(&observations, &["tool".to_owned()], &policy);
        let replay = adapt_tool_policy(&observations, &["tool".to_owned()], &policy);
        assert_eq!(first, replay);
        assert!(!first.expands_capability_authority);
        assert!(!first.bypasses_approval);
    }
}
