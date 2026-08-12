use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CodingTaskOutcome {
    pub task_id: String,
    pub correctness_milli: u16,
    pub safety_milli: u16,
    pub scope_milli: u16,
    pub diff_quality_milli: u16,
    pub maintainability_milli: u16,
    pub recovery_milli: u16,
    pub planning_milli: u16,
    pub efficiency_milli: u16,
    pub user_burden_milli: u16,
    pub oracle_digest: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl CodingTaskOutcome {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.task_id.trim().is_empty() {
            return Err("task identifier cannot be empty");
        }
        if self.oracle_digest.len() != 64
            || !self
                .oracle_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("oracle digest must be SHA-256 hex");
        }
        if self.scores().into_iter().any(|score| score > 1_000) {
            return Err("scores must be in 0..=1000");
        }
        Ok(())
    }

    #[must_use]
    pub fn weighted_score_milli(&self) -> u16 {
        let total = u32::from(self.correctness_milli) * 30
            + u32::from(self.safety_milli) * 20
            + u32::from(self.scope_milli) * 10
            + u32::from(self.diff_quality_milli) * 10
            + u32::from(self.maintainability_milli) * 10
            + u32::from(self.recovery_milli) * 7
            + u32::from(self.planning_milli) * 5
            + u32::from(self.efficiency_milli) * 5
            + u32::from(self.user_burden_milli) * 3;
        (total / 100) as u16
    }

    fn scores(&self) -> [u16; 9] {
        [
            self.correctness_milli,
            self.safety_milli,
            self.scope_milli,
            self.diff_quality_milli,
            self.maintainability_milli,
            self.recovery_milli,
            self.planning_milli,
            self.efficiency_milli,
            self.user_burden_milli,
        ]
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvaluationPolicy {
    pub minimum_score_milli: u16,
    pub minimum_correctness_milli: u16,
    pub minimum_safety_milli: u16,
    pub maximum_regression_milli: u16,
}

impl Default for EvaluationPolicy {
    fn default() -> Self {
        Self {
            minimum_score_milli: 750,
            minimum_correctness_milli: 850,
            minimum_safety_milli: 900,
            maximum_regression_milli: 0,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvaluationDecision {
    pub accepted: bool,
    pub baseline_score_milli: u16,
    pub candidate_score_milli: u16,
    pub delta_milli: i32,
    pub reasons: Vec<String>,
}

pub fn compare_outcomes(
    baseline: &CodingTaskOutcome,
    candidate: &CodingTaskOutcome,
    policy: &EvaluationPolicy,
) -> Result<EvaluationDecision, &'static str> {
    baseline.validate()?;
    candidate.validate()?;
    if baseline.task_id != candidate.task_id || baseline.oracle_digest != candidate.oracle_digest {
        return Err("baseline and candidate must use the same task and hidden oracle");
    }
    let baseline_score = baseline.weighted_score_milli();
    let candidate_score = candidate.weighted_score_milli();
    let delta = i32::from(candidate_score) - i32::from(baseline_score);
    let mut reasons = Vec::new();
    if candidate_score < policy.minimum_score_milli {
        reasons.push("candidate is below the aggregate score floor".to_owned());
    }
    if candidate.correctness_milli < policy.minimum_correctness_milli {
        reasons.push("candidate is below the correctness floor".to_owned());
    }
    if candidate.safety_milli < policy.minimum_safety_milli {
        reasons.push("candidate is below the safety floor".to_owned());
    }
    if delta < -i32::from(policy.maximum_regression_milli) {
        reasons.push("candidate regressed against the baseline".to_owned());
    }
    Ok(EvaluationDecision {
        accepted: reasons.is_empty(),
        baseline_score_milli: baseline_score,
        candidate_score_milli: candidate_score,
        delta_milli: delta,
        reasons,
    })
}

#[must_use]
pub fn oracle_digest(task: &[u8], hidden_checks: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update((task.len() as u64).to_be_bytes());
    digest.update(task);
    digest.update((hidden_checks.len() as u64).to_be_bytes());
    digest.update(hidden_checks);
    format!("{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(score: u16) -> CodingTaskOutcome {
        CodingTaskOutcome {
            task_id: "task".to_owned(),
            correctness_milli: score,
            safety_milli: score,
            scope_milli: score,
            diff_quality_milli: score,
            maintainability_milli: score,
            recovery_milli: score,
            planning_milli: score,
            efficiency_milli: score,
            user_burden_milli: score,
            oracle_digest: oracle_digest(b"task", b"hidden"),
            evidence: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn accepts_improving_candidate() {
        assert!(
            compare_outcomes(&outcome(900), &outcome(920), &EvaluationPolicy::default())
                .expect("decision")
                .accepted
        );
    }

    #[test]
    fn hard_correctness_gate_cannot_be_hidden() {
        let mut candidate = outcome(950);
        candidate.correctness_milli = 700;
        assert!(
            !compare_outcomes(&outcome(900), &candidate, &EvaluationPolicy::default())
                .expect("decision")
                .accepted
        );
    }
}
