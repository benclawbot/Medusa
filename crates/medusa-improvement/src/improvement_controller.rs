#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta_improvement::MetaImprovementTarget;

    fn candidate(id: &str) -> ImprovementCandidate {
        ImprovementCandidate {
            id: id.to_owned(),
            target: MetaImprovementTarget::PromptOverlay,
            current_policy_version: "policy-v1".to_owned(),
            predecessor_policy_version: "policy-v0".to_owned(),
            source_drift_ids: vec!["drift-1".to_owned()],
            source_outcome_ids: vec!["outcome-1".to_owned()],
            cohort: "rust-debugging".to_owned(),
            frozen_oracle_version: "oracle-v1".to_owned(),
            hypothesized_mechanism: "verified evidence identifies a bounded improvement".to_owned(),
            minimal_change: "apply the candidate policy".to_owned(),
            minimum_samples: 10,
            minimum_effect_milli: 50,
            canary_required: true,
            evaluator_changed: false,
            authority_expanded: false,
            protected_change: false,
        }
    }

    fn evaluation(id: &str) -> IndependentEvaluation {
        IndependentEvaluation {
            evaluation_id: id.to_owned(),
            sample_count: 20,
            effect_milli: 100,
            baseline_verified_success_milli: 800,
            verified_success_milli: 900,
            latency_delta_ms: -10,
            cost_per_verified_completion_microusd: Some(100),
            baseline_cost_per_verified_completion_microusd: Some(120),
            safety_passed: true,
            privacy_passed: true,
            integrity_passed: true,
            independent: true,
            frozen_oracle_version: "oracle-v1".to_owned(),
            comparable_control: true,
            canary_complete: true,
        }
    }

    #[test]
    fn verified_canary_is_promoted_once_and_replay_is_idempotent() {
        let candidate = candidate("candidate-1");
        let evaluation = evaluation("eval-1");
        let policy = ImprovementControllerPolicy::default();
        let first = evaluate_candidate(&candidate, &evaluation, &policy);
        let replay = evaluate_candidate(&candidate, &evaluation, &policy);
        assert_eq!(first.decision, ImprovementDecision::Promoted);
        assert_eq!(first, replay);
        assert_eq!(first.rollback_target.as_deref(), Some("policy-v0"));
    }

    #[test]
    fn latency_improvement_with_success_regression_is_rejected() {
        let candidate = candidate("candidate-2");
        let mut evaluation = evaluation("eval-2");
        evaluation.verified_success_milli = 700;
        evaluation.latency_delta_ms = -500;
        let receipt = evaluate_candidate(
            &candidate,
            &evaluation,
            &ImprovementControllerPolicy::default(),
        );
        assert_eq!(receipt.decision, ImprovementDecision::Rejected);
    }

    #[test]
    fn cheaper_requests_with_worse_verified_completion_cost_are_rejected() {
        let candidate = candidate("candidate-3");
        let mut evaluation = evaluation("eval-3");
        evaluation.cost_per_verified_completion_microusd = Some(200);
        evaluation.baseline_cost_per_verified_completion_microusd = Some(100);
        let receipt = evaluate_candidate(
            &candidate,
            &evaluation,
            &ImprovementControllerPolicy::default(),
        );
        assert_eq!(receipt.decision, ImprovementDecision::Rejected);
    }

    #[test]
    fn safety_violation_rolls_back_immediately_to_exact_predecessor() {
        let candidate = candidate("candidate-4");
        let mut evaluation = evaluation("eval-4");
        evaluation.safety_passed = false;
        evaluation.canary_complete = true;
        let receipt = evaluate_candidate(
            &candidate,
            &evaluation,
            &ImprovementControllerPolicy::default(),
        );
        assert_eq!(receipt.decision, ImprovementDecision::RolledBack);
        assert_eq!(receipt.rollback_target.as_deref(), Some("policy-v0"));
        assert!(receipt.immediate_rollback);
    }

    #[test]
    fn evaluator_or_permission_change_is_forced_to_protected_lane() {
        let mut candidate = candidate("candidate-5");
        candidate.evaluator_changed = true;
        let receipt = evaluate_candidate(
            &candidate,
            &evaluation("eval-5"),
            &ImprovementControllerPolicy::default(),
        );
        assert_eq!(receipt.decision, ImprovementDecision::Escalated);
    }

    #[test]
    fn sparse_or_incomparable_evidence_stays_in_evaluation() {
        let candidate = candidate("candidate-6");
        let mut evaluation = evaluation("eval-6");
        evaluation.sample_count = 2;
        evaluation.comparable_control = false;
        let receipt = evaluate_candidate(
            &candidate,
            &evaluation,
            &ImprovementControllerPolicy::default(),
        );
        assert_eq!(receipt.decision, ImprovementDecision::CollectEvidence);
    }

    #[test]
    fn missing_predecessor_prevents_automatic_promotion() {
        let mut candidate = candidate("candidate-7");
        candidate.predecessor_policy_version.clear();
        let receipt = evaluate_candidate(
            &candidate,
            &evaluation("eval-7"),
            &ImprovementControllerPolicy::default(),
        );
        assert_eq!(receipt.decision, ImprovementDecision::Escalated);
    }

    #[test]
    fn source_linked_trigger_generates_a_bounded_candidate_deterministically() {
        let trigger = ImprovementTrigger {
            id: "drift-42".to_owned(),
            target: MetaImprovementTarget::PromptOverlay,
            current_policy_version: "policy-v1".to_owned(),
            predecessor_policy_version: "policy-v0".to_owned(),
            source_drift_ids: vec!["drift-42".to_owned()],
            source_outcome_ids: vec!["outcome-42".to_owned()],
            cohort: "rust-debugging".to_owned(),
            frozen_oracle_version: "oracle-v1".to_owned(),
            hypothesized_mechanism: "repair loop follows stale prompt guidance".to_owned(),
            minimal_change: "use the verified repair checklist".to_owned(),
            protected_change: false,
        };
        let first = generate_candidate(&trigger).expect("candidate");
        let replay = generate_candidate(&trigger).expect("candidate replay");
        assert_eq!(first, replay);
        assert_eq!(first.source_drift_ids, vec!["drift-42"]);
        assert!(first.canary_required);
    }

    #[test]
    fn experiment_ledger_deduplicates_replayed_receipts() {
        let candidate = candidate("candidate-ledger");
        let evaluation = evaluation("eval-ledger");
        let mut ledger = ImprovementExperimentLedger::default();
        let first = ledger.apply(
            &candidate,
            &evaluation,
            &ImprovementControllerPolicy::default(),
        );
        let replay = ledger.apply(
            &candidate,
            &evaluation,
            &ImprovementControllerPolicy::default(),
        );
        assert_eq!(first, replay);
        assert_eq!(ledger.receipts().len(), 1);
    }
}
// Pure, deterministic gates for the bounded improvement lifecycle.
//
// The controller produces a receipt; activation and rollback remain delegated to
// `MetaImprovementStore` and `RefinementAuthorityStore`. It never changes evaluator, metric,
// permission, or verification authority as part of a candidate decision.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::meta_improvement::MetaImprovementTarget;

