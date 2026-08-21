#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        behavioral_metrics::{
            BehavioralCohortKey, BehavioralCohortReport, BehavioralDriftReport, DriftClassification,
        },
        behavioral_outcome::{
            BehavioralComplexityBand, BehavioralRiskClass, BehavioralTaskIntent,
            BehavioralWorkspaceMode,
        },
        improvement_controller::{
            ImprovementCandidate, ImprovementControllerPolicy, IndependentEvaluation,
            evaluate_candidate,
        },
    };

    fn key() -> BehavioralCohortKey {
        BehavioralCohortKey {
            classification_version: 1,
            workspace_mode: BehavioralWorkspaceMode::Git,
            task_intent: BehavioralTaskIntent::BugFix,
            language_families: vec!["rust".to_owned()],
            risk_class: BehavioralRiskClass::Low,
            complexity_band: BehavioralComplexityBand::Medium,
            model: "model-a".to_owned(),
            provider: "provider-a".to_owned(),
            route_fingerprint: "route-a".to_owned(),
            harness_version: "harness-1".to_owned(),
            tool_fingerprint: "tools-1".to_owned(),
            repository_revision_family: "repo-1".to_owned(),
        }
    }

    fn report() -> BehavioralCohortReport {
        BehavioralCohortReport {
            schema_version: 1,
            key: key(),
            sample_count: 10,
            verified_successes: 9,
            verified_failures: 1,
            cancelled: 0,
            partial: 0,
            inconclusive: 0,
            first_pass_verified_successes: 8,
            verification_attempts: 11,
            recovery_count: 1,
            tool_failure_count: 1,
            verified_success_rate_milli: Some(900),
            first_pass_success_rate_milli: Some(800),
            median_latency_millis: Some(100),
            p95_latency_millis: Some(250),
            tokens_per_verified_success: Some(1000),
            cost_per_verified_success_microunits: Some(75),
            latency_coverage_milli: 1000,
            token_coverage_milli: 1000,
            cost_coverage_milli: 1000,
            uncertainty_milli: 100,
            outcome_ids: vec!["outcome-1".to_owned()],
        }
    }

    #[test]
    fn missing_report_is_insufficient_and_cost_stays_unknown() {
        let snapshot =
            build_behavioral_health_snapshot(None, &[], None, None, None, None, None, None);
        assert_eq!(
            snapshot.status,
            BehavioralHealthStatus::InsufficientEvidence
        );
        assert_eq!(snapshot.verified_success_rate_milli, None);
        assert_eq!(snapshot.cost_per_verified_success_microunits, None);
    }

    #[test]
    fn shared_snapshot_carries_drift_and_promotion_state_without_recalculation() {
        let drift = BehavioralDriftReport {
            schema_version: 1,
            key: key(),
            baseline_samples: 10,
            current_samples: 10,
            baseline_success_rate_milli: Some(950),
            current_success_rate_milli: Some(900),
            delta_success_rate_milli: Some(-50),
            combined_uncertainty_milli: 100,
            classification: DriftClassification::Regression,
            correlational_only: false,
            baseline_outcome_ids: vec!["baseline-1".to_owned()],
            current_outcome_ids: vec!["outcome-1".to_owned()],
        };
        let candidate = ImprovementCandidate {
            id: "candidate-1".to_owned(),
            target: crate::meta_improvement::MetaImprovementTarget::PromptOverlay,
            current_policy_version: "policy-v1".to_owned(),
            predecessor_policy_version: "policy-v0".to_owned(),
            source_drift_ids: vec!["drift-1".to_owned()],
            source_outcome_ids: vec!["outcome-1".to_owned()],
            cohort: "rust".to_owned(),
            frozen_oracle_version: "oracle-1".to_owned(),
            hypothesized_mechanism: "x".to_owned(),
            minimal_change: "y".to_owned(),
            minimum_samples: 1,
            minimum_effect_milli: 1,
            canary_required: true,
            evaluator_changed: false,
            authority_expanded: false,
            protected_change: false,
        };
        let evaluation = IndependentEvaluation {
            evaluation_id: "eval-1".to_owned(),
            sample_count: 10,
            effect_milli: 100,
            baseline_verified_success_milli: 800,
            verified_success_milli: 900,
            latency_delta_ms: 0,
            cost_per_verified_completion_microusd: None,
            baseline_cost_per_verified_completion_microusd: None,
            safety_passed: true,
            privacy_passed: true,
            integrity_passed: true,
            independent: true,
            frozen_oracle_version: "oracle-1".to_owned(),
            comparable_control: true,
            canary_complete: true,
        };
        let receipt = evaluate_candidate(
            &candidate,
            &evaluation,
            &ImprovementControllerPolicy::default(),
        );
        let snapshot = build_behavioral_health_snapshot(
            Some(&report()),
            &[drift],
            None,
            Some(&receipt),
            Some("policy-v1"),
            Some(CanaryAssignment {
                cohort: "rust".to_owned(),
                percentage_milli: 100,
                control_present: true,
            }),
            Some("cursor-10"),
            None,
        );
        assert_eq!(snapshot.status, BehavioralHealthStatus::Degraded);
        assert_eq!(snapshot.improvement_state, ImprovementHealthState::Promoted);
        assert_eq!(
            snapshot.regression_source_ids,
            vec!["baseline-1", "outcome-1"]
        );
        assert_eq!(snapshot.active_policy_version.as_deref(), Some("policy-v1"));
        assert_eq!(snapshot.reconciliation_cursor.as_deref(), Some("cursor-10"));
    }

    #[test]
    fn degraded_projection_never_looks_healthy_empty() {
        let snapshot = build_behavioral_health_snapshot(
            None,
            &[],
            None,
            None,
            None,
            None,
            None,
            Some("corrupt projection"),
        );
        assert_eq!(snapshot.status, BehavioralHealthStatus::Degraded);
        assert_eq!(
            snapshot.degraded_reason.as_deref(),
            Some("corrupt projection")
        );
    }

    #[test]
    fn same_inputs_produce_equal_surface_snapshots() {
        let first = build_behavioral_health_snapshot(
            Some(&report()),
            &[],
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let second = build_behavioral_health_snapshot(
            Some(&report()),
            &[],
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(first, second);
    }
}
use serde::{Deserialize, Serialize};

use crate::{
    behavioral_metrics::{
        BehavioralCohortKey, BehavioralCohortReport, BehavioralDriftReport, DriftClassification,
    },
    improvement_controller::{ImprovementDecision, ImprovementReceipt},
    tool_learning::ToolReliabilityReport,
};

pub const BEHAVIORAL_HEALTH_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BehavioralHealthStatus {
    Healthy,
    InsufficientEvidence,
    Degraded,
    Rebuilding,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImprovementHealthState {
    None,
    Evaluating,
    CollectEvidence,
    Canary,
    Promoted,
    Rejected,
    RolledBack,
    Escalated,
}

impl Default for ImprovementHealthState {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CanaryAssignment {
    pub cohort: String,
    pub percentage_milli: u16,
    pub control_present: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolHealthSummary {
    pub tool_id: String,
    pub p50_latency_ms: Option<u64>,
    pub p95_latency_ms: Option<u64>,
    pub p99_latency_ms: Option<u64>,
    pub invalid_output_count: u64,
    pub eligible_verified_contributions: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BehavioralHealthSnapshot {
    pub schema_version: u16,
    pub status: BehavioralHealthStatus,
    pub degraded_reason: Option<String>,
    pub cohort: Option<BehavioralCohortKey>,
    pub classifier_version: Option<u16>,
    pub verified_success_rate_milli: Option<u16>,
    pub first_pass_verified_success_rate_milli: Option<u16>,
    pub cost_per_verified_success_microunits: Option<u64>,
    pub median_latency_millis: Option<u64>,
    pub p95_latency_millis: Option<u64>,
    pub repair_burden_milli: Option<u64>,
    pub sample_count: usize,
    pub uncertainty_milli: Option<u16>,
    pub cost_coverage_milli: Option<u16>,
    pub source_outcome_ids: Vec<String>,
    pub regression_source_ids: Vec<String>,
    pub improvement_source_ids: Vec<String>,
    pub tool: Option<ToolHealthSummary>,
    pub active_policy_version: Option<String>,
    pub canary: Option<CanaryAssignment>,
    pub improvement_state: ImprovementHealthState,
    pub rollback_target: Option<String>,
    pub reconciliation_cursor: Option<String>,
}

#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn build_behavioral_health_snapshot(
    report: Option<&BehavioralCohortReport>,
    drift_reports: &[BehavioralDriftReport],
    tool_report: Option<&ToolReliabilityReport>,
    improvement_receipt: Option<&ImprovementReceipt>,
    active_policy_version: Option<&str>,
    canary: Option<CanaryAssignment>,
    reconciliation_cursor: Option<&str>,
    degraded_reason: Option<&str>,
) -> BehavioralHealthSnapshot {
    let regression_source_ids = source_ids_for(drift_reports, DriftClassification::Regression);
    let improvement_source_ids = source_ids_for(drift_reports, DriftClassification::Improvement);
    let report_status = report.map_or(BehavioralHealthStatus::InsufficientEvidence, |report| {
        if report.sample_count == 0 || report.uncertainty_milli > 500 {
            BehavioralHealthStatus::InsufficientEvidence
        } else if !regression_source_ids.is_empty() {
            BehavioralHealthStatus::Degraded
        } else {
            BehavioralHealthStatus::Healthy
        }
    });
    let status = if degraded_reason.is_some() {
        BehavioralHealthStatus::Degraded
    } else {
        report_status
    };
    let (improvement_state, rollback_target) =
        improvement_receipt.map_or((ImprovementHealthState::None, None), |receipt| {
            (
                map_improvement_state(receipt.decision),
                receipt.rollback_target.clone(),
            )
        });
    let tool = tool_report.map(|report| ToolHealthSummary {
        tool_id: report.tool_id.clone(),
        p50_latency_ms: report.p50_latency_ms,
        p95_latency_ms: report.p95_latency_ms,
        p99_latency_ms: report.p99_latency_ms,
        invalid_output_count: report.invalid_output_count,
        eligible_verified_contributions: report.eligible_verified_contributions,
    });
    BehavioralHealthSnapshot {
        schema_version: BEHAVIORAL_HEALTH_SCHEMA_VERSION,
        status,
        degraded_reason: degraded_reason.map(str::to_owned),
        cohort: report.map(|report| report.key.clone()),
        classifier_version: report.map(|report| report.key.classification_version),
        verified_success_rate_milli: report.and_then(|report| report.verified_success_rate_milli),
        first_pass_verified_success_rate_milli: report
            .and_then(|report| report.first_pass_success_rate_milli),
        cost_per_verified_success_microunits: report
            .and_then(|report| report.cost_per_verified_success_microunits),
        median_latency_millis: report.and_then(|report| report.median_latency_millis),
        p95_latency_millis: report.and_then(|report| report.p95_latency_millis),
        repair_burden_milli: report.and_then(|report| {
            (report.verified_successes > 0).then(|| {
                report.recovery_count.saturating_mul(1_000) / report.verified_successes as u64
            })
        }),
        sample_count: report.map_or(0, |report| report.sample_count),
        uncertainty_milli: report.map(|report| report.uncertainty_milli),
        cost_coverage_milli: report.map(|report| report.cost_coverage_milli),
        source_outcome_ids: report.map_or_else(Vec::new, |report| report.outcome_ids.clone()),
        regression_source_ids,
        improvement_source_ids,
        tool,
        active_policy_version: active_policy_version.map(str::to_owned),
        canary,
        improvement_state,
        rollback_target,
        reconciliation_cursor: reconciliation_cursor.map(str::to_owned),
    }
}

fn source_ids_for(
    reports: &[BehavioralDriftReport],
    classification: DriftClassification,
) -> Vec<String> {
    let mut ids = reports
        .iter()
        .filter(|report| report.classification == classification)
        .flat_map(|report| {
            report
                .baseline_outcome_ids
                .iter()
                .chain(report.current_outcome_ids.iter())
                .cloned()
        })
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn map_improvement_state(decision: ImprovementDecision) -> ImprovementHealthState {
    match decision {
        ImprovementDecision::Evaluating => ImprovementHealthState::Evaluating,
        ImprovementDecision::CollectEvidence => ImprovementHealthState::CollectEvidence,
        ImprovementDecision::Canary => ImprovementHealthState::Canary,
        ImprovementDecision::Promoted => ImprovementHealthState::Promoted,
        ImprovementDecision::Rejected => ImprovementHealthState::Rejected,
        ImprovementDecision::RolledBack => ImprovementHealthState::RolledBack,
        ImprovementDecision::Escalated => ImprovementHealthState::Escalated,
    }
}
