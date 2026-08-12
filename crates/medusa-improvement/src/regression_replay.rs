use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::lesson_inference::LessonCandidate;
use crate::solution_selection::{SolutionProposal, SolutionType};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayScenarioKind {
    OriginatingFailure,
    IntendedContext,
    NonApplicableContext,
    CriticalSafety,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplayScenario {
    pub id: String,
    pub kind: ReplayScenarioKind,
    pub input: String,
    pub expected_behavior: String,
    pub candidate_should_trigger: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplayEnvironment {
    pub model: String,
    pub provider: String,
    pub runtime_version: String,
    pub seed: u64,
    pub temperature_milli: u16,
    pub network_enabled: bool,
    pub clock_epoch_ms: u64,
}

impl Default for ReplayEnvironment {
    fn default() -> Self {
        Self {
            model: "deterministic-fixture".to_owned(),
            provider: "local".to_owned(),
            runtime_version: env!("CARGO_PKG_VERSION").to_owned(),
            seed: 0,
            temperature_milli: 0,
            network_enabled: false,
            clock_epoch_ms: 0,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplayBundle {
    pub id: String,
    pub lesson_id: String,
    pub source_signal_ids: Vec<String>,
    pub repository_fixture: String,
    pub tool_capabilities: Vec<String>,
    pub artifact_expectations: Vec<String>,
    pub failure_evidence: String,
    pub scenarios: Vec<ReplayScenario>,
    pub environment: ReplayEnvironment,
    pub redacted: bool,
    pub digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplayObservation {
    pub behavior: String,
    pub candidate_triggered: bool,
    pub critical_safety_passed: bool,
    pub evidence_links: Vec<String>,
    pub metrics: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScenarioComparison {
    pub scenario_id: String,
    pub kind: ReplayScenarioKind,
    pub baseline: ReplayObservation,
    pub candidate: ReplayObservation,
    pub baseline_reproduced_failure: bool,
    pub candidate_resolved_failure: bool,
    pub trigger_correct: bool,
    pub safety_passed: bool,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayDecision {
    Validated,
    Failed,
    WaiverRequired,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplayReport {
    pub bundle_id: String,
    pub lesson_id: String,
    pub decision: ReplayDecision,
    pub comparisons: Vec<ScenarioComparison>,
    pub reviewer_decision: Option<String>,
    pub promotion_blocked: bool,
    pub evidence_links: Vec<String>,
    pub diagnostics: Vec<String>,
}

pub trait ReplayRunner {
    fn run(
        &self,
        scenario: &ReplayScenario,
        candidate: Option<&SolutionProposal>,
        environment: &ReplayEnvironment,
    ) -> ReplayObservation;
}

#[derive(Clone, Debug, Default)]
pub struct ReplayBundleBuilder;

impl ReplayBundleBuilder {
    #[must_use]
    pub fn build(
        &self,
        lesson: &LessonCandidate,
        proposal: &SolutionProposal,
        repository_fixture: &str,
        tool_capabilities: Vec<String>,
    ) -> ReplayBundle {
        let mut scenarios = lesson
            .regression_examples
            .iter()
            .enumerate()
            .map(|(index, example)| ReplayScenario {
                id: format!("origin-{index}"),
                kind: ReplayScenarioKind::OriginatingFailure,
                input: redact(&example.input_summary),
                expected_behavior: redact(&example.expected_behavior),
                candidate_should_trigger: true,
            })
            .collect::<Vec<_>>();

        if scenarios.is_empty() {
            scenarios.push(ReplayScenario {
                id: "origin-0".to_owned(),
                kind: ReplayScenarioKind::OriginatingFailure,
                input: redact(&lesson.observed_pattern),
                expected_behavior: redact(&lesson.generalized_rule),
                candidate_should_trigger: true,
            });
        }

        scenarios.extend(lesson.non_applicable_contexts.iter().enumerate().map(
            |(index, context)| ReplayScenario {
                id: format!("negative-{index}"),
                kind: ReplayScenarioKind::NonApplicableContext,
                input: redact(context),
                expected_behavior: "candidate remains inactive".to_owned(),
                candidate_should_trigger: false,
            },
        ));
        scenarios.push(ReplayScenario {
            id: "critical-safety".to_owned(),
            kind: ReplayScenarioKind::CriticalSafety,
            input: "run existing critical safety behavior".to_owned(),
            expected_behavior: "all critical safety checks pass".to_owned(),
            candidate_should_trigger: false,
        });

        let artifact_expectations = proposal
            .artifacts
            .iter()
            .map(|artifact| artifact.path.clone())
            .collect::<Vec<_>>();
        let failure_evidence = redact(&format!(
            "{}: {}",
            lesson.root_cause, lesson.generalized_rule
        ));
        let mut bundle = ReplayBundle {
            id: format!("replay-{}", lesson.id),
            lesson_id: lesson.id.clone(),
            source_signal_ids: lesson.supporting_signal_ids.clone(),
            repository_fixture: redact(repository_fixture),
            tool_capabilities,
            artifact_expectations,
            failure_evidence,
            scenarios,
            environment: ReplayEnvironment::default(),
            redacted: true,
            digest: String::new(),
        };
        bundle.digest = digest_bundle(&bundle);
        bundle
    }
}

#[derive(Clone, Debug, Default)]
pub struct ReplayValidator;

impl ReplayValidator {
    #[must_use]
    pub fn validate<R: ReplayRunner>(
        &self,
        bundle: &ReplayBundle,
        proposal: &SolutionProposal,
        runner: &R,
    ) -> ReplayReport {
        let deterministic = bundle.environment.seed == 0
            && bundle.environment.temperature_milli == 0
            && !bundle.environment.network_enabled
            && bundle.environment.clock_epoch_ms == 0;
        if !deterministic {
            return ReplayReport {
                bundle_id: bundle.id.clone(),
                lesson_id: bundle.lesson_id.clone(),
                decision: ReplayDecision::WaiverRequired,
                comparisons: Vec::new(),
                reviewer_decision: None,
                promotion_blocked: true,
                evidence_links: Vec::new(),
                diagnostics: vec![
                    "replay environment is nondeterministic; an explicit waiver is required"
                        .to_owned(),
                ],
            };
        }

        let mut comparisons = Vec::new();
        let mut evidence_links = Vec::new();
        for scenario in &bundle.scenarios {
            let baseline = runner.run(scenario, None, &bundle.environment);
            let candidate = runner.run(scenario, Some(proposal), &bundle.environment);
            evidence_links.extend(baseline.evidence_links.iter().cloned());
            evidence_links.extend(candidate.evidence_links.iter().cloned());
            let baseline_reproduced_failure = match scenario.kind {
                ReplayScenarioKind::OriginatingFailure => {
                    baseline.behavior != scenario.expected_behavior
                }
                _ => true,
            };
            let candidate_resolved_failure = match scenario.kind {
                ReplayScenarioKind::OriginatingFailure | ReplayScenarioKind::IntendedContext => {
                    candidate.behavior == scenario.expected_behavior
                }
                _ => true,
            };
            let trigger_correct =
                candidate.candidate_triggered == scenario.candidate_should_trigger;
            let safety_passed = baseline.critical_safety_passed
                && candidate.critical_safety_passed
                && (scenario.kind != ReplayScenarioKind::CriticalSafety
                    || candidate.behavior == scenario.expected_behavior);
            let mut diagnostics = Vec::new();
            if !baseline_reproduced_failure {
                diagnostics
                    .push("originating failure was not reproduced by the baseline".to_owned());
            }
            if !candidate_resolved_failure {
                diagnostics.push("candidate did not produce the expected behavior".to_owned());
            }
            if !trigger_correct {
                diagnostics
                    .push("candidate trigger behavior did not match scenario intent".to_owned());
            }
            if !safety_passed {
                diagnostics.push("critical safety behavior regressed".to_owned());
            }
            comparisons.push(ScenarioComparison {
                scenario_id: scenario.id.clone(),
                kind: scenario.kind,
                baseline,
                candidate,
                baseline_reproduced_failure,
                candidate_resolved_failure,
                trigger_correct,
                safety_passed,
                diagnostics,
            });
        }

        evidence_links.sort();
        evidence_links.dedup();
        let passed = comparisons.iter().all(|comparison| {
            comparison.baseline_reproduced_failure
                && comparison.candidate_resolved_failure
                && comparison.trigger_correct
                && comparison.safety_passed
        });
        let diagnostics = comparisons
            .iter()
            .flat_map(|comparison| {
                comparison
                    .diagnostics
                    .iter()
                    .map(move |message| format!("{}: {message}", comparison.scenario_id))
            })
            .collect::<Vec<_>>();
        ReplayReport {
            bundle_id: bundle.id.clone(),
            lesson_id: bundle.lesson_id.clone(),
            decision: if passed {
                ReplayDecision::Validated
            } else {
                ReplayDecision::Failed
            },
            comparisons,
            reviewer_decision: None,
            promotion_blocked: !passed,
            evidence_links,
            diagnostics,
        }
    }
}

#[must_use]
pub fn supports_solution(solution_type: SolutionType) -> bool {
    matches!(
        solution_type,
        SolutionType::UserPreference
            | SolutionType::RepositoryMemory
            | SolutionType::ReusableSkill
            | SolutionType::HarnessPolicy
            | SolutionType::WorkflowGate
            | SolutionType::RegressionFixture
            | SolutionType::ConfigurationChange
            | SolutionType::ProductCodeChange
    )
}

fn redact(value: &str) -> String {
    value
        .split_whitespace()
        .map(|token| {
            let lower = token.to_ascii_lowercase();
            if lower.contains("token=")
                || lower.contains("password=")
                || lower.starts_with("sk-")
                || lower.starts_with("bearer")
            {
                "[REDACTED]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn digest_bundle(bundle: &ReplayBundle) -> String {
    let mut clone = bundle.clone();
    clone.digest.clear();
    let bytes = serde_json::to_vec(&clone).expect("serializable replay bundle");
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lesson_inference::{ImplementationType, LessonScope, RegressionExample};
    use crate::solution_selection::{ReviewStrength, SolutionSelector};

    fn lesson() -> LessonCandidate {
        LessonCandidate {
            id: "completeness".to_owned(),
            observed_pattern: "claimed completeness before inventorying sources".to_owned(),
            root_cause: "authoritative sources were not mapped".to_owned(),
            generalized_rule: "inventory authoritative sources before claiming completeness"
                .to_owned(),
            scope: LessonScope::DomainGeneral,
            non_applicable_contexts: vec!["ordinary non-comprehensive rewrite".to_owned()],
            confidence_milli: 900,
            uncertainty: Vec::new(),
            supporting_signal_ids: vec!["signal-1".to_owned()],
            contradictory_signal_ids: Vec::new(),
            implementation_type: ImplementationType::Skill,
            regression_examples: vec![RegressionExample {
                source_signal_id: "signal-1".to_owned(),
                input_summary: "create a complete test plan token=secret".to_owned(),
                expected_behavior: "inventory authoritative sources before claiming completeness"
                    .to_owned(),
            }],
            rationale: "evidence-backed".to_owned(),
            promotion_blocked: false,
        }
    }

    #[derive(Default)]
    struct FixtureRunner {
        candidate_fixes_origin: bool,
        candidate_overtriggers: bool,
        safety_passes: bool,
    }

    impl ReplayRunner for FixtureRunner {
        fn run(
            &self,
            scenario: &ReplayScenario,
            candidate: Option<&SolutionProposal>,
            _environment: &ReplayEnvironment,
        ) -> ReplayObservation {
            let is_candidate = candidate.is_some();
            let behavior = match (scenario.kind, is_candidate) {
                (ReplayScenarioKind::OriginatingFailure, false) => "incomplete plan".to_owned(),
                (ReplayScenarioKind::OriginatingFailure, true) if self.candidate_fixes_origin => {
                    scenario.expected_behavior.clone()
                }
                (ReplayScenarioKind::NonApplicableContext, _) => {
                    "candidate remains inactive".to_owned()
                }
                (ReplayScenarioKind::CriticalSafety, _) => scenario.expected_behavior.clone(),
                _ => "unchanged".to_owned(),
            };
            ReplayObservation {
                behavior,
                candidate_triggered: is_candidate
                    && match scenario.kind {
                        ReplayScenarioKind::OriginatingFailure => true,
                        ReplayScenarioKind::NonApplicableContext => self.candidate_overtriggers,
                        ReplayScenarioKind::CriticalSafety => false,
                        ReplayScenarioKind::IntendedContext => true,
                    },
                critical_safety_passed: self.safety_passes,
                evidence_links: vec![format!("evidence://{}", scenario.id)],
                metrics: BTreeMap::from([("steps".to_owned(), 1)]),
            }
        }
    }

    #[test]
    fn bundle_redacts_sensitive_values_and_is_stable() {
        let lesson = lesson();
        let proposal = SolutionSelector.propose(&lesson);
        let builder = ReplayBundleBuilder;
        let bundle = builder.build(
            &lesson,
            &proposal,
            "fixture password=hunter2",
            vec!["read".to_owned(), "write".to_owned()],
        );
        assert!(bundle.redacted);
        assert!(!serde_json::to_string(&bundle).unwrap().contains("hunter2"));
        assert!(!serde_json::to_string(&bundle).unwrap().contains("secret"));
        assert_eq!(bundle.digest, digest_bundle(&bundle));
        assert!(
            bundle
                .scenarios
                .iter()
                .any(|scenario| scenario.kind == ReplayScenarioKind::NonApplicableContext)
        );
    }

    #[test]
    fn validation_requires_baseline_reproduction_resolution_and_no_overtriggering() {
        let lesson = lesson();
        let proposal = SolutionSelector.propose(&lesson);
        let bundle = ReplayBundleBuilder.build(&lesson, &proposal, "repo", vec!["read".to_owned()]);
        let report = ReplayValidator.validate(
            &bundle,
            &proposal,
            &FixtureRunner {
                candidate_fixes_origin: true,
                candidate_overtriggers: false,
                safety_passes: true,
            },
        );
        assert_eq!(report.decision, ReplayDecision::Validated);
        assert!(!report.promotion_blocked);
        assert!(!report.evidence_links.is_empty());
    }

    #[test]
    fn failed_candidate_remains_blocked_with_diagnostics() {
        let lesson = lesson();
        let proposal = SolutionSelector.propose(&lesson);
        let bundle = ReplayBundleBuilder.build(&lesson, &proposal, "repo", Vec::new());
        let report = ReplayValidator.validate(
            &bundle,
            &proposal,
            &FixtureRunner {
                candidate_fixes_origin: false,
                candidate_overtriggers: true,
                safety_passes: false,
            },
        );
        assert_eq!(report.decision, ReplayDecision::Failed);
        assert!(report.promotion_blocked);
        assert!(!report.diagnostics.is_empty());
    }

    #[test]
    fn nondeterministic_environment_requires_explicit_waiver() {
        let lesson = lesson();
        let proposal = SolutionProposal {
            lesson_id: lesson.id.clone(),
            source_signal_ids: Vec::new(),
            selected: vec![SolutionType::ReusableSkill],
            alternatives: Vec::new(),
            rationale: String::new(),
            review_strength: ReviewStrength::Elevated,
            isolated: true,
            editable: true,
            artifacts: Vec::new(),
            activation_blocked: true,
        };
        let mut bundle = ReplayBundleBuilder.build(&lesson, &proposal, "repo", Vec::new());
        bundle.environment.network_enabled = true;
        let report = ReplayValidator.validate(&bundle, &proposal, &FixtureRunner::default());
        assert_eq!(report.decision, ReplayDecision::WaiverRequired);
        assert!(report.promotion_blocked);
    }

    #[test]
    fn all_durable_solution_classes_are_supported() {
        for solution_type in [
            SolutionType::UserPreference,
            SolutionType::RepositoryMemory,
            SolutionType::ReusableSkill,
            SolutionType::HarnessPolicy,
            SolutionType::WorkflowGate,
            SolutionType::RegressionFixture,
            SolutionType::ConfigurationChange,
            SolutionType::ProductCodeChange,
        ] {
            assert!(supports_solution(solution_type));
        }
        assert!(!supports_solution(SolutionType::NoPersistence));
    }
}
