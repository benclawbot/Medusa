use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Stable identifier for one acceptance criterion inside a goal contract.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct AcceptanceCriterionId(String);

impl AcceptanceCriterionId {
    /// Creates a validated criterion identifier.
    pub fn parse(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("acceptance criterion identifier cannot be empty");
        }
        if trimmed.len() > 96 {
            return Err("acceptance criterion identifier is too long");
        }
        if !trimmed
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err("acceptance criterion identifier contains unsupported characters");
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// Returns the identifier as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Evidence category required or produced while proving completion.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Test,
    Build,
    Lint,
    RuntimeObservation,
    FileInspection,
    UserConfirmation,
    ExternalArtifact,
}

/// One explicit, independently verifiable completion condition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AcceptanceCriterion {
    pub id: AcceptanceCriterionId,
    pub description: String,
    #[serde(default)]
    pub required_evidence: BTreeSet<EvidenceKind>,
}

impl AcceptanceCriterion {
    /// Creates a criterion and rejects descriptions that cannot guide verification.
    pub fn new(
        id: AcceptanceCriterionId,
        description: impl Into<String>,
        required_evidence: impl IntoIterator<Item = EvidenceKind>,
    ) -> Result<Self, &'static str> {
        let description = description.into();
        if description.trim().is_empty() {
            return Err("acceptance criterion description cannot be empty");
        }
        Ok(Self {
            id,
            description,
            required_evidence: required_evidence.into_iter().collect(),
        })
    }
}

/// A durable proof item tied to one acceptance criterion.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompletionEvidence {
    pub criterion_id: AcceptanceCriterionId,
    pub kind: EvidenceKind,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<String>,
}

impl CompletionEvidence {
    /// Creates evidence and rejects empty summaries.
    pub fn new(
        criterion_id: AcceptanceCriterionId,
        kind: EvidenceKind,
        summary: impl Into<String>,
    ) -> Result<Self, &'static str> {
        let summary = summary.into();
        if summary.trim().is_empty() {
            return Err("completion evidence summary cannot be empty");
        }
        Ok(Self {
            criterion_id,
            kind,
            summary,
            artifact_refs: Vec::new(),
        })
    }

    /// Adds a durable artifact reference such as a log, report, commit, or file path.
    #[must_use]
    pub fn with_artifact(mut self, artifact_ref: impl Into<String>) -> Self {
        let artifact_ref = artifact_ref.into();
        if !artifact_ref.trim().is_empty() {
            self.artifact_refs.push(artifact_ref);
        }
        self
    }
}

/// Durable goal definition used to decide whether work is actually complete.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GoalContract {
    pub objective: String,
    #[serde(default)]
    pub criteria: Vec<AcceptanceCriterion>,
}

impl GoalContract {
    /// Creates a goal contract with unique criterion identifiers.
    pub fn new(
        objective: impl Into<String>,
        criteria: Vec<AcceptanceCriterion>,
    ) -> Result<Self, &'static str> {
        let objective = objective.into();
        if objective.trim().is_empty() {
            return Err("goal objective cannot be empty");
        }
        let mut ids = BTreeSet::new();
        if criteria.iter().any(|criterion| !ids.insert(criterion.id.clone())) {
            return Err("goal contains duplicate acceptance criterion identifiers");
        }
        Ok(Self {
            objective,
            criteria,
        })
    }

    /// Evaluates collected evidence without trusting a model-authored completion claim.
    #[must_use]
    pub fn evaluate(&self, evidence: &[CompletionEvidence]) -> CompletionAssessment {
        let criteria_by_id = self
            .criteria
            .iter()
            .map(|criterion| (criterion.id.clone(), criterion))
            .collect::<BTreeMap<_, _>>();
        let mut observed = BTreeMap::<AcceptanceCriterionId, BTreeSet<EvidenceKind>>::new();
        let mut orphan_evidence = Vec::new();

        for item in evidence {
            if criteria_by_id.contains_key(&item.criterion_id) {
                observed
                    .entry(item.criterion_id.clone())
                    .or_default()
                    .insert(item.kind);
            } else {
                orphan_evidence.push(item.clone());
            }
        }

        let criteria = self
            .criteria
            .iter()
            .map(|criterion| {
                let observed_kinds = observed.get(&criterion.id).cloned().unwrap_or_default();
                let missing_evidence = criterion
                    .required_evidence
                    .difference(&observed_kinds)
                    .copied()
                    .collect::<BTreeSet<_>>();
                CriterionAssessment {
                    criterion_id: criterion.id.clone(),
                    satisfied: missing_evidence.is_empty(),
                    observed_evidence: observed_kinds,
                    missing_evidence,
                }
            })
            .collect::<Vec<_>>();

        CompletionAssessment {
            complete: !criteria.is_empty() && criteria.iter().all(|criterion| criterion.satisfied),
            criteria,
            orphan_evidence,
        }
    }
}