pub const IMPROVEMENT_CONTROLLER_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImprovementTrigger {
    pub id: String,
    pub target: MetaImprovementTarget,
    pub current_policy_version: String,
    pub predecessor_policy_version: String,
    pub source_drift_ids: Vec<String>,
    pub source_outcome_ids: Vec<String>,
    pub cohort: String,
    pub frozen_oracle_version: String,
    pub hypothesized_mechanism: String,
    pub minimal_change: String,
    pub protected_change: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImprovementCandidate {
    pub id: String,
    pub target: MetaImprovementTarget,
    pub current_policy_version: String,
    pub predecessor_policy_version: String,
    pub source_drift_ids: Vec<String>,
    pub source_outcome_ids: Vec<String>,
    pub cohort: String,
    pub frozen_oracle_version: String,
    pub hypothesized_mechanism: String,
    pub minimal_change: String,
    pub minimum_samples: u64,
    pub minimum_effect_milli: u16,
    pub canary_required: bool,
    pub evaluator_changed: bool,
    pub authority_expanded: bool,
    pub protected_change: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IndependentEvaluation {
    pub evaluation_id: String,
    pub sample_count: u64,
    pub effect_milli: i16,
    pub baseline_verified_success_milli: u16,
    pub verified_success_milli: u16,
    pub latency_delta_ms: i64,
    pub cost_per_verified_completion_microusd: Option<u64>,
    pub baseline_cost_per_verified_completion_microusd: Option<u64>,
    pub safety_passed: bool,
    pub privacy_passed: bool,
    pub integrity_passed: bool,
    pub independent: bool,
    pub frozen_oracle_version: String,
    pub comparable_control: bool,
    pub canary_complete: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImprovementControllerPolicy {
    pub version: u16,
    pub minimum_samples: u64,
    pub minimum_verified_success_milli: u16,
    pub minimum_effect_milli: u16,
    pub maximum_latency_regression_ms: u64,
    pub maximum_cost_regression_microusd: u64,
}

impl Default for ImprovementControllerPolicy {
    fn default() -> Self {
        Self {
            version: 1,
            minimum_samples: 10,
            minimum_verified_success_milli: 800,
            minimum_effect_milli: 50,
            maximum_latency_regression_ms: 250,
            maximum_cost_regression_microusd: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImprovementDecision {
    Evaluating,
    CollectEvidence,
    Canary,
    Promoted,
    Rejected,
    RolledBack,
    Escalated,
}

/// Converts a source-linked drift trigger into the narrowest deterministic candidate record.
pub fn generate_candidate(trigger: &ImprovementTrigger) -> Result<ImprovementCandidate, String> {
    if trigger.id.trim().is_empty()
        || trigger.current_policy_version.trim().is_empty()
        || trigger.predecessor_policy_version.trim().is_empty()
        || trigger.cohort.trim().is_empty()
        || trigger.frozen_oracle_version.trim().is_empty()
        || trigger.hypothesized_mechanism.trim().is_empty()
        || trigger.minimal_change.trim().is_empty()
        || trigger.source_drift_ids.is_empty()
        || trigger.source_outcome_ids.is_empty()
    {
        return Err("source-linked improvement trigger is incomplete".to_owned());
    }
    Ok(ImprovementCandidate {
        id: format!("candidate:{}", trigger.id),
        target: trigger.target,
        current_policy_version: trigger.current_policy_version.clone(),
        predecessor_policy_version: trigger.predecessor_policy_version.clone(),
        source_drift_ids: trigger.source_drift_ids.clone(),
        source_outcome_ids: trigger.source_outcome_ids.clone(),
        cohort: trigger.cohort.clone(),
        frozen_oracle_version: trigger.frozen_oracle_version.clone(),
        hypothesized_mechanism: trigger.hypothesized_mechanism.clone(),
        minimal_change: trigger.minimal_change.clone(),
        minimum_samples: 10,
        minimum_effect_milli: 50,
        canary_required: true,
        evaluator_changed: false,
        authority_expanded: false,
        protected_change: trigger.protected_change,
    })
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImprovementReceipt {
    pub schema_version: u16,
    pub controller_policy_version: u16,
    pub candidate_id: String,
    pub evaluation_id: String,
    pub decision: ImprovementDecision,
    pub source_drift_ids: Vec<String>,
    pub source_outcome_ids: Vec<String>,
    pub cohort: String,
    pub predecessor_policy_version: Option<String>,
    pub rollback_target: Option<String>,
    pub rationale: String,
    pub idempotency_key: String,
    pub immediate_rollback: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImprovementExperimentLedger {
    receipts: BTreeMap<String, ImprovementReceipt>,
}

impl ImprovementExperimentLedger {
    pub fn apply(
        &mut self,
        candidate: &ImprovementCandidate,
        evaluation: &IndependentEvaluation,
        policy: &ImprovementControllerPolicy,
    ) -> ImprovementReceipt {
        let receipt = evaluate_candidate(candidate, evaluation, policy);
        if let Some(previous) = self.receipts.get(&receipt.idempotency_key) {
            return previous.clone();
        }
        self.receipts
            .insert(receipt.idempotency_key.clone(), receipt.clone());
        receipt
    }

    #[must_use]
    pub fn receipts(&self) -> Vec<ImprovementReceipt> {
        self.receipts.values().cloned().collect()
    }
}

#[must_use]
pub fn evaluate_candidate(
    candidate: &ImprovementCandidate,
    evaluation: &IndependentEvaluation,
    policy: &ImprovementControllerPolicy,
) -> ImprovementReceipt {
    let predecessor = (!candidate.predecessor_policy_version.trim().is_empty())
        .then(|| candidate.predecessor_policy_version.clone());
    let idempotency_key = format!(
        "{}:{}:{}:{}",
        candidate.id,
        candidate.current_policy_version,
        evaluation.evaluation_id,
        evaluation.frozen_oracle_version
    );
    let mut receipt = ImprovementReceipt {
        schema_version: IMPROVEMENT_CONTROLLER_SCHEMA_VERSION,
        controller_policy_version: policy.version,
        candidate_id: candidate.id.clone(),
        evaluation_id: evaluation.evaluation_id.clone(),
        decision: ImprovementDecision::Evaluating,
        source_drift_ids: candidate.source_drift_ids.clone(),
        source_outcome_ids: candidate.source_outcome_ids.clone(),
        cohort: candidate.cohort.clone(),
        predecessor_policy_version: predecessor.clone(),
        rollback_target: None,
        rationale: String::new(),
        idempotency_key,
        immediate_rollback: false,
    };

    if candidate.evaluator_changed || candidate.authority_expanded || candidate.protected_change {
        receipt.decision = ImprovementDecision::Escalated;
        receipt.rationale =
            "candidate changes a protected evaluator, authority, or engineering boundary"
                .to_owned();
        return receipt;
    }
    if predecessor.is_none() {
        receipt.decision = ImprovementDecision::Escalated;
        receipt.rationale =
            "automatic lifecycle requires an exact predecessor rollback target".to_owned();
        return receipt;
    }
    if !candidate.target.runtime_safe() {
        receipt.decision = ImprovementDecision::Escalated;
        receipt.rationale = "target is outside the runtime-safe refinement lane".to_owned();
        return receipt;
    }
    if !evaluation.safety_passed || !evaluation.privacy_passed || !evaluation.integrity_passed {
        receipt.decision = ImprovementDecision::RolledBack;
        receipt.rollback_target = predecessor;
        receipt.immediate_rollback = true;
        receipt.rationale = "safety, privacy, or integrity guardrail failed; exact predecessor required immediately".to_owned();
        return receipt;
    }
    if evaluation.sample_count < policy.minimum_samples.max(candidate.minimum_samples)
        || !evaluation.comparable_control
    {
        receipt.decision = ImprovementDecision::CollectEvidence;
        receipt.rationale =
            "evidence is sparse or incomparable; no promotion decision is authorized".to_owned();
        return receipt;
    }
    if !evaluation.independent
        || evaluation.frozen_oracle_version != candidate.frozen_oracle_version
    {
        receipt.decision = ImprovementDecision::Evaluating;
        receipt.rationale =
            "independent evaluation or frozen oracle binding is incomplete".to_owned();
        return receipt;
    }
    if evaluation.verified_success_milli < policy.minimum_verified_success_milli
        || evaluation.verified_success_milli < evaluation.baseline_verified_success_milli
    {
        receipt.decision = ImprovementDecision::Rejected;
        receipt.rationale =
            "verified-success guardrail regressed or fell below the minimum".to_owned();
        return receipt;
    }
    if evaluation.latency_delta_ms > policy.maximum_latency_regression_ms as i64 {
        receipt.decision = ImprovementDecision::Rejected;
        receipt.rationale = "latency regression exceeded the predeclared guardrail".to_owned();
        return receipt;
    }
    if let (Some(current), Some(baseline)) = (
        evaluation.cost_per_verified_completion_microusd,
        evaluation.baseline_cost_per_verified_completion_microusd,
    ) && current > baseline.saturating_add(policy.maximum_cost_regression_microusd)
    {
        receipt.decision = ImprovementDecision::Rejected;
        receipt.rationale = "cost per independently verified completion regressed".to_owned();
        return receipt;
    }
    let minimum_effect = policy
        .minimum_effect_milli
        .max(candidate.minimum_effect_milli) as i16;
    if evaluation.effect_milli < minimum_effect {
        receipt.decision = ImprovementDecision::Evaluating;
        receipt.rationale = "practical effect threshold has not been met".to_owned();
        return receipt;
    }
    if candidate.canary_required && !evaluation.canary_complete {
        receipt.decision = ImprovementDecision::Canary;
        receipt.rationale =
            "independent offline evidence passed; bounded canary is the next authority-owned stage"
                .to_owned();
        return receipt;
    }
    receipt.decision = ImprovementDecision::Promoted;
    receipt.rollback_target = predecessor;
    receipt.rationale =
        "independent frozen evaluation and bounded canary passed every guardrail".to_owned();
    receipt
}
