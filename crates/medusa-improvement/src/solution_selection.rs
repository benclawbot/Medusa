use serde::{Deserialize, Serialize};

use crate::lesson_inference::{ImplementationType, LessonCandidate, LessonScope};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SolutionType {
    NoPersistence,
    SessionNote,
    RepositoryMemory,
    UserPreference,
    ReusableSkill,
    HarnessPolicy,
    WorkflowGate,
    RegressionFixture,
    DocumentationUpdate,
    ConfigurationChange,
    ProductCodeChange,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStrength {
    Standard,
    Elevated,
    Critical,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SolutionScore {
    pub solution_type: SolutionType,
    pub score: i16,
    pub reasons: Vec<String>,
    pub rejected_because: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GeneratedArtifact {
    pub path: String,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SolutionProposal {
    pub lesson_id: String,
    pub source_signal_ids: Vec<String>,
    pub selected: Vec<SolutionType>,
    pub alternatives: Vec<SolutionScore>,
    pub rationale: String,
    pub review_strength: ReviewStrength,
    pub isolated: bool,
    pub editable: bool,
    pub artifacts: Vec<GeneratedArtifact>,
    pub activation_blocked: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SolutionSelector;

impl SolutionSelector {
    #[must_use]
    pub fn propose(&self, lesson: &LessonCandidate) -> SolutionProposal {
        let mut scores = SolutionType::all()
            .into_iter()
            .map(|solution_type| score(solution_type, lesson))
            .collect::<Vec<_>>();
        scores.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.solution_type.cmp(&right.solution_type))
        });

        let unsafe_to_generate = lesson.promotion_blocked
            || !lesson.contradictory_signal_ids.is_empty()
            || lesson.confidence_milli < 600
            || lesson.scope == LessonScope::Unresolved;
        let selected = if unsafe_to_generate {
            vec![SolutionType::NoPersistence]
        } else {
            select_solutions(&scores)
        };
        let review_strength = selected
            .iter()
            .map(|solution_type| review_strength(*solution_type))
            .max_by_key(review_rank)
            .unwrap_or(ReviewStrength::Standard);
        let artifacts = if unsafe_to_generate {
            Vec::new()
        } else {
            selected
                .iter()
                .filter_map(|solution_type| generate(*solution_type, lesson))
                .collect()
        };

        SolutionProposal {
            lesson_id: lesson.id.clone(),
            source_signal_ids: lesson.supporting_signal_ids.clone(),
            selected,
            alternatives: scores,
            rationale: rationale(lesson, unsafe_to_generate),
            review_strength,
            isolated: true,
            editable: true,
            artifacts,
            activation_blocked: true,
        }
    }
}

fn select_solutions(scores: &[SolutionScore]) -> Vec<SolutionType> {
    let primary = scores
        .first()
        .map(|item| item.solution_type)
        .unwrap_or(SolutionType::NoPersistence);
    let score_for = |kind| {
        scores
            .iter()
            .find(|item| item.solution_type == kind)
            .map_or(i16::MIN, |item| item.score)
    };
    let code_score = score_for(SolutionType::ProductCodeChange);
    let fixture_score = score_for(SolutionType::RegressionFixture);

    if code_score >= 60 && fixture_score >= 60 {
        return vec![
            SolutionType::ProductCodeChange,
            SolutionType::RegressionFixture,
        ];
    }

    let mut selected = vec![primary];
    if primary == SolutionType::ReusableSkill && fixture_score >= 60 {
        selected.push(SolutionType::RegressionFixture);
    }
    selected
}

impl SolutionType {
    fn all() -> [Self; 11] {
        [
            Self::NoPersistence,
            Self::SessionNote,
            Self::RepositoryMemory,
            Self::UserPreference,
            Self::ReusableSkill,
            Self::HarnessPolicy,
            Self::WorkflowGate,
            Self::RegressionFixture,
            Self::DocumentationUpdate,
            Self::ConfigurationChange,
            Self::ProductCodeChange,
        ]
    }
}

fn score(solution_type: SolutionType, lesson: &LessonCandidate) -> SolutionScore {
    let mut value = 0_i16;
    let mut reasons = Vec::new();
    let mut rejected_because = Vec::new();

    match (solution_type, lesson.scope) {
        (SolutionType::UserPreference, LessonScope::User) => {
            value += 100;
            reasons.push(
                "the lesson is scoped to one user's durable interaction preferences".to_owned(),
            );
        }
        (SolutionType::RepositoryMemory, LessonScope::Repository) => {
            value += 95;
            reasons.push("the lesson is a repository-local convention".to_owned());
        }
        (
            SolutionType::ReusableSkill,
            LessonScope::ProjectWorkflow | LessonScope::DomainGeneral,
        ) => {
            value += 85;
            reasons.push("the lesson describes a portable repeatable procedure".to_owned());
        }
        (SolutionType::SessionNote, LessonScope::Task) => {
            value += 70;
            reasons.push("the lesson is bounded to the active task".to_owned());
        }
        (SolutionType::NoPersistence, LessonScope::Unresolved) => value += 120,
        _ => {}
    }

    match (solution_type, lesson.implementation_type) {
        (SolutionType::UserPreference, ImplementationType::Memory)
        | (SolutionType::RepositoryMemory, ImplementationType::RepositoryConvention)
        | (SolutionType::ReusableSkill, ImplementationType::Skill)
        | (SolutionType::HarnessPolicy, ImplementationType::PromptRule)
        | (SolutionType::RegressionFixture, ImplementationType::TestOrEval) => value += 45,
        _ => {}
    }

    let text = format!(
        "{} {} {}",
        lesson.observed_pattern, lesson.root_cause, lesson.generalized_rule
    )
    .to_ascii_lowercase();
    if contains_any(&text, &["safety", "permission", "must never", "invariant"])
        && solution_type == SolutionType::HarnessPolicy
    {
        value += 100;
        reasons.push("the lesson requires an enforceable invariant".to_owned());
    }
    if contains_any(
        &text,
        &[
            "defect",
            "bug",
            "crash",
            "incorrect serialization",
            "product behavior",
        ],
    ) {
        if solution_type == SolutionType::ProductCodeChange {
            value += 100;
            reasons.push("the evidence identifies a recurring product defect".to_owned());
        }
        if solution_type == SolutionType::RegressionFixture {
            value += 80;
            reasons.push("a deterministic fixture protects against recurrence".to_owned());
        }
    }
    if contains_any(
        &text,
        &[
            "check before",
            "before claiming",
            "completion gate",
            "checklist",
        ],
    ) {
        if solution_type == SolutionType::WorkflowGate {
            value += 90;
            reasons.push("the lesson requires a completion-time workflow check".to_owned());
        }
        if solution_type == SolutionType::RegressionFixture {
            value += 60;
        }
    }

    if lesson.scope == LessonScope::Repository
        && matches!(
            solution_type,
            SolutionType::UserPreference | SolutionType::HarnessPolicy
        )
    {
        value -= 100;
        rejected_because.push(
            "repository-local evidence cannot silently change user-global or harness behavior"
                .to_owned(),
        );
    }
    if lesson.confidence_milli < 600
        || lesson.promotion_blocked
        || !lesson.contradictory_signal_ids.is_empty()
    {
        if solution_type == SolutionType::NoPersistence {
            value += 150;
            reasons.push(
                "low-confidence, blocked, or contradictory evidence must remain inactive"
                    .to_owned(),
            );
        } else {
            value -= 150;
            rejected_because
                .push("the evidence is not safe to turn into durable behavior".to_owned());
        }
    }

    SolutionScore {
        solution_type,
        score: value,
        reasons,
        rejected_because,
    }
}

fn generate(solution_type: SolutionType, lesson: &LessonCandidate) -> Option<GeneratedArtifact> {
    let artifact = match solution_type {
        SolutionType::ReusableSkill => GeneratedArtifact {
            path: format!(".medusa/candidates/skills/{}.md", lesson.id),
            content: format!(
                "# {}\n\n## Trigger conditions\nApply when: {}\n\n## Workflow\n1. Inspect the cited evidence.\n2. Apply this rule: {}\n3. Verify the result before completion.\n\n## Completion criteria\nThe rule is satisfied and evidence is recorded.\n\n## Exclusions\n{}\n\n## Evidence requirements\nSource signals: {}\n",
                lesson.id,
                lesson.observed_pattern,
                lesson.generalized_rule,
                lesson.non_applicable_contexts.join("; "),
                lesson.supporting_signal_ids.join(", ")
            ),
        },
        SolutionType::HarnessPolicy => GeneratedArtifact {
            path: format!(".medusa/candidates/policies/{}.md", lesson.id),
            content: format!(
                "# Policy candidate: {}\n\nEnforcement point: task completion and action authorization.\n\nAffected task classes: tasks matching {}.\n\nInvariant: {}\n\nThis candidate is inactive until elevated review.\n",
                lesson.id, lesson.observed_pattern, lesson.generalized_rule
            ),
        },
        SolutionType::ProductCodeChange => GeneratedArtifact {
            path: format!(".medusa/candidates/code/{}/PLAN.md", lesson.id),
            content: format!(
                "# Isolated code-change candidate\n\nSource lesson: {}\n\nRequired behavior: {}\n\nImplementation must occur on a dedicated branch or worktree and include a regression test. No direct main-branch mutation is permitted.\n",
                lesson.id, lesson.generalized_rule
            ),
        },
        SolutionType::RegressionFixture => GeneratedArtifact {
            path: format!(".medusa/candidates/regressions/{}.md", lesson.id),
            content: format!(
                "# Regression fixture: {}\n\nGiven evidence from {}, assert that future behavior satisfies: {}\n",
                lesson.id,
                lesson.supporting_signal_ids.join(", "),
                lesson.generalized_rule
            ),
        },
        SolutionType::RepositoryMemory => GeneratedArtifact {
            path: format!(".medusa/candidates/memory/{}.md", lesson.id),
            content: format!(
                "# Repository memory candidate\n\n{}\n",
                lesson.generalized_rule
            ),
        },
        SolutionType::UserPreference => GeneratedArtifact {
            path: format!(".medusa/candidates/preferences/{}.json", lesson.id),
            content: format!(
                "{{\"lesson_id\":\"{}\",\"preference\":{:?},\"active\":false}}",
                lesson.id, lesson.generalized_rule
            ),
        },
        SolutionType::WorkflowGate => GeneratedArtifact {
            path: format!(".medusa/candidates/gates/{}.md", lesson.id),
            content: format!(
                "# Workflow gate candidate\n\nBefore completion: {}\n",
                lesson.generalized_rule
            ),
        },
        SolutionType::SessionNote
        | SolutionType::DocumentationUpdate
        | SolutionType::ConfigurationChange => GeneratedArtifact {
            path: format!(
                ".medusa/candidates/notes/{}-{:?}.md",
                lesson.id, solution_type
            ),
            content: format!("# Candidate\n\n{}\n", lesson.generalized_rule),
        },
        SolutionType::NoPersistence => return None,
    };
    Some(artifact)
}

fn review_strength(solution_type: SolutionType) -> ReviewStrength {
    match solution_type {
        SolutionType::HarnessPolicy
        | SolutionType::ConfigurationChange
        | SolutionType::ProductCodeChange => ReviewStrength::Critical,
        SolutionType::ReusableSkill
        | SolutionType::WorkflowGate
        | SolutionType::RegressionFixture => ReviewStrength::Elevated,
        _ => ReviewStrength::Standard,
    }
}

fn review_rank(strength: &ReviewStrength) -> u8 {
    match strength {
        ReviewStrength::Standard => 0,
        ReviewStrength::Elevated => 1,
        ReviewStrength::Critical => 2,
    }
}

fn rationale(lesson: &LessonCandidate, unsafe_to_generate: bool) -> String {
    if unsafe_to_generate {
        return "No durable implementation was generated because scope, confidence, or contradictory evidence requires resolution first.".to_owned();
    }
    format!(
        "The selected mechanism matches scope {:?}, inferred implementation {:?}, enforcement needs, portability, and reversibility. Alternatives remain visible and editable before validation.",
        lesson.scope, lesson.implementation_type
    )
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lesson_inference::{ImplementationType, LessonCandidate, LessonScope};

    fn lesson(
        scope: LessonScope,
        implementation_type: ImplementationType,
        rule: &str,
    ) -> LessonCandidate {
        LessonCandidate {
            id: "lesson-test".to_owned(),
            observed_pattern: rule.to_owned(),
            root_cause: rule.to_owned(),
            generalized_rule: rule.to_owned(),
            scope,
            non_applicable_contexts: vec!["explicit exclusions".to_owned()],
            confidence_milli: 850,
            uncertainty: Vec::new(),
            supporting_signal_ids: vec!["signal-1".to_owned()],
            contradictory_signal_ids: Vec::new(),
            implementation_type,
            regression_examples: Vec::new(),
            rationale: "evidence-backed".to_owned(),
            promotion_blocked: false,
        }
    }

    #[test]
    fn classifies_user_repository_skill_policy_code_and_no_action() {
        let selector = SolutionSelector;
        assert_eq!(
            selector
                .propose(&lesson(
                    LessonScope::User,
                    ImplementationType::Memory,
                    "prefer concise updates"
                ))
                .selected[0],
            SolutionType::UserPreference
        );
        assert_eq!(
            selector
                .propose(&lesson(
                    LessonScope::Repository,
                    ImplementationType::RepositoryConvention,
                    "use repository naming rules"
                ))
                .selected[0],
            SolutionType::RepositoryMemory
        );
        assert_eq!(
            selector
                .propose(&lesson(
                    LessonScope::DomainGeneral,
                    ImplementationType::Skill,
                    "inventory sources before claiming completeness"
                ))
                .selected[0],
            SolutionType::ReusableSkill
        );
        assert_eq!(
            selector
                .propose(&lesson(
                    LessonScope::DomainGeneral,
                    ImplementationType::PromptRule,
                    "a safety invariant must never permit secret disclosure"
                ))
                .selected[0],
            SolutionType::HarnessPolicy
        );
        assert_eq!(
            selector
                .propose(&lesson(
                    LessonScope::Repository,
                    ImplementationType::TestOrEval,
                    "a recurring product defect causes a crash"
                ))
                .selected[0],
            SolutionType::ProductCodeChange
        );
        let mut unresolved = lesson(
            LessonScope::Unresolved,
            ImplementationType::Unresolved,
            "unclear",
        );
        unresolved.confidence_milli = 400;
        assert_eq!(
            selector.propose(&unresolved).selected,
            vec![SolutionType::NoPersistence]
        );
    }

    #[test]
    fn skill_candidate_contains_required_reviewable_sections() {
        let proposal = SolutionSelector.propose(&lesson(
            LessonScope::ProjectWorkflow,
            ImplementationType::Skill,
            "verify prerequisites before completion",
        ));
        let content = &proposal.artifacts[0].content;
        assert!(content.contains("Trigger conditions"));
        assert!(content.contains("Workflow"));
        assert!(content.contains("Completion criteria"));
        assert!(content.contains("Exclusions"));
        assert!(content.contains("Evidence requirements"));
        assert!(proposal.activation_blocked);
        assert!(proposal.editable);
    }

    #[test]
    fn code_candidate_is_isolated_and_requires_strong_review() {
        let proposal = SolutionSelector.propose(&lesson(
            LessonScope::Repository,
            ImplementationType::TestOrEval,
            "a recurring product defect causes incorrect serialization",
        ));
        assert_eq!(
            proposal.selected,
            vec![
                SolutionType::ProductCodeChange,
                SolutionType::RegressionFixture
            ]
        );
        assert_eq!(proposal.review_strength, ReviewStrength::Critical);
        assert!(proposal.isolated);
        assert!(
            proposal
                .artifacts
                .iter()
                .any(|artifact| { artifact.content.contains("No direct main-branch mutation") })
        );
    }

    #[test]
    fn contradictory_evidence_is_blocked_even_when_flag_is_false() {
        let mut item = lesson(
            LessonScope::User,
            ImplementationType::Memory,
            "prefer terse replies",
        );
        item.contradictory_signal_ids.push("signal-2".to_owned());
        let proposal = SolutionSelector.propose(&item);
        assert_eq!(proposal.selected, vec![SolutionType::NoPersistence]);
        assert!(proposal.artifacts.is_empty());
        assert!(proposal.activation_blocked);
    }

    #[test]
    fn blocked_or_low_confidence_evidence_creates_no_active_behavior() {
        let mut item = lesson(
            LessonScope::User,
            ImplementationType::Memory,
            "prefer terse replies",
        );
        item.promotion_blocked = true;
        let proposal = SolutionSelector.propose(&item);
        assert_eq!(proposal.selected, vec![SolutionType::NoPersistence]);
        assert!(proposal.artifacts.is_empty());
        assert!(proposal.activation_blocked);
    }
}