/// Evidence evaluation for one acceptance criterion.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CriterionAssessment {
    pub criterion_id: AcceptanceCriterionId,
    pub satisfied: bool,
    pub observed_evidence: BTreeSet<EvidenceKind>,
    pub missing_evidence: BTreeSet<EvidenceKind>,
}

/// Deterministic completion decision and its supporting detail.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompletionAssessment {
    pub complete: bool,
    pub criteria: Vec<CriterionAssessment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub orphan_evidence: Vec<CompletionEvidence>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> AcceptanceCriterionId {
        AcceptanceCriterionId::parse(value).expect("valid criterion id")
    }

    #[test]
    fn completion_requires_every_required_evidence_kind() {
        let contract = GoalContract::new(
            "ship a verified fix",
            vec![
                AcceptanceCriterion::new(
                    id("behavior-fixed"),
                    "The reported behavior is corrected",
                    [EvidenceKind::Test, EvidenceKind::RuntimeObservation],
                )
                .expect("criterion"),
                AcceptanceCriterion::new(
                    id("build-clean"),
                    "The project still builds",
                    [EvidenceKind::Build],
                )
                .expect("criterion"),
            ],
        )
        .expect("contract");

        let partial = vec![
            CompletionEvidence::new(id("behavior-fixed"), EvidenceKind::Test, "targeted test passed")
                .expect("evidence"),
            CompletionEvidence::new(id("build-clean"), EvidenceKind::Build, "cargo check passed")
                .expect("evidence"),
        ];
        let assessment = contract.evaluate(&partial);
        assert!(!assessment.complete);
        assert_eq!(
            assessment.criteria[0].missing_evidence,
            BTreeSet::from([EvidenceKind::RuntimeObservation])
        );

        let mut complete = partial;
        complete.push(
            CompletionEvidence::new(
                id("behavior-fixed"),
                EvidenceKind::RuntimeObservation,
                "reproduction no longer fails",
            )
            .expect("evidence"),
        );
        assert!(contract.evaluate(&complete).complete);
    }

    #[test]
    fn model_claim_without_criteria_never_counts_as_complete() {
        let contract = GoalContract::new("do the work", Vec::new()).expect("contract");
        assert!(!contract.evaluate(&[]).complete);
    }

    #[test]
    fn duplicate_criterion_ids_are_rejected() {
        let criterion = AcceptanceCriterion::new(
            id("same"),
            "first condition",
            [EvidenceKind::Test],
        )
        .expect("criterion");
        assert_eq!(
            GoalContract::new("objective", vec![criterion.clone(), criterion]),
            Err("goal contains duplicate acceptance criterion identifiers")
        );
    }

    #[test]
    fn evidence_for_unknown_criteria_is_reported_but_never_satisfies_the_goal() {
        let contract = GoalContract::new(
            "objective",
            vec![
                AcceptanceCriterion::new(id("known"), "known condition", [EvidenceKind::Test])
                    .expect("criterion"),
            ],
        )
        .expect("contract");
        let evidence = CompletionEvidence::new(id("unknown"), EvidenceKind::Test, "passed")
            .expect("evidence");
        let assessment = contract.evaluate(&[evidence]);
        assert!(!assessment.complete);
        assert_eq!(assessment.orphan_evidence.len(), 1);
    }
}
