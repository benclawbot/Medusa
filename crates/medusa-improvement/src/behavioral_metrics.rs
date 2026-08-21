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

/// A dimension that may be removed only when a caller explicitly requests a broader view.
/// Exact cohort reports continue to include every dimension by default.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CohortDimension {
    WorkspaceMode,
    TaskIntent,
    LanguageFamilies,
    RiskClass,
    ComplexityBand,
    Model,
    Provider,
    Route,
    Harness,
    Tools,
    RepositoryRevisionFamily,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BehavioralCohortReportView {
    pub schema_version: u16,
    pub report: BehavioralCohortReport,
    pub removed_dimensions: Vec<CohortDimension>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct BehavioralAggregateMetrics {
    pub schema_version: u16,
    pub all_attempt_count: usize,
    pub eligible_comparable_count: usize,
    pub verified_success_count: usize,
    pub verified_failure_count: usize,
    pub cancelled_count: usize,
    pub partial_count: usize,
    pub inconclusive_count: usize,
    pub model_request_count: u64,
    pub tool_call_count: u64,
    pub failed_or_denied_tool_call_count: u64,
    pub repair_loop_count: u64,
    pub model_requests_per_verified_success_milli: Option<u64>,
    pub repair_loops_per_verified_success_milli: Option<u64>,
    pub p95_latency_millis: Option<u64>,
    pub latency_coverage_milli: u16,
    pub cost_per_verified_success_microunits: Option<u64>,
    pub cost_coverage_milli: u16,
    pub uncertainty_milli: u16,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct BehavioralWindow {
    pub since_unix_ms: Option<i64>,
    pub until_unix_ms: Option<i64>,
    pub max_samples: Option<usize>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BehavioralMetric {
    VerifiedSuccessRate,
    P95LatencyMillis,
    RepairLoopsPerVerifiedSuccess,
    CostPerVerifiedSuccess,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetricDriftPolicy {
    pub minimum_samples: usize,
    pub minimum_effect_milli: u16,
    pub maximum_uncertainty_milli: u16,
    pub minimum_coverage_milli: u16,
}

impl Default for MetricDriftPolicy {
    fn default() -> Self {
        Self {
            minimum_samples: 3,
            minimum_effect_milli: 50,
            maximum_uncertainty_milli: 500,
            minimum_coverage_milli: 500,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetricDriftReport {
    pub schema_version: u16,
    pub metric: BehavioralMetric,
    pub baseline_value: Option<u64>,
    pub current_value: Option<u64>,
    pub delta: Option<i64>,
    pub baseline_samples: usize,
    pub current_samples: usize,
    pub baseline_coverage_milli: u16,
    pub current_coverage_milli: u16,
    pub combined_uncertainty_milli: u16,
    pub classification: DriftClassification,
    pub correlational_only: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftLifecycleState {
    InsufficientEvidence,
    Stable,
    CandidateChange,
    ConfirmedImprovement,
    ConfirmedRegression,
    Recovering,
    Resolved,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DriftLifecyclePolicy {
    pub confirmations_required: u16,
    pub cooldown_millis: i64,
}

impl Default for DriftLifecyclePolicy {
    fn default() -> Self {
        Self {
            confirmations_required: 2,
            cooldown_millis: 15 * 60 * 1_000,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DriftLifecycle {
    pub schema_version: u16,
    pub state: DriftLifecycleState,
    pub consecutive_regressions: u16,
    pub consecutive_improvements: u16,
    pub cooldown_until_unix_ms: Option<i64>,
    pub source_report_ids: Vec<String>,
}

const METRICS_SCHEMA_VERSION: u16 = 1;

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
    build_cohort_reports_with_view(outcomes, &[])
        .into_iter()
        .map(|view| view.report)
        .collect()
}

/// Builds exact or explicitly broadened views over the same source outcomes. The returned view
/// records every removed dimension so a consumer cannot mistake a broad aggregate for an exact
/// cohort comparison.
#[must_use]
pub fn build_cohort_reports_with_view(
    outcomes: &[BehavioralOutcomeV1],
    removed_dimensions: &[CohortDimension],
) -> Vec<BehavioralCohortReportView> {
    let mut removed = removed_dimensions.to_vec();
    removed.sort_unstable();
    removed.dedup();
    let mut groups = BTreeMap::<BehavioralCohortKey, Vec<&BehavioralOutcomeV1>>::new();
    for outcome in outcomes {
        groups
            .entry(project_cohort_key(cohort_key(outcome), &removed))
            .or_default()
            .push(outcome);
    }
    groups
        .into_iter()
        .map(|(key, outcomes)| BehavioralCohortReportView {
            schema_version: METRICS_SCHEMA_VERSION,
            report: report_for(key, outcomes),
            removed_dimensions: removed.clone(),
        })
        .collect()
}

#[must_use]
pub fn aggregate_behavioral_metrics(
    outcomes: &[BehavioralOutcomeV1],
) -> BehavioralAggregateMetrics {
    let all_attempt_count = outcomes.len();
    let verified_success_count = outcomes.iter().filter(|o| o.verified_success).count();
    let verified_failure_count = outcomes
        .iter()
        .filter(|o| o.terminal_status == BehavioralTerminalStatus::VerifiedFailure)
        .count();
    let eligible_comparable_count = outcomes
        .iter()
        .filter(|o| {
            o.root_task_eligible
                && matches!(
                    o.terminal_status,
                    BehavioralTerminalStatus::VerifiedSuccess
                        | BehavioralTerminalStatus::VerifiedFailure
                )
        })
        .count();
    let cancelled_count = outcomes
        .iter()
        .filter(|o| o.terminal_status == BehavioralTerminalStatus::Cancelled)
        .count();
    let partial_count = outcomes
        .iter()
        .filter(|o| o.terminal_status == BehavioralTerminalStatus::Partial)
        .count();
    let inconclusive_count = all_attempt_count.saturating_sub(
        verified_success_count + verified_failure_count + cancelled_count + partial_count,
    );
    let model_request_count = outcomes
        .iter()
        .map(|o| o.model_executions.len() as u64)
        .sum();
    let tool_call_count = outcomes
        .iter()
        .map(|o| o.tool_executions.len() as u64)
        .sum();
    let failed_or_denied_tool_call_count = outcomes
        .iter()
        .flat_map(|o| o.tool_executions.iter())
        .filter(|tool| tool.denied || tool.exit_code.is_some_and(|code| code != 0))
        .count() as u64;
    let repair_loop_count = outcomes
        .iter()
        .map(|o| u64::from(o.failed_verification_attempts) + u64::from(o.recovery_count))
        .sum();
    let latencies = outcomes
        .iter()
        .filter_map(|o| o.latency_millis)
        .collect::<Vec<_>>();
    let costs = outcomes
        .iter()
        .filter(|o| o.verified_success)
        .filter_map(|o| o.monetary_cost_microunits)
        .collect::<Vec<_>>();
    BehavioralAggregateMetrics {
        schema_version: METRICS_SCHEMA_VERSION,
        all_attempt_count,
        eligible_comparable_count,
        verified_success_count,
        verified_failure_count,
        cancelled_count,
        partial_count,
        inconclusive_count,
        model_request_count,
        tool_call_count,
        failed_or_denied_tool_call_count,
        repair_loop_count,
        model_requests_per_verified_success_milli: ratio_milli(
            model_request_count,
            verified_success_count,
        ),
        repair_loops_per_verified_success_milli: ratio_milli(
            repair_loop_count,
            verified_success_count,
        ),
        p95_latency_millis: percentile(&latencies, 95),
        latency_coverage_milli: coverage_milli(latencies.len(), all_attempt_count),
        cost_per_verified_success_microunits: average_if_complete(costs, verified_success_count),
        cost_coverage_milli: coverage_milli(
            outcomes
                .iter()
                .filter(|o| o.monetary_cost_microunits.is_some())
                .count(),
            all_attempt_count,
        ),
        uncertainty_milli: uncertainty_milli(eligible_comparable_count),
    }
}

/// Selects a deterministic rolling window without dropping source identity. Missing timestamps
/// are excluded from bounded windows because they cannot be compared safely.
#[must_use]
pub fn outcomes_in_window(
    outcomes: &[BehavioralOutcomeV1],
    window: BehavioralWindow,
) -> Vec<BehavioralOutcomeV1> {
    if window.since_unix_ms.is_none()
        && window.until_unix_ms.is_none()
        && window.max_samples.is_none()
    {
        return outcomes.to_vec();
    }
    let mut selected = outcomes
        .iter()
        .filter(|outcome| {
            let Some(timestamp) = outcome.last_event_unix_ms else {
                return false;
            };
            window.since_unix_ms.is_none_or(|since| timestamp >= since)
                && window.until_unix_ms.is_none_or(|until| timestamp <= until)
        })
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| {
        left.last_event_unix_ms
            .cmp(&right.last_event_unix_ms)
            .then_with(|| left.outcome_id.cmp(&right.outcome_id))
    });
    if let Some(max_samples) = window.max_samples {
        let drop_count = selected.len().saturating_sub(max_samples);
        if drop_count > 0 {
            selected.drain(..drop_count);
        }
    }
    selected.into_iter().cloned().collect()
}

#[must_use]
pub fn detect_metric_drift(
    baseline: &BehavioralAggregateMetrics,
    current: &BehavioralAggregateMetrics,
    metric: BehavioralMetric,
    policy: MetricDriftPolicy,
) -> MetricDriftReport {
    let (baseline_value, current_value, baseline_coverage_milli, current_coverage_milli) =
        metric_values(baseline, current, metric);
    let delta = baseline_value
        .zip(current_value)
        .and_then(|(before, after)| {
            i64::try_from(after)
                .ok()?
                .checked_sub(i64::try_from(before).ok()?)
        });
    let combined_uncertainty = baseline
        .uncertainty_milli
        .saturating_add(current.uncertainty_milli);
    let classification = if baseline.all_attempt_count < policy.minimum_samples
        || current.all_attempt_count < policy.minimum_samples
        || baseline_value.is_none()
        || current_value.is_none()
        || baseline_coverage_milli < policy.minimum_coverage_milli
        || current_coverage_milli < policy.minimum_coverage_milli
        || combined_uncertainty > policy.maximum_uncertainty_milli
    {
        DriftClassification::InsufficientEvidence
    } else if let Some(delta) = delta {
        let before = baseline_value.unwrap_or_default();
        let effect_milli = if before == 0 {
            delta.unsigned_abs().min(1_000)
        } else {
            (delta.unsigned_abs().saturating_mul(1_000) / before).min(1_000)
        };
        if effect_milli < u64::from(policy.minimum_effect_milli)
            || combined_uncertainty > effect_milli as u16
        {
            DriftClassification::NoMaterialChange
        } else {
            let higher_is_better = matches!(metric, BehavioralMetric::VerifiedSuccessRate);
            let improved = if higher_is_better {
                delta > 0
            } else {
                delta < 0
            };
            if improved {
                DriftClassification::Improvement
            } else {
                DriftClassification::Regression
            }
        }
    } else {
        DriftClassification::InsufficientEvidence
    };
    MetricDriftReport {
        schema_version: METRICS_SCHEMA_VERSION,
        metric,
        baseline_value,
        current_value,
        delta,
        baseline_samples: baseline.all_attempt_count,
        current_samples: current.all_attempt_count,
        baseline_coverage_milli,
        current_coverage_milli,
        combined_uncertainty_milli: combined_uncertainty,
        classification,
        correlational_only: true,
    }
}

#[must_use]
pub fn advance_drift_lifecycle(
    previous: Option<&DriftLifecycle>,
    report: &MetricDriftReport,
    now_unix_ms: i64,
    policy: DriftLifecyclePolicy,
) -> DriftLifecycle {
    let mut next = previous.cloned().unwrap_or(DriftLifecycle {
        schema_version: METRICS_SCHEMA_VERSION,
        state: DriftLifecycleState::Unknown,
        consecutive_regressions: 0,
        consecutive_improvements: 0,
        cooldown_until_unix_ms: None,
        source_report_ids: Vec::new(),
    });
    let report_id = digest(&serde_json::to_vec(report).unwrap_or_default());
    if !next.source_report_ids.contains(&report_id) {
        next.source_report_ids.push(report_id);
    }
    if next.source_report_ids.len() > 128 {
        let excess = next.source_report_ids.len() - 128;
        next.source_report_ids.drain(..excess);
    }
    if next
        .cooldown_until_unix_ms
        .is_some_and(|until| now_unix_ms < until)
    {
        return next;
    }
    match report.classification {
        DriftClassification::InsufficientEvidence | DriftClassification::Confounded => {
            next.state = DriftLifecycleState::InsufficientEvidence;
            next.consecutive_regressions = 0;
            next.consecutive_improvements = 0;
        }
        DriftClassification::NoMaterialChange => {
            next.state = match next.state {
                DriftLifecycleState::ConfirmedRegression | DriftLifecycleState::Recovering => {
                    DriftLifecycleState::Recovering
                }
                DriftLifecycleState::ConfirmedImprovement => DriftLifecycleState::Resolved,
                _ => DriftLifecycleState::Stable,
            };
            next.consecutive_regressions = 0;
            next.consecutive_improvements = 0;
        }
        DriftClassification::Regression => {
            next.consecutive_regressions = next.consecutive_regressions.saturating_add(1);
            next.consecutive_improvements = 0;
            next.state = if next.state == DriftLifecycleState::ConfirmedImprovement {
                DriftLifecycleState::Recovering
            } else if next.consecutive_regressions >= policy.confirmations_required.max(1) {
                DriftLifecycleState::ConfirmedRegression
            } else {
                DriftLifecycleState::CandidateChange
            };
        }
        DriftClassification::Improvement => {
            next.consecutive_improvements = next.consecutive_improvements.saturating_add(1);
            next.consecutive_regressions = 0;
            next.state = if matches!(
                next.state,
                DriftLifecycleState::ConfirmedRegression | DriftLifecycleState::Recovering
            ) {
                if next.consecutive_improvements >= policy.confirmations_required.max(1) {
                    DriftLifecycleState::Resolved
                } else {
                    DriftLifecycleState::Recovering
                }
            } else if next.consecutive_improvements >= policy.confirmations_required.max(1) {
                DriftLifecycleState::ConfirmedImprovement
            } else {
                DriftLifecycleState::CandidateChange
            };
        }
    }
    if matches!(
        next.state,
        DriftLifecycleState::CandidateChange
            | DriftLifecycleState::ConfirmedImprovement
            | DriftLifecycleState::ConfirmedRegression
            | DriftLifecycleState::Recovering
    ) {
        next.cooldown_until_unix_ms = Some(now_unix_ms.saturating_add(policy.cooldown_millis));
    } else {
        next.cooldown_until_unix_ms = None;
    }
    next
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

fn project_cohort_key(
    mut key: BehavioralCohortKey,
    removed_dimensions: &[CohortDimension],
) -> BehavioralCohortKey {
    for dimension in removed_dimensions {
        match dimension {
            CohortDimension::WorkspaceMode => key.workspace_mode = BehavioralWorkspaceMode::Unknown,
            CohortDimension::TaskIntent => key.task_intent = BehavioralTaskIntent::Unknown,
            CohortDimension::LanguageFamilies => key.language_families = vec!["unknown".to_owned()],
            CohortDimension::RiskClass => key.risk_class = BehavioralRiskClass::Unknown,
            CohortDimension::ComplexityBand => {
                key.complexity_band = BehavioralComplexityBand::Unknown
            }
            CohortDimension::Model => key.model = "unknown-model".to_owned(),
            CohortDimension::Provider => key.provider = "unknown-provider".to_owned(),
            CohortDimension::Route => key.route_fingerprint = "unknown-route".to_owned(),
            CohortDimension::Harness => key.harness_version = "unknown-harness".to_owned(),
            CohortDimension::Tools => key.tool_fingerprint = "unknown-tools".to_owned(),
            CohortDimension::RepositoryRevisionFamily => {
                key.repository_revision_family = "unknown-revision".to_owned()
            }
        }
    }
    key
}

fn metric_values(
    baseline: &BehavioralAggregateMetrics,
    current: &BehavioralAggregateMetrics,
    metric: BehavioralMetric,
) -> (Option<u64>, Option<u64>, u16, u16) {
    match metric {
        BehavioralMetric::VerifiedSuccessRate => (
            rate_milli(
                baseline.verified_success_count,
                baseline.eligible_comparable_count,
            )
            .map(u64::from),
            rate_milli(
                current.verified_success_count,
                current.eligible_comparable_count,
            )
            .map(u64::from),
            coverage_milli(
                baseline.eligible_comparable_count,
                baseline.all_attempt_count,
            ),
            coverage_milli(current.eligible_comparable_count, current.all_attempt_count),
        ),
        BehavioralMetric::P95LatencyMillis => (
            baseline.p95_latency_millis,
            current.p95_latency_millis,
            baseline.latency_coverage_milli,
            current.latency_coverage_milli,
        ),
        BehavioralMetric::RepairLoopsPerVerifiedSuccess => (
            baseline.repair_loops_per_verified_success_milli,
            current.repair_loops_per_verified_success_milli,
            coverage_milli(
                baseline.eligible_comparable_count,
                baseline.all_attempt_count,
            ),
            coverage_milli(current.eligible_comparable_count, current.all_attempt_count),
        ),
        BehavioralMetric::CostPerVerifiedSuccess => (
            baseline.cost_per_verified_success_microunits,
            current.cost_per_verified_success_microunits,
            baseline.cost_coverage_milli,
            current.cost_coverage_milli,
        ),
    }
}

fn rate_milli(numerator: usize, denominator: usize) -> Option<u16> {
    (denominator > 0).then(|| ((numerator as u128 * 1_000) / denominator as u128) as u16)
}

fn coverage_milli(observed: usize, total: usize) -> u16 {
    rate_milli(observed, total).unwrap_or(0)
}

fn ratio_milli(numerator: u64, denominator: usize) -> Option<u64> {
    (denominator > 0).then(|| numerator.saturating_mul(1_000) / denominator as u64)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavioral_outcome::{
        BehavioralModelExecutionV1, BehavioralTaskClassificationV1, BehavioralTerminalStatus,
    };
    use serde_json::Value;

    fn outcome(
        id: &str,
        provider: &str,
        status: BehavioralTerminalStatus,
        latency_millis: Option<u64>,
        failed_verification_attempts: u32,
        cost: Option<u64>,
    ) -> BehavioralOutcomeV1 {
        BehavioralOutcomeV1 {
            schema_version: 1,
            outcome_id: id.to_owned(),
            root_task_id: id.to_owned(),
            session_id: id.to_owned(),
            trajectory_id: id.to_owned(),
            root_task_eligible: true,
            repository_revision: Some("revision-1".to_owned()),
            harness_version: "harness-1".to_owned(),
            task_classification: BehavioralTaskClassificationV1 {
                schema_version: 1,
                workspace_mode: BehavioralWorkspaceMode::Git,
                intent: BehavioralTaskIntent::BugFix,
                language_families: vec!["rust".to_owned()],
                risk_class: BehavioralRiskClass::Medium,
                complexity_band: BehavioralComplexityBand::Medium,
                task_features: vec!["tests".to_owned()],
                unknowns: Vec::new(),
            },
            terminal_status: status,
            verified_success: status == BehavioralTerminalStatus::VerifiedSuccess,
            verification_passed: Some(status == BehavioralTerminalStatus::VerifiedSuccess),
            verification_receipt_ids: vec!["receipt".to_owned()],
            integration_receipt_ids: Vec::new(),
            model_executions: vec![BehavioralModelExecutionV1 {
                event_id: format!("{id}-model"),
                event_sequence: 1,
                provider: provider.to_owned(),
                model: "model-1".to_owned(),
                request_id: Some(format!("{id}-request")),
                request_fingerprint: Some("route-1".to_owned()),
                manifest_ref: None,
                attempt_ordinal: 1,
                parent_request_id: None,
                response_id: Some(format!("{id}-response")),
                usage: Some(Value::from(10)),
                failed: false,
                failure_event_id: None,
                mutation_contribution: true,
            }],
            provider_execution_records: Vec::new(),
            tool_executions: Vec::new(),
            mutation_count: 1,
            verification_attempts: failed_verification_attempts.saturating_add(1),
            failed_verification_attempts,
            recovery_count: 0,
            cancellation_requested: status == BehavioralTerminalStatus::Cancelled,
            user_correction_count: 0,
            approval_denial_count: 0,
            latency_millis,
            observed_token_usage: Some(10),
            monetary_cost_microunits: cost,
            source_event_ids: vec![format!("{id}-event")],
            source_event_checksums: vec![format!("{id}-checksum")],
            first_event_unix_ms: Some(1),
            last_event_unix_ms: Some(2),
        }
    }

    #[test]
    fn broader_views_report_removed_dimensions_without_pooling_by_default() {
        let outcomes = vec![
            outcome(
                "provider-a",
                "provider-a",
                BehavioralTerminalStatus::VerifiedSuccess,
                Some(10),
                0,
                Some(5),
            ),
            outcome(
                "provider-b",
                "provider-b",
                BehavioralTerminalStatus::VerifiedFailure,
                Some(20),
                1,
                Some(7),
            ),
        ];

        let exact = build_cohort_reports(&outcomes);
        assert_eq!(exact.len(), 2);

        let broader = build_cohort_reports_with_view(
            &outcomes,
            &[CohortDimension::Provider, CohortDimension::Route],
        );
        assert_eq!(broader.len(), 1);
        assert_eq!(broader[0].report.sample_count, 2);
        assert_eq!(
            broader[0].removed_dimensions,
            vec![CohortDimension::Provider, CohortDimension::Route]
        );
        assert_eq!(broader[0].report.verified_success_rate_milli, Some(500));
    }

    #[test]
    fn aggregate_metrics_keep_censored_attempts_and_unknown_cost_explicit() {
        let outcomes = vec![
            outcome(
                "success",
                "provider-a",
                BehavioralTerminalStatus::VerifiedSuccess,
                Some(10),
                0,
                None,
            ),
            outcome(
                "cancelled",
                "provider-a",
                BehavioralTerminalStatus::Cancelled,
                None,
                0,
                Some(1),
            ),
        ];

        let metrics = aggregate_behavioral_metrics(&outcomes);
        assert_eq!(metrics.all_attempt_count, 2);
        assert_eq!(metrics.eligible_comparable_count, 1);
        assert_eq!(metrics.cancelled_count, 1);
        assert_eq!(metrics.cost_coverage_milli, 500);
        assert_eq!(metrics.cost_per_verified_success_microunits, None);
        assert_eq!(
            metrics.model_requests_per_verified_success_milli,
            Some(2_000)
        );
    }

    #[test]
    fn metric_drift_detects_latency_and_repair_regressions_with_coverage() {
        let baseline = (0..20)
            .map(|index| {
                outcome(
                    &format!("baseline-{index}"),
                    "provider-a",
                    BehavioralTerminalStatus::VerifiedSuccess,
                    Some(if index == 19 { 1_000 } else { 10 }),
                    0,
                    Some(10),
                )
            })
            .collect::<Vec<_>>();
        let current = (0..20)
            .map(|index| {
                outcome(
                    &format!("current-{index}"),
                    "provider-a",
                    BehavioralTerminalStatus::VerifiedSuccess,
                    Some(if index == 19 { 2_000 } else { 20 }),
                    1,
                    Some(10),
                )
            })
            .collect::<Vec<_>>();

        let baseline_metrics = aggregate_behavioral_metrics(&baseline);
        let current_metrics = aggregate_behavioral_metrics(&current);
        let latency = detect_metric_drift(
            &baseline_metrics,
            &current_metrics,
            BehavioralMetric::P95LatencyMillis,
            MetricDriftPolicy::default(),
        );
        assert_eq!(latency.classification, DriftClassification::Regression);
        let repair = detect_metric_drift(
            &baseline_metrics,
            &current_metrics,
            BehavioralMetric::RepairLoopsPerVerifiedSuccess,
            MetricDriftPolicy::default(),
        );
        assert_eq!(repair.classification, DriftClassification::Regression);
        assert_eq!(repair.baseline_coverage_milli, 1_000);
        assert_eq!(repair.current_coverage_milli, 1_000);
    }

    #[test]
    fn drift_lifecycle_requires_hysteresis_and_resolves_after_recovery() {
        let policy = DriftLifecyclePolicy {
            confirmations_required: 2,
            cooldown_millis: 0,
        };
        let regression = MetricDriftReport {
            schema_version: 1,
            metric: BehavioralMetric::VerifiedSuccessRate,
            baseline_value: Some(1_000),
            current_value: Some(700),
            delta: Some(-300),
            baseline_samples: 20,
            current_samples: 20,
            baseline_coverage_milli: 1_000,
            current_coverage_milli: 1_000,
            combined_uncertainty_milli: 100,
            classification: DriftClassification::Regression,
            correlational_only: true,
        };
        let improvement = MetricDriftReport {
            classification: DriftClassification::Improvement,
            ..regression.clone()
        };
        let first = advance_drift_lifecycle(None, &regression, 1, policy);
        assert_eq!(first.state, DriftLifecycleState::CandidateChange);
        let confirmed = advance_drift_lifecycle(Some(&first), &regression, 2, policy);
        assert_eq!(confirmed.state, DriftLifecycleState::ConfirmedRegression);
        let recovering = advance_drift_lifecycle(Some(&confirmed), &improvement, 3, policy);
        assert_eq!(recovering.state, DriftLifecycleState::Recovering);
        let resolved = advance_drift_lifecycle(Some(&recovering), &improvement, 4, policy);
        assert_eq!(resolved.state, DriftLifecycleState::Resolved);
        assert!(!resolved.source_report_ids.is_empty());
    }

    #[test]
    fn rolling_window_is_time_bounded_and_keeps_newest_samples_deterministically() {
        let mut older = outcome(
            "older",
            "provider-a",
            BehavioralTerminalStatus::VerifiedSuccess,
            Some(10),
            0,
            Some(10),
        );
        older.last_event_unix_ms = Some(10);
        let mut newer = outcome(
            "newer",
            "provider-a",
            BehavioralTerminalStatus::VerifiedSuccess,
            Some(20),
            0,
            Some(20),
        );
        newer.last_event_unix_ms = Some(20);
        let selected = outcomes_in_window(
            &[older, newer],
            BehavioralWindow {
                since_unix_ms: Some(10),
                until_unix_ms: Some(20),
                max_samples: Some(1),
            },
        );
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].outcome_id, "newer");
    }
}
