//! Explainable retrieval and task-scoped application of approved learned behavior.
//!
//! The resolver combines deterministic relevance matching with the authoritative scope resolver.
//! It records every considered item, blocks ambiguous equal-scope conflicts, respects explicit
//! negative contexts and per-task suppression, and keeps high-impact behavior review-gated.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::scoped_memory::{
    LearningScope, ResolutionSet, ResolvedLearning, ScopeContext, ScopedLearning,
    ScopedMemoryStore, StoreError,
};

const DEFAULT_MINIMUM_SCORE: u32 = 180;
const DEFAULT_MAX_SELECTED: usize = 8;
const DEFAULT_MAX_CONSIDERED: usize = 10_000;
const DEFAULT_STALE_AFTER_MS: i64 = 180 * 24 * 60 * 60 * 1_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationPhase {
    Planning,
    ToolSelection,
    Execution,
    Verification,
    Completion,
    ResponseStyle,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningImpact {
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionDisposition {
    Selected,
    Rejected,
    Suppressed,
    Conflict,
    ReviewRequired,
    Stale,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskContext {
    pub scope: ScopeContext,
    pub objective: String,
    pub explicit_exclusions: BTreeSet<String>,
    pub suppressed_learning_ids: BTreeSet<String>,
    pub approved_high_impact_ids: BTreeSet<String>,
    pub now_unix_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetrievalConfig {
    pub minimum_score: u32,
    pub max_selected: usize,
    pub max_considered: usize,
    pub stale_after_ms: i64,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            minimum_score: DEFAULT_MINIMUM_SCORE,
            max_selected: DEFAULT_MAX_SELECTED,
            max_considered: DEFAULT_MAX_CONSIDERED,
            stale_after_ms: DEFAULT_STALE_AFTER_MS,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelectedLearning {
    pub learning_id: String,
    pub conflict_key: String,
    pub scope: LearningScope,
    pub version: u64,
    pub phase: ApplicationPhase,
    pub impact: LearningImpact,
    pub score: u32,
    pub generalized_rule: String,
    pub explanation: String,
    pub shadowed_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelectionRecord {
    pub learning_id: String,
    pub conflict_key: String,
    pub scope: LearningScope,
    pub version: u64,
    pub phase: ApplicationPhase,
    pub impact: LearningImpact,
    pub score: u32,
    pub disposition: SelectionDisposition,
    pub explanation: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrievalResult {
    pub selected: Vec<SelectedLearning>,
    pub considered: Vec<SelectionRecord>,
    pub truncated_count: usize,
}

impl RetrievalResult {
    #[must_use]
    pub fn prompt_context(&self) -> Option<String> {
        if self.selected.is_empty() {
            return None;
        }
        let mut output = String::from(
            "Approved learned behavior selected for this task. Apply only within the stated phase and do not override explicit user intent:\n",
        );
        for selected in &self.selected {
            output.push_str(&format!(
                "- [{} v{}; {:?}; score {}] {} Reason: {}\n",
                selected.learning_id,
                selected.version,
                selected.phase,
                selected.score,
                selected.generalized_rule,
                selected.explanation
            ));
        }
        Some(output)
    }

    #[must_use]
    pub fn selected_ids(&self) -> BTreeSet<String> {
        self.selected
            .iter()
            .map(|selected| selected.learning_id.clone())
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvaluationCase {
    pub resolution: ResolutionSet,
    pub context: TaskContext,
    pub expected_selected_ids: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrievalEvaluation {
    pub true_positive: usize,
    pub false_positive: usize,
    pub false_negative: usize,
    pub precision_milli: u16,
    pub recall_milli: u16,
}

pub fn retrieve(
    store: &ScopedMemoryStore,
    context: &TaskContext,
    config: &RetrievalConfig,
) -> Result<RetrievalResult, StoreError> {
    let resolution = store.resolve(&context.scope)?;
    Ok(retrieve_resolution(&resolution, context, config))
}

#[must_use]
pub fn retrieve_resolution(
    resolution: &ResolutionSet,
    context: &TaskContext,
    config: &RetrievalConfig,
) -> RetrievalResult {
    let objective_terms = expanded_terms(&context.objective);
    let mut candidates = resolution.resolved.iter().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        scope_weight(right.winner.scope)
            .cmp(&scope_weight(left.winner.scope))
            .then_with(|| right.winner.version.cmp(&left.winner.version))
            .then_with(|| left.winner.id.cmp(&right.winner.id))
    });

    let truncated_count = candidates.len().saturating_sub(config.max_considered);
    candidates.truncate(config.max_considered);

    let mut result = RetrievalResult {
        truncated_count,
        ..RetrievalResult::default()
    };

    for resolved in candidates {
        let winner = &resolved.winner;
        let phase = application_phase(winner);
        let impact = learning_impact(winner);
        let score = relevance_score(winner, context, &objective_terms);
        let base = SelectionRecord {
            learning_id: winner.id.clone(),
            conflict_key: winner.conflict_key.clone(),
            scope: winner.scope,
            version: winner.version,
            phase,
            impact,
            score,
            disposition: SelectionDisposition::Rejected,
            explanation: String::new(),
        };

        if context.suppressed_learning_ids.contains(&winner.id) {
            result.considered.push(SelectionRecord {
                disposition: SelectionDisposition::Suppressed,
                explanation: "suppressed for this task without deleting the learned item"
                    .to_owned(),
                ..base
            });
            continue;
        }

        if explicitly_excluded(winner, context) {
            result.considered.push(SelectionRecord {
                disposition: SelectionDisposition::Suppressed,
                explanation: "an explicit task exclusion matched this learned behavior".to_owned(),
                ..base
            });
            continue;
        }

        if equal_scope_conflict(resolved) {
            result.considered.push(SelectionRecord {
                disposition: SelectionDisposition::Conflict,
                explanation:
                    "multiple active items at the same scope disagree; explicit resolution is required"
                        .to_owned(),
                ..base
            });
            continue;
        }

        if is_stale(winner, context.now_unix_ms, config.stale_after_ms) {
            result.considered.push(SelectionRecord {
                disposition: SelectionDisposition::Stale,
                explanation: "the learned item is older than the configured freshness limit"
                    .to_owned(),
                ..base
            });
            continue;
        }

        if score < config.minimum_score {
            result.considered.push(SelectionRecord {
                explanation: format!(
                    "relevance score {score} is below the selection threshold {}",
                    config.minimum_score
                ),
                ..base
            });
            continue;
        }

        if impact == LearningImpact::High && !context.approved_high_impact_ids.contains(&winner.id)
        {
            result.considered.push(SelectionRecord {
                disposition: SelectionDisposition::ReviewRequired,
                explanation:
                    "high-impact learned behavior requires explicit task-bound approval before use"
                        .to_owned(),
                ..base
            });
            continue;
        }

        let explanation = selection_explanation(winner, resolved, context, score);
        result.selected.push(SelectedLearning {
            learning_id: winner.id.clone(),
            conflict_key: winner.conflict_key.clone(),
            scope: winner.scope,
            version: winner.version,
            phase,
            impact,
            score,
            generalized_rule: winner.generalized_rule.clone(),
            explanation: explanation.clone(),
            shadowed_ids: resolved
                .shadowed
                .iter()
                .map(|entry| entry.id.clone())
                .collect(),
        });
        result.considered.push(SelectionRecord {
            disposition: SelectionDisposition::Selected,
            explanation,
            ..base
        });
    }

    result.selected.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| scope_weight(right.scope).cmp(&scope_weight(left.scope)))
            .then_with(|| left.learning_id.cmp(&right.learning_id))
    });

    if result.selected.len() > config.max_selected {
        let rejected = result.selected.split_off(config.max_selected);
        let rejected_ids = rejected
            .into_iter()
            .map(|selected| selected.learning_id)
            .collect::<BTreeSet<_>>();
        for record in &mut result.considered {
            if rejected_ids.contains(&record.learning_id) {
                record.disposition = SelectionDisposition::Rejected;
                record.explanation = format!(
                    "ranked below the configured maximum of {} selected items",
                    config.max_selected
                );
            }
        }
    }

    result.considered.sort_by(|left, right| {
        left.conflict_key
            .cmp(&right.conflict_key)
            .then_with(|| left.learning_id.cmp(&right.learning_id))
    });
    result
}

#[must_use]
pub fn evaluate(cases: &[EvaluationCase], config: &RetrievalConfig) -> RetrievalEvaluation {
    let mut evaluation = RetrievalEvaluation::default();
    for case in cases {
        let selected = retrieve_resolution(&case.resolution, &case.context, config).selected_ids();
        evaluation.true_positive = evaluation
            .true_positive
            .saturating_add(selected.intersection(&case.expected_selected_ids).count());
        evaluation.false_positive = evaluation
            .false_positive
            .saturating_add(selected.difference(&case.expected_selected_ids).count());
        evaluation.false_negative = evaluation
            .false_negative
            .saturating_add(case.expected_selected_ids.difference(&selected).count());
    }
    evaluation.precision_milli = ratio_milli(
        evaluation.true_positive,
        evaluation
            .true_positive
            .saturating_add(evaluation.false_positive),
    );
    evaluation.recall_milli = ratio_milli(
        evaluation.true_positive,
        evaluation
            .true_positive
            .saturating_add(evaluation.false_negative),
    );
    evaluation
}

fn relevance_score(
    learning: &ScopedLearning,
    context: &TaskContext,
    objective_terms: &BTreeSet<String>,
) -> u32 {
    let rule_terms = expanded_terms(&learning.generalized_rule);
    let key_terms = expanded_terms(&learning.conflict_key);
    let rule_overlap = objective_terms.intersection(&rule_terms).count();
    let key_overlap = objective_terms.intersection(&key_terms).count();
    let exact_task = context
        .scope
        .task_kind
        .as_ref()
        .is_some_and(|task_kind| learning.applicability.task_kinds.contains(task_kind));
    let exact_artifact = context
        .scope
        .artifact_kind
        .as_ref()
        .is_some_and(|artifact_kind| {
            learning
                .applicability
                .artifact_kinds
                .contains(artifact_kind)
        });
    let recency = recency_score(learning.updated_at_unix_ms, context.now_unix_ms);
    let evidence = u32::try_from(learning.provenance.evidence_digests.len())
        .unwrap_or(u32::MAX)
        .saturating_mul(10)
        .min(50);
    let version = u32::try_from(learning.version).unwrap_or(u32::MAX).min(20);

    u32::try_from(rule_overlap)
        .unwrap_or(u32::MAX)
        .saturating_mul(80)
        .saturating_add(
            u32::try_from(key_overlap)
                .unwrap_or(u32::MAX)
                .saturating_mul(120),
        )
        .saturating_add(if exact_task { 300 } else { 0 })
        .saturating_add(if exact_artifact { 250 } else { 0 })
        .saturating_add(u32::from(scope_weight(learning.scope)).saturating_mul(10))
        .saturating_add(recency)
        .saturating_add(evidence)
        .saturating_add(version)
}

fn explicitly_excluded(learning: &ScopedLearning, context: &TaskContext) -> bool {
    let learning_terms = expanded_terms(&format!(
        "{} {} {}",
        learning.id, learning.conflict_key, learning.generalized_rule
    ));
    context.explicit_exclusions.iter().any(|excluded| {
        learning_terms
            .iter()
            .any(|term| bounded_word_variant(excluded, term))
    }) || !learning
        .applicability
        .excluded_contexts
        .is_disjoint(&context.scope.context_tags)
}

fn bounded_word_variant(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    let (shorter, longer) = if left.len() <= right.len() {
        (left, right)
    } else {
        (right, left)
    };
    if shorter.len() < 4 {
        return false;
    }
    longer
        .strip_prefix(shorter)
        .is_some_and(|suffix| matches!(suffix, "s" | "es" | "ed" | "ing"))
}

fn equal_scope_conflict(resolved: &ResolvedLearning) -> bool {
    resolved.shadowed.iter().any(|shadowed| {
        shadowed.scope == resolved.winner.scope
            && shadowed.generalized_rule.trim() != resolved.winner.generalized_rule.trim()
    })
}

fn is_stale(learning: &ScopedLearning, now_unix_ms: i64, stale_after_ms: i64) -> bool {
    stale_after_ms >= 0
        && now_unix_ms
            .saturating_sub(learning.updated_at_unix_ms)
            .gt(&stale_after_ms)
}

fn selection_explanation(
    learning: &ScopedLearning,
    resolved: &ResolvedLearning,
    context: &TaskContext,
    score: u32,
) -> String {
    let mut reasons = Vec::new();
    if context
        .scope
        .task_kind
        .as_ref()
        .is_some_and(|task_kind| learning.applicability.task_kinds.contains(task_kind))
    {
        reasons.push("task kind matched".to_owned());
    }
    if context
        .scope
        .artifact_kind
        .as_ref()
        .is_some_and(|artifact_kind| {
            learning
                .applicability
                .artifact_kinds
                .contains(artifact_kind)
        })
    {
        reasons.push("artifact kind matched".to_owned());
    }
    if !resolved.shadowed.is_empty() {
        reasons.push(format!(
            "the {:?} scope overrode {} broader item(s)",
            learning.scope,
            resolved.shadowed.len()
        ));
    }
    if reasons.is_empty() {
        reasons.push("the task wording matched the learned rule and trigger key".to_owned());
    }
    format!(
        "{}; deterministic relevance score {score}",
        reasons.join(", ")
    )
}

fn application_phase(learning: &ScopedLearning) -> ApplicationPhase {
    let terms = expanded_terms(&format!(
        "{} {}",
        learning.conflict_key, learning.generalized_rule
    ));
    if contains_any(&terms, &["plan", "scope", "inventory", "completeness"]) {
        ApplicationPhase::Planning
    } else if contains_any(&terms, &["tool", "command", "provider", "search"]) {
        ApplicationPhase::ToolSelection
    } else if contains_any(&terms, &["verify", "verification", "test", "validation"]) {
        ApplicationPhase::Verification
    } else if contains_any(&terms, &["complete", "completion", "finish", "evidence"]) {
        ApplicationPhase::Completion
    } else if contains_any(&terms, &["style", "tone", "format", "response"]) {
        ApplicationPhase::ResponseStyle
    } else {
        ApplicationPhase::Execution
    }
}

fn learning_impact(learning: &ScopedLearning) -> LearningImpact {
    let value = format!(
        "{} {}",
        learning.conflict_key.to_ascii_lowercase(),
        learning.generalized_rule.to_ascii_lowercase()
    );
    if [
        "delete",
        "credential",
        "secret",
        "production",
        "deploy",
        "publish",
        "merge",
        "push",
        "permission",
        "network access",
        "shell command",
        "code change",
        "workflow gate",
        "harness policy",
    ]
    .iter()
    .any(|marker| value.contains(marker))
    {
        LearningImpact::High
    } else if ["verify", "test", "tool", "plan", "workflow"]
        .iter()
        .any(|marker| value.contains(marker))
    {
        LearningImpact::Medium
    } else {
        LearningImpact::Low
    }
}

fn recency_score(updated_at_unix_ms: i64, now_unix_ms: i64) -> u32 {
    let age_days = now_unix_ms.saturating_sub(updated_at_unix_ms).max(0) / (24 * 60 * 60 * 1_000);
    match age_days {
        0..=7 => 50,
        8..=30 => 35,
        31..=90 => 20,
        91..=180 => 10,
        _ => 0,
    }
}

const fn scope_weight(scope: LearningScope) -> u8 {
    match scope {
        LearningScope::Global => 0,
        LearningScope::User => 1,
        LearningScope::Organization => 2,
        LearningScope::Workspace => 3,
        LearningScope::Repository => 4,
        LearningScope::Session => 5,
        LearningScope::Task => 6,
    }
}

fn expanded_terms(value: &str) -> BTreeSet<String> {
    let mut terms = normalized_terms(value);
    let expansions = [
        (
            &["all", "complete", "comprehensive", "exhaustive"][..],
            "completeness",
        ),
        (
            &["test", "tests", "verify", "validation"][..],
            "verification",
        ),
        (&["release", "deploy", "publish"][..], "release"),
        (&["docs", "documentation", "readme"][..], "documentation"),
        (&["fix", "bug", "defect", "repair"][..], "bugfix"),
        (&["plan", "design", "approach"][..], "planning"),
        (&["format", "formatter", "lint"][..], "formatting"),
    ];
    for (aliases, canonical) in expansions {
        if aliases.iter().any(|alias| terms.contains(*alias)) {
            terms.insert(canonical.to_owned());
        }
    }
    terms
}

fn normalized_terms(value: &str) -> BTreeSet<String> {
    const STOP_WORDS: &[&str] = &[
        "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of",
        "on", "or", "that", "the", "this", "to", "use", "with",
    ];
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::trim)
        .filter(|term| term.len() >= 2)
        .map(str::to_ascii_lowercase)
        .filter(|term| !STOP_WORDS.contains(&term.as_str()))
        .collect()
}

fn contains_any(terms: &BTreeSet<String>, expected: &[&str]) -> bool {
    expected.iter().any(|term| terms.contains(*term))
}

fn ratio_milli(numerator: usize, denominator: usize) -> u16 {
    if denominator == 0 {
        return 1_000;
    }
    let value = numerator.saturating_mul(1_000) / denominator;
    u16::try_from(value).unwrap_or(1_000).min(1_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scoped_memory::{
        Applicability, LearningProvenance, LearningState, RepositoryIdentity, ScopeConflict,
    };

    fn learning(
        id: &str,
        key: &str,
        scope: LearningScope,
        rule: &str,
        task_kind: Option<&str>,
    ) -> ScopedLearning {
        ScopedLearning {
            id: id.to_owned(),
            conflict_key: key.to_owned(),
            owner_id: "user-1".to_owned(),
            scope,
            version: 1,
            state: LearningState::Active,
            generalized_rule: rule.to_owned(),
            provenance: LearningProvenance {
                source_signal_ids: vec!["signal-1".to_owned()],
                evidence_digests: vec!["a".repeat(64)],
                generalized_from_private_content: false,
            },
            applicability: Applicability {
                task_kinds: task_kind.into_iter().map(str::to_owned).collect(),
                ..Applicability::default()
            },
            created_at_unix_ms: 1_000,
            updated_at_unix_ms: 1_000,
        }
    }

    fn context(objective: &str, task_kind: Option<&str>) -> TaskContext {
        TaskContext {
            scope: ScopeContext {
                owner_id: "user-1".to_owned(),
                repository: RepositoryIdentity::new("https://example.test/repo", "/repo/.git").ok(),
                workspace_id: None,
                organization_id: None,
                session_id: Some("session-1".to_owned()),
                task_id: Some("task-1".to_owned()),
                task_kind: task_kind.map(str::to_owned),
                artifact_kind: None,
                context_tags: BTreeSet::new(),
            },
            objective: objective.to_owned(),
            explicit_exclusions: BTreeSet::new(),
            suppressed_learning_ids: BTreeSet::new(),
            approved_high_impact_ids: BTreeSet::new(),
            now_unix_ms: 2_000,
        }
    }

    fn resolution(entries: Vec<ResolvedLearning>) -> ResolutionSet {
        ResolutionSet {
            resolved: entries,
            conflicts: Vec::<ScopeConflict>::new(),
        }
    }

    #[test]
    fn matching_behavior_is_selected_with_explanation() {
        let item = learning(
            "complete-audit",
            "workflow.completeness",
            LearningScope::User,
            "inventory authoritative sources before a comprehensive test plan",
            Some("testing"),
        );
        let result = retrieve_resolution(
            &resolution(vec![ResolvedLearning {
                winner: item,
                shadowed: Vec::new(),
            }]),
            &context(
                "create a comprehensive repository test plan",
                Some("testing"),
            ),
            &RetrievalConfig::default(),
        );
        assert_eq!(
            result.selected_ids(),
            BTreeSet::from(["complete-audit".to_owned()])
        );
        assert!(result.selected[0].explanation.contains("task kind matched"));
        assert!(result.prompt_context().unwrap().contains("complete-audit"));
    }

    #[test]
    fn nearby_non_applicable_task_is_suppressed() {
        let item = learning(
            "release-check",
            "workflow.release",
            LearningScope::User,
            "verify release artifacts before publishing",
            None,
        );
        let mut task = context("write release documentation without publishing", None);
        task.explicit_exclusions.insert("publish".to_owned());
        let result = retrieve_resolution(
            &resolution(vec![ResolvedLearning {
                winner: item,
                shadowed: Vec::new(),
            }]),
            &task,
            &RetrievalConfig::default(),
        );
        assert!(result.selected.is_empty());
        assert_eq!(
            result.considered[0].disposition,
            SelectionDisposition::Suppressed
        );
    }

    #[test]
    fn repository_specific_behavior_overrides_broader_user_behavior() {
        let repository = learning(
            "repo-format",
            "format.command",
            LearningScope::Repository,
            "run cargo fmt --all",
            Some("formatting"),
        );
        let user = learning(
            "user-format",
            "format.command",
            LearningScope::User,
            "run the default formatter",
            Some("formatting"),
        );
        let result = retrieve_resolution(
            &resolution(vec![ResolvedLearning {
                winner: repository,
                shadowed: vec![user],
            }]),
            &context("format the repository", Some("formatting")),
            &RetrievalConfig::default(),
        );
        assert_eq!(result.selected[0].learning_id, "repo-format");
        assert_eq!(result.selected[0].shadowed_ids, vec!["user-format"]);
        assert!(result.selected[0].explanation.contains("overrode"));
    }

    #[test]
    fn equal_scope_disagreement_is_blocked() {
        let winner = learning(
            "format-a",
            "format.command",
            LearningScope::Repository,
            "run cargo fmt",
            Some("formatting"),
        );
        let shadowed = learning(
            "format-b",
            "format.command",
            LearningScope::Repository,
            "never run cargo fmt",
            Some("formatting"),
        );
        let result = retrieve_resolution(
            &resolution(vec![ResolvedLearning {
                winner,
                shadowed: vec![shadowed],
            }]),
            &context("format the repository", Some("formatting")),
            &RetrievalConfig::default(),
        );
        assert!(result.selected.is_empty());
        assert_eq!(
            result.considered[0].disposition,
            SelectionDisposition::Conflict
        );
    }

    #[test]
    fn high_impact_behavior_requires_task_bound_approval() {
        let item = learning(
            "deploy-rule",
            "workflow.deploy",
            LearningScope::User,
            "deploy production after verification",
            Some("release"),
        );
        let mut task = context("deploy the production release", Some("release"));
        let set = resolution(vec![ResolvedLearning {
            winner: item,
            shadowed: Vec::new(),
        }]);
        let blocked = retrieve_resolution(&set, &task, &RetrievalConfig::default());
        assert_eq!(
            blocked.considered[0].disposition,
            SelectionDisposition::ReviewRequired
        );
        task.approved_high_impact_ids
            .insert("deploy-rule".to_owned());
        assert_eq!(
            retrieve_resolution(&set, &task, &RetrievalConfig::default()).selected[0].learning_id,
            "deploy-rule"
        );
    }

    #[test]
    fn current_task_suppression_does_not_delete_behavior() {
        let item = learning(
            "testing-rule",
            "workflow.verification",
            LearningScope::User,
            "run verification tests before completion",
            Some("testing"),
        );
        let set = resolution(vec![ResolvedLearning {
            winner: item,
            shadowed: Vec::new(),
        }]);
        let mut suppressed = context("run verification tests", Some("testing"));
        suppressed
            .suppressed_learning_ids
            .insert("testing-rule".to_owned());
        assert!(
            retrieve_resolution(&set, &suppressed, &RetrievalConfig::default())
                .selected
                .is_empty()
        );
        assert_eq!(
            retrieve_resolution(
                &set,
                &context("run verification tests", Some("testing")),
                &RetrievalConfig::default(),
            )
            .selected[0]
                .learning_id,
            "testing-rule"
        );
    }

    #[test]
    fn large_corpus_is_bounded_deterministically() {
        let entries = (0..20_000)
            .map(|index| ResolvedLearning {
                winner: learning(
                    &format!("item-{index:05}"),
                    &format!("topic-{index:05}"),
                    LearningScope::User,
                    "unrelated behavior",
                    None,
                ),
                shadowed: Vec::new(),
            })
            .collect();
        let config = RetrievalConfig {
            max_considered: 512,
            ..RetrievalConfig::default()
        };
        let result = retrieve_resolution(
            &resolution(entries),
            &context("prepare a release", None),
            &config,
        );
        assert_eq!(result.considered.len(), 512);
        assert_eq!(result.truncated_count, 19_488);
    }

    #[test]
    fn evaluation_reports_precision_recall_and_false_positives() {
        let expected = learning(
            "expected",
            "workflow.verification",
            LearningScope::User,
            "run verification tests",
            Some("testing"),
        );
        let unrelated = learning(
            "unrelated",
            "response.style",
            LearningScope::User,
            "use a concise response style",
            None,
        );
        let case = EvaluationCase {
            resolution: resolution(vec![
                ResolvedLearning {
                    winner: expected,
                    shadowed: Vec::new(),
                },
                ResolvedLearning {
                    winner: unrelated,
                    shadowed: Vec::new(),
                },
            ]),
            context: context("run verification tests", Some("testing")),
            expected_selected_ids: BTreeSet::from(["expected".to_owned()]),
        };
        let evaluation = evaluate(&[case], &RetrievalConfig::default());
        assert_eq!(evaluation.true_positive, 1);
        assert_eq!(evaluation.false_positive, 0);
        assert_eq!(evaluation.false_negative, 0);
        assert_eq!(evaluation.precision_milli, 1_000);
        assert_eq!(evaluation.recall_milli, 1_000);
    }
}
