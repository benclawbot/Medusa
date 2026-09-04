use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::correction_signals::{CandidateScope, LearningSignal, LearningSignalKind};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonScope {
    Task,
    Repository,
    ProjectWorkflow,
    User,
    DomainGeneral,
    Unresolved,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImplementationType {
    Memory,
    Skill,
    PromptRule,
    RepositoryConvention,
    TestOrEval,
    Unresolved,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RegressionExample {
    pub source_signal_id: String,
    pub input_summary: String,
    pub expected_behavior: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LessonCandidate {
    pub id: String,
    pub observed_pattern: String,
    pub root_cause: String,
    pub generalized_rule: String,
    pub scope: LessonScope,
    pub non_applicable_contexts: Vec<String>,
    pub confidence_milli: u16,
    pub uncertainty: Vec<String>,
    pub supporting_signal_ids: Vec<String>,
    pub contradictory_signal_ids: Vec<String>,
    pub implementation_type: ImplementationType,
    pub regression_examples: Vec<RegressionExample>,
    pub rationale: String,
    pub promotion_blocked: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct LessonInferenceBatch {
    pub candidates: Vec<LessonCandidate>,
    pub ignored_signal_ids: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct LessonInferenceEngine;

impl LessonInferenceEngine {
    /// Drop candidates whose generalized rule already exists in `known_rules`
    /// (normalized, case-insensitive). Prevents duplicate lessons from repeated
    /// failures ever reaching review; merged evidence is the caller's job.
    #[must_use]
    pub fn dedupe_against_known(
        batch: LessonInferenceBatch,
        known_rules: &[String],
    ) -> LessonInferenceBatch {
        let known: Vec<String> = known_rules
            .iter()
            .map(|rule| rule.trim().to_lowercase())
            .collect();
        let mut candidates = Vec::new();
        let mut ignored_signal_ids = batch.ignored_signal_ids;
        for candidate in batch.candidates {
            let normalized = candidate.generalized_rule.trim().to_lowercase();
            if known.iter().any(|rule| rule == &normalized) {
                ignored_signal_ids.extend(candidate.supporting_signal_ids.clone());
            } else {
                candidates.push(candidate);
            }
        }
        LessonInferenceBatch {
            candidates,
            ignored_signal_ids,
        }
    }

    #[must_use]
    pub fn infer(&self, signals: &[LearningSignal]) -> LessonInferenceBatch {
        let mut grouped = BTreeMap::<String, LessonCandidate>::new();
        let mut ignored_signal_ids = Vec::new();

        for signal in signals {
            if is_temporary_local_fact(signal) {
                ignored_signal_ids.push(signal.id.clone());
                continue;
            }

            let interpretations = interpretations(signal);
            for interpretation in interpretations {
                let key = format!(
                    "{:?}:{}",
                    interpretation.scope, interpretation.generalized_rule
                );
                let entry = grouped.entry(key).or_insert_with(|| LessonCandidate {
                    id: format!(
                        "lesson-{}",
                        stable_suffix(&signal.id, &interpretation.generalized_rule)
                    ),
                    observed_pattern: interpretation.observed_pattern.clone(),
                    root_cause: interpretation.root_cause.clone(),
                    generalized_rule: interpretation.generalized_rule.clone(),
                    scope: interpretation.scope,
                    non_applicable_contexts: interpretation.non_applicable_contexts.clone(),
                    confidence_milli: interpretation.confidence_milli,
                    uncertainty: interpretation.uncertainty.clone(),
                    supporting_signal_ids: Vec::new(),
                    contradictory_signal_ids: Vec::new(),
                    implementation_type: interpretation.implementation_type,
                    regression_examples: Vec::new(),
                    rationale: interpretation.rationale.clone(),
                    promotion_blocked: false,
                });

                push_unique(&mut entry.supporting_signal_ids, signal.id.clone());
                entry.regression_examples.push(RegressionExample {
                    source_signal_id: signal.id.clone(),
                    input_summary: concise(
                        signal.user_correction.as_deref().unwrap_or("correction"),
                    ),
                    expected_behavior: interpretation.generalized_rule.clone(),
                });

                for contradicted in &signal.contradicted_by {
                    push_unique(&mut entry.contradictory_signal_ids, contradicted.clone());
                }
                if !signal.contradicted_by.is_empty() || !signal.ambiguity.is_empty() {
                    entry.confidence_milli = entry.confidence_milli.saturating_sub(200);
                    entry.promotion_blocked = true;
                    push_unique(
                        &mut entry.uncertainty,
                        "source evidence is ambiguous or contradictory".to_owned(),
                    );
                }
            }
        }

        let mut candidates = grouped.into_values().collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.id.cmp(&right.id));
        ignored_signal_ids.sort();
        LessonInferenceBatch {
            candidates,
            ignored_signal_ids,
        }
    }
}

struct Interpretation {
    observed_pattern: String,
    root_cause: String,
    generalized_rule: String,
    scope: LessonScope,
    non_applicable_contexts: Vec<String>,
    confidence_milli: u16,
    uncertainty: Vec<String>,
    implementation_type: ImplementationType,
    rationale: String,
}

fn interpretations(signal: &LearningSignal) -> Vec<Interpretation> {
    let correction = signal.user_correction.as_deref().unwrap_or_default();
    let lower = correction.to_ascii_lowercase();

    if signal.kind == LearningSignalKind::Omission
        && contains_any(
            &lower,
            &["complete", "coverage", "missed", "include", "history"],
        )
    {
        return vec![Interpretation {
            observed_pattern: "an output claimed or implied completeness while authoritative inputs were not fully inventoried".to_owned(),
            root_cause: "the workflow generated the deliverable before mapping all authoritative sources to required output coverage".to_owned(),
            generalized_rule: "before claiming comprehensive coverage, inventory authoritative sources and map every relevant item to the output or an explicit exclusion".to_owned(),
            scope: LessonScope::DomainGeneral,
            non_applicable_contexts: vec![
                "explicitly exploratory or intentionally partial drafts".to_owned(),
                "tasks where the user requests a bounded sample".to_owned(),
            ],
            confidence_milli: 850,
            uncertainty: Vec::new(),
            implementation_type: ImplementationType::Skill,
            rationale: "The correction identifies missing coverage; the reusable failure is the absence of a completeness gate, not the omitted item itself.".to_owned(),
        }];
    }

    if signal.kind == LearningSignalKind::Preference {
        return vec![Interpretation {
            observed_pattern: "the user expressed a stable interaction preference".to_owned(),
            root_cause: "the active behavior did not match the user's preferred working style".to_owned(),
            generalized_rule: concise(correction),
            scope: LessonScope::User,
            non_applicable_contexts: vec![
                "higher-priority safety or policy requirements".to_owned(),
                "situations where the preference would make the task impossible".to_owned(),
            ],
            confidence_milli: 800,
            uncertainty: Vec::new(),
            implementation_type: ImplementationType::Memory,
            rationale: "Preference language is evidence about the user's desired interaction style, not an engineering policy.".to_owned(),
        }];
    }

    if signal.kind == LearningSignalKind::WorkflowFailure {
        return vec![Interpretation {
            observed_pattern: "the task sequence caused avoidable rework or an unsupported claim".to_owned(),
            root_cause: "a required verification or dependency-resolution step occurred too late".to_owned(),
            generalized_rule: "perform prerequisite discovery and verification before making commitments or presenting completion".to_owned(),
            scope: LessonScope::ProjectWorkflow,
            non_applicable_contexts: vec!["low-risk brainstorming that is clearly labelled provisional".to_owned()],
            confidence_milli: 780,
            uncertainty: Vec::new(),
            implementation_type: ImplementationType::Skill,
            rationale: "The correction concerns ordering and evidence, so it should become a workflow procedure rather than a remembered phrase.".to_owned(),
        }];
    }

    if signal.kind == LearningSignalKind::UnjustifiedClaim {
        return vec![Interpretation {
            observed_pattern: "a factual or completion claim exceeded the available evidence".to_owned(),
            root_cause: "the response did not bind claim strength to verification strength".to_owned(),
            generalized_rule: "state completion or certainty only to the level supported by checked evidence, and identify what remains unverified".to_owned(),
            scope: LessonScope::DomainGeneral,
            non_applicable_contexts: vec!["clearly marked hypothetical reasoning".to_owned()],
            confidence_milli: 850,
            uncertainty: Vec::new(),
            implementation_type: ImplementationType::PromptRule,
            rationale: "The durable lesson is evidence-calibrated communication, not the specific disputed claim.".to_owned(),
        }];
    }

    let scope = map_scope(signal.candidate_scope);
    vec![Interpretation {
        observed_pattern: concise(&signal.observed_behavior),
        root_cause: "the available evidence supports a correction but not a more specific causal mechanism".to_owned(),
        generalized_rule: signal
            .requested_outcome
            .as_deref()
            .map(concise)
            .unwrap_or_else(|| concise(correction)),
        scope,
        non_applicable_contexts: vec!["contexts outside the inferred scope".to_owned()],
        confidence_milli: signal.confidence_milli.saturating_sub(100),
        uncertainty: vec!["root cause requires additional independent evidence".to_owned()],
        implementation_type: implementation_for_scope(scope),
        rationale: "The candidate preserves the explicit correction while avoiding unsupported abstraction.".to_owned(),
    }]
}

fn map_scope(scope: CandidateScope) -> LessonScope {
    match scope {
        CandidateScope::Task => LessonScope::Task,
        CandidateScope::Repository => LessonScope::Repository,
        CandidateScope::User => LessonScope::User,
        CandidateScope::Unresolved => LessonScope::Unresolved,
    }
}

fn implementation_for_scope(scope: LessonScope) -> ImplementationType {
    match scope {
        LessonScope::User => ImplementationType::Memory,
        LessonScope::Repository => ImplementationType::RepositoryConvention,
        LessonScope::ProjectWorkflow | LessonScope::DomainGeneral => ImplementationType::Skill,
        LessonScope::Task => ImplementationType::TestOrEval,
        LessonScope::Unresolved => ImplementationType::Unresolved,
    }
}

fn is_temporary_local_fact(signal: &LearningSignal) -> bool {
    if signal.candidate_scope != CandidateScope::Task {
        return false;
    }
    let text = signal
        .user_correction
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    contains_any(
        &text,
        &[
            "branch ",
            "filename",
            ".zip",
            ".patch",
            "commit ",
            "today",
            "this time",
        ],
    )
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn concise(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect()
}

fn stable_suffix(left: &str, right: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in left.bytes().chain(right.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::correction_signals::{EvidenceReference, RedactionStatus};

    #[test]
    fn dedupe_drops_candidates_with_known_rules() {
        let engine = LessonInferenceEngine;
        let batch = engine.infer(&[signal(
            "s1",
            LearningSignalKind::ExplicitCorrection,
            "always run cargo test before pushing",
            CandidateScope::Task,
        )]);
        assert!(!batch.candidates.is_empty());
        let known: Vec<String> = batch
            .candidates
            .iter()
            .map(|candidate| candidate.generalized_rule.clone())
            .collect();
        let deduped = LessonInferenceEngine::dedupe_against_known(batch, &known);
        assert!(deduped.candidates.is_empty());
    }

    fn signal(
        id: &str,
        kind: LearningSignalKind,
        text: &str,
        scope: CandidateScope,
    ) -> LearningSignal {
        LearningSignal {
            id: id.to_owned(),
            kind,
            source_turns: vec![format!("turn-{id}")],
            task_id: Some("task-1".to_owned()),
            observed_behavior: "assistant produced an incomplete test plan".to_owned(),
            user_correction: Some(text.to_owned()),
            requested_outcome: None,
            candidate_scope: scope,
            confidence_milli: 850,
            ambiguity: Vec::new(),
            evidence: vec![EvidenceReference {
                turn_id: format!("turn-{id}"),
                excerpt_digest: "sha256:test".to_owned(),
            }],
            redaction_status: RedactionStatus::NotRequired,
            contradicted_by: Vec::new(),
        }
    }

    #[test]
    fn generalizes_repository_omission_into_completeness_procedure() {
        let batch = LessonInferenceEngine.infer(&[signal(
            "one",
            LearningSignalKind::Omission,
            "You missed commit history coverage, so the plan was not complete.",
            CandidateScope::Repository,
        )]);
        let lesson = &batch.candidates[0];
        assert_eq!(lesson.scope, LessonScope::DomainGeneral);
        assert!(lesson.generalized_rule.contains("authoritative sources"));
        assert!(
            lesson.regression_examples[0]
                .input_summary
                .contains("commit history")
        );
    }

    #[test]
    fn ignores_one_off_branch_and_filename_facts() {
        let batch = LessonInferenceEngine.infer(&[signal(
            "local",
            LearningSignalKind::ExplicitCorrection,
            "Use branch fix-504 and filename result.patch this time.",
            CandidateScope::Task,
        )]);
        assert!(batch.candidates.is_empty());
        assert_eq!(batch.ignored_signal_ids, vec!["local"]);
    }

    #[test]
    fn distinguishes_user_preference_from_engineering_policy() {
        let batch = LessonInferenceEngine.infer(&[signal(
            "pref",
            LearningSignalKind::Preference,
            "Always give me the complete failure set before editing.",
            CandidateScope::User,
        )]);
        let lesson = &batch.candidates[0];
        assert_eq!(lesson.scope, LessonScope::User);
        assert_eq!(lesson.implementation_type, ImplementationType::Memory);
    }

    #[test]
    fn conflicting_evidence_blocks_promotion_and_lowers_confidence() {
        let mut item = signal(
            "conflict",
            LearningSignalKind::Preference,
            "Always ask before editing.",
            CandidateScope::User,
        );
        item.contradicted_by.push("signal-never-ask".to_owned());
        let lesson = &LessonInferenceEngine.infer(&[item]).candidates[0];
        assert!(lesson.promotion_blocked);
        assert!(lesson.confidence_milli < 800);
        assert_eq!(lesson.contradictory_signal_ids, vec!["signal-never-ask"]);
    }

    #[test]
    fn ambiguous_evidence_blocks_promotion_and_records_uncertainty() {
        let mut item = signal(
            "ambiguous",
            LearningSignalKind::ExplicitCorrection,
            "Use a more complete approach.",
            CandidateScope::Repository,
        );
        item.ambiguity
            .push("the requested scope is not yet clear".to_owned());
        let lesson = &LessonInferenceEngine.infer(&[item]).candidates[0];
        assert!(lesson.promotion_blocked);
        assert!(lesson.confidence_milli < 750);
        assert!(
            lesson
                .uncertainty
                .iter()
                .any(|value| value.contains("ambiguous or contradictory"))
        );
    }

    #[test]
    fn unjustified_claim_becomes_cross_domain_evidence_rule() {
        let batch = LessonInferenceEngine.infer(&[signal(
            "claim",
            LearningSignalKind::UnjustifiedClaim,
            "Do not claim completeness without verifying every source.",
            CandidateScope::User,
        )]);
        let lesson = &batch.candidates[0];
        assert_eq!(lesson.scope, LessonScope::DomainGeneral);
        assert_eq!(lesson.implementation_type, ImplementationType::PromptRule);
        assert!(lesson.rationale.contains("durable lesson"));
    }

    #[test]
    fn equivalent_lessons_merge_and_keep_independent_evidence() {
        let first = signal(
            "a",
            LearningSignalKind::WorkflowFailure,
            "You should have checked dependencies before saying it was done.",
            CandidateScope::Repository,
        );
        let second = signal(
            "b",
            LearningSignalKind::WorkflowFailure,
            "Verify prerequisites before reporting completion.",
            CandidateScope::Repository,
        );
        let batch = LessonInferenceEngine.infer(&[first, second]);
        assert_eq!(batch.candidates.len(), 1);
        assert_eq!(batch.candidates[0].supporting_signal_ids.len(), 2);
        assert_eq!(batch.candidates[0].regression_examples.len(), 2);
    }
}
