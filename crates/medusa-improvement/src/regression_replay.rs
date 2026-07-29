use serde::{Deserialize, Serialize};

use crate::solution_selection::{SolutionProposal, SolutionType};

const REDACTED: &str = "[REDACTED]";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplayRuntime {
    pub model: String,
    pub provider: String,
    pub runtime: String,
    pub seed: u64,
    pub temperature_milli: u16,
    pub network_enabled: bool,
    pub live_provider: bool,
}

impl ReplayRuntime {
    #[must_use]
    pub fn deterministic(model: impl Into<String>, runtime: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            provider: "fixture".to_owned(),
            runtime: runtime.into(),
            seed: 0,
            temperature_milli: 0,
            network_enabled: false,
            live_provider: false,
        }
    }

    #[must_use]
    pub fn is_deterministic(&self) -> bool {
        self.seed == 0
            && self.temperature_milli == 0
            && !self.network_enabled
            && !self.live_provider
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayScenarioKind {
    OriginatingFailure,
    IntendedContext,
    NonApplicableContext,
    CriticalBehavior,
    Safety,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplayScenario {
    pub id: String,
    pub kind: ReplayScenarioKind,
    pub request: String,
    pub relevant_turns: Vec<String>,
    pub repository_fixture: String,
    pub tool_capabilities: Vec<String>,
    pub expected_artifacts: Vec<String>,
    pub failure_evidence: Vec<String>,
    pub candidate_should_trigger: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplayBundle {
    pub id: String,
    pub lesson_id: String,
    pub candidate_types: Vec<SolutionType>,
    pub runtime: ReplayRuntime,
    pub scenarios: Vec<ReplayScenario>,
    pub waiver: Option<DeterminismWaiver>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeterminismWaiver {
    pub reason: String,
    pub reviewer: String,
    pub bounded_live_test: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScenarioObservation {
    pub scenario_id: String,
    pub baseline_reproduced_failure: bool,
    pub candidate_resolved_failure: bool,
    pub candidate_triggered: bool,
    pub critical_behavior_passed: bool,
    pub safety_passed: bool,
    pub evidence_links: Vec<String>,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplayComparison {
    pub bundle_id: String,
    pub lesson_id: String,
    pub baseline_failures: usize,
    pub candidate_failures: usize,
    pub checks: Vec<ReplayCheck>,
    pub observations: Vec<ScenarioObservation>,
    pub promotion_blocked: bool,
    pub reviewer_decision: ReviewerDecision,
    pub summary: ReplaySummary,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplaySummary {
    pub title: String,
    pub baseline: String,
    pub candidate: String,
    pub evidence_links: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerDecision {
    Pending,
    Accept,
    Revise,
    Waived,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplayCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Default)]
pub struct RegressionReplayValidator;

impl RegressionReplayValidator {
    #[must_use]
    pub fn bundle_for(
        &self,
        proposal: &SolutionProposal,
        runtime: ReplayRuntime,
        scenarios: Vec<ReplayScenario>,
    ) -> ReplayBundle {
        ReplayBundle {
            id: format!("replay-{}", proposal.lesson_id),
            lesson_id: proposal.lesson_id.clone(),
            candidate_types: proposal.selected.clone(),
            runtime,
            scenarios: scenarios.into_iter().map(redact_scenario).collect(),
            waiver: None,
        }
    }

    #[must_use]
    pub fn validate(
        &self,
        bundle: &ReplayBundle,
        observations: Vec<ScenarioObservation>,
    ) -> ReplayComparison {
        let originating = observations_for(bundle, &observations, ReplayScenarioKind::OriginatingFailure);
        let intended = observations_for(bundle, &observations, ReplayScenarioKind::IntendedContext);
        let excluded = observations_for(bundle, &observations, ReplayScenarioKind::NonApplicableContext);
        let critical = observations_for(bundle, &observations, ReplayScenarioKind::CriticalBehavior);
        let safety = observations_for(bundle, &observations, ReplayScenarioKind::Safety);

        let deterministic = bundle.runtime.is_deterministic() || bundle.waiver.is_some();
        let baseline_reproduced = !originating.is_empty()
            && originating
                .iter()
                .all(|item| item.baseline_reproduced_failure);
        let candidate_fixed = !originating.is_empty()
            && originating
                .iter()
                .all(|item| item.candidate_resolved_failure);
        let intended_triggered = intended
            .iter()
            .all(|item| item.candidate_triggered);
        let exclusions_respected = excluded
            .iter()
            .all(|item| !item.candidate_triggered);
        let critical_passed = critical
            .iter()
            .all(|item| item.critical_behavior_passed);
        let safety_passed = safety.iter().all(|item| item.safety_passed);
        let private_data_absent = bundle_is_redacted(bundle);

        let checks = vec![
            check("deterministic runtime or explicit waiver", deterministic),
            check("originating failure reproduced", baseline_reproduced),
            check("candidate resolves originating failure", candidate_fixed),
            check("candidate triggers in intended contexts", intended_triggered),
            check("candidate stays inactive in excluded contexts", exclusions_respected),
            check("critical behavior remains intact", critical_passed),
            check("safety scenarios pass", safety_passed),
            check("fixtures and logs contain no sensitive material", private_data_absent),
        ];
        let promotion_blocked = checks.iter().any(|item| !item.passed);
        let evidence_links = observations
            .iter()
            .flat_map(|item| item.evidence_links.iter().cloned())
            .collect::<Vec<_>>();
        let baseline_failures = observations
            .iter()
            .filter(|item| item.baseline_reproduced_failure)
            .count();
        let candidate_failures = observations
            .iter()
            .filter(|item| !item.candidate_resolved_failure && item.baseline_reproduced_failure)
            .count();

        ReplayComparison {
            bundle_id: bundle.id.clone(),
            lesson_id: bundle.lesson_id.clone(),
            baseline_failures,
            candidate_failures,
            checks,
            observations,
            promotion_blocked,
            reviewer_decision: if bundle.waiver.is_some() {
                ReviewerDecision::Waived
            } else if promotion_blocked {
                ReviewerDecision::Revise
            } else {
                ReviewerDecision::Pending
            },
            summary: ReplaySummary {
                title: format!("Regression replay for {}", bundle.lesson_id),
                baseline: format!("{baseline_failures} reproduced failure(s)"),
                candidate: format!("{candidate_failures} remaining failure(s)"),
                evidence_links,
            },
        }
    }
}

fn observations_for<'a>(
    bundle: &ReplayBundle,
    observations: &'a [ScenarioObservation],
    kind: ReplayScenarioKind,
) -> Vec<&'a ScenarioObservation> {
    bundle
        .scenarios
        .iter()
        .filter(|scenario| scenario.kind == kind)
        .filter_map(|scenario| {
            observations
                .iter()
                .find(|item| item.scenario_id == scenario.id)
        })
        .collect()
}

fn check(name: &str, passed: bool) -> ReplayCheck {
    ReplayCheck {
        name: name.to_owned(),
        passed,
        detail: if passed {
            "passed".to_owned()
        } else {
            "failed; candidate remains inactive and diagnostics are retained".to_owned()
        },
    }
}

fn redact_scenario(mut scenario: ReplayScenario) -> ReplayScenario {
    scenario.request = redact(&scenario.request);
    scenario.relevant_turns = scenario
        .relevant_turns
        .into_iter()
        .map(|value| redact(&value))
        .collect();
    scenario.failure_evidence = scenario
        .failure_evidence
        .into_iter()
        .map(|value| redact(&value))
        .collect();
    scenario
}

fn redact(value: &str) -> String {
    value
        .split_whitespace()
        .map(|token| {
            let lower = token.to_ascii_lowercase();
            if lower.contains("token=")
                || lower.contains("password=")
                || lower.contains("secret=")
                || lower.starts_with("sk-")
                || lower.contains('@')
            {
                REDACTED
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn bundle_is_redacted(bundle: &ReplayBundle) -> bool {
    bundle.scenarios.iter().all(|scenario| {
        [&scenario.request, &scenario.repository_fixture]
            .into_iter()
            .chain(scenario.relevant_turns.iter())
            .chain(scenario.failure_evidence.iter())
            .all(|value| !contains_sensitive(value))
    })
}

fn contains_sensitive(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("token=")
        || lower.contains("password=")
        || lower.contains("secret=")
        || lower.contains("sk-")
        || value
            .split_whitespace()
            .any(|token| token.contains('@') && token != REDACTED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solution_selection::{ReviewStrength, SolutionProposal};

    fn proposal() -> SolutionProposal {
        SolutionProposal {
            lesson_id: "complete-plans".to_owned(),
            source_signal_ids: vec!["signal-1".to_owned()],
            selected: vec![SolutionType::ReusableSkill, SolutionType::RegressionFixture],
            alternatives: Vec::new(),
            rationale: "test completeness across contexts".to_owned(),
            review_strength: ReviewStrength::Elevated,
            isolated: true,
            editable: true,
            artifacts: Vec::new(),
            activation_blocked: true,
        }
    }

    fn scenario(id: &str, kind: ReplayScenarioKind, should_trigger: bool) -> ReplayScenario {
        ReplayScenario {
            id: id.to_owned(),
            kind,
            request: "build a complete test plan token=private".to_owned(),
            relevant_turns: vec!["contact person@example.com".to_owned()],
            repository_fixture: "fixtures/repository".to_owned(),
            tool_capabilities: vec!["repository search".to_owned()],
            expected_artifacts: vec!["complete plan".to_owned()],
            failure_evidence: vec!["secret=hidden omitted scenario".to_owned()],
            candidate_should_trigger: should_trigger,
        }
    }

    fn observation(
        id: &str,
        baseline_failed: bool,
        fixed: bool,
        triggered: bool,
    ) -> ScenarioObservation {
        ScenarioObservation {
            scenario_id: id.to_owned(),
            baseline_reproduced_failure: baseline_failed,
            candidate_resolved_failure: fixed,
            candidate_triggered: triggered,
            critical_behavior_passed: true,
            safety_passed: true,
            evidence_links: vec![format!("evidence/{id}.json")],
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn completeness_skill_passes_original_adjacent_and_ordinary_scenarios() {
        let validator = RegressionReplayValidator;
        let bundle = validator.bundle_for(
            &proposal(),
            ReplayRuntime::deterministic("fixture-model", "medusa-replay-v1"),
            vec![
                scenario("original-test-plan", ReplayScenarioKind::OriginatingFailure, true),
                scenario("adjacent-audit", ReplayScenarioKind::IntendedContext, true),
                scenario("ordinary-edit", ReplayScenarioKind::NonApplicableContext, false),
                scenario("critical", ReplayScenarioKind::CriticalBehavior, false),
                scenario("safety", ReplayScenarioKind::Safety, false),
            ],
        );
        let comparison = validator.validate(
            &bundle,
            vec![
                observation("original-test-plan", true, true, true),
                observation("adjacent-audit", false, true, true),
                observation("ordinary-edit", false, true, false),
                observation("critical", false, true, false),
                observation("safety", false, true, false),
            ],
        );

        assert!(!comparison.promotion_blocked);
        assert_eq!(comparison.baseline_failures, 1);
        assert_eq!(comparison.candidate_failures, 0);
        assert!(bundle.scenarios[0].request.contains(REDACTED));
        assert!(bundle.scenarios[0].relevant_turns[0].contains(REDACTED));
    }

    #[test]
    fn missing_baseline_reproduction_blocks_skill_or_policy_promotion() {
        let validator = RegressionReplayValidator;
        let bundle = validator.bundle_for(
            &proposal(),
            ReplayRuntime::deterministic("fixture-model", "medusa-replay-v1"),
            vec![scenario(
                "original-test-plan",
                ReplayScenarioKind::OriginatingFailure,
                true,
            )],
        );
        let comparison = validator.validate(
            &bundle,
            vec![observation("original-test-plan", false, true, true)],
        );

        assert!(comparison.promotion_blocked);
        assert_eq!(comparison.reviewer_decision, ReviewerDecision::Revise);
        assert!(comparison
            .checks
            .iter()
            .any(|item| item.name.contains("reproduced") && !item.passed));
    }

    #[test]
    fn nondeterministic_runs_require_an_explicit_bounded_waiver() {
        let validator = RegressionReplayValidator;
        let mut runtime = ReplayRuntime::deterministic("live-model", "provider-runtime");
        runtime.live_provider = true;
        runtime.network_enabled = true;
        let mut bundle = validator.bundle_for(
            &proposal(),
            runtime,
            vec![scenario(
                "original-test-plan",
                ReplayScenarioKind::OriginatingFailure,
                true,
            )],
        );
        let observations = vec![observation("original-test-plan", true, true, true)];
        assert!(validator.validate(&bundle, observations.clone()).promotion_blocked);

        bundle.waiver = Some(DeterminismWaiver {
            reason: "provider behavior cannot be reproduced locally".to_owned(),
            reviewer: "human-reviewer".to_owned(),
            bounded_live_test: true,
        });
        let comparison = validator.validate(&bundle, observations);
        assert_eq!(comparison.reviewer_decision, ReviewerDecision::Waived);
    }
}
