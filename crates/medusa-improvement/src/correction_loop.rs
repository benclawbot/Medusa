//! Durable correction-to-improvement orchestration.
//!
//! This module is the production seam between typed provenance and the canonical refinement
//! authority. It deliberately stops at an evaluated, reviewable candidate: approval and
//! activation remain explicit authority operations owned by the runtime.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    correction_signals::{ConversationTurn, CorrectionSignalDetector, LearningSignal},
    lesson_inference::{LessonCandidate, LessonInferenceEngine, LessonScope},
    provenance::{ProvenanceGraph, ProvenanceObservation, ProvenanceSource},
    refinement_authority::RefinementAuthorityStore,
    regression_replay::{
        ReplayBundle, ReplayBundleBuildError, ReplayBundleBuilder, ReplayDecision,
        ReplayEnvironment, ReplayObservation, ReplayReport, ReplayRunner, ReplayScenario,
        ReplayScenarioKind, ReplayValidator, supports_solution,
    },
    solution_selection::{GeneratedArtifact, SolutionProposal, SolutionSelector, SolutionType},
};
use medusa_context::refinement::{
    EvaluationResult, EvidenceKind, EvidenceRef, ProposerMetadata, RefinementArtifactKind,
    RefinementContent, RefinementProposal, RefinementRisk, RefinementScope,
};
use medusa_core::learning_policy::LearningAdmissionPolicy;
use serde::{Deserialize, Serialize};

const STATE_SCHEMA_VERSION: u32 = 1;
const MAX_EPISODES: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum CorrectionLoopError {
    #[error("correction loop I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("correction loop serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("replay bundle construction failed: {0}")]
    ReplayBundle(#[from] ReplayBundleBuildError),
    #[error("correction loop rejected the candidate: {0}")]
    Validation(String),
    #[error("canonical refinement authority rejected the candidate: {0}")]
    Authority(String),
}

#[derive(Clone, Debug)]
pub struct CorrectionLoopRequest {
    pub session_id: String,
    pub objective: String,
    pub turns: Vec<ConversationTurn>,
    pub provenance: ProvenanceGraph,
    pub policy: LearningAdmissionPolicy,
    pub repository_fixture: String,
    pub tool_capabilities: Vec<String>,
    pub now_unix_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionEpisodeState {
    Detected,
    Inferred,
    Selected,
    Evaluated,
    AwaitingReview,
    Blocked,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateArtifactType {
    Memory,
    RepositoryConvention,
    Workflow,
    PromptGuidance,
    EngineeringProposal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CandidateEvaluationPlan {
    pub baseline_case_ids: Vec<String>,
    pub candidate_case_ids: Vec<String>,
    pub negative_case_ids: Vec<String>,
    pub adjacent_case_ids: Vec<String>,
    pub oracle: String,
    pub cohort: String,
    pub same_cohort: bool,
    pub environment: ReplayEnvironment,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CandidateRollbackPlan {
    pub predecessor: Option<String>,
    pub stop_conditions: Vec<String>,
    pub rollback_action: String,
}

/// Versioned candidate contract emitted by the correction loop.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImprovementCandidate {
    pub id: String,
    pub version: u64,
    pub artifact_type: CandidateArtifactType,
    pub source_observation_ids: Vec<String>,
    pub root_trajectory_id: String,
    pub root_cause_hypothesis: String,
    pub generalized_rule: String,
    pub intended_scope: LessonScope,
    pub matching_predicates: Vec<String>,
    pub exclusions: Vec<String>,
    pub expiry_or_review_unix_ms: Option<i64>,
    pub alternatives_considered: Vec<SolutionType>,
    pub confidence_milli: u16,
    pub uncertainty: Vec<String>,
    pub safety_impact: String,
    pub artifact: Vec<GeneratedArtifact>,
    pub evaluation: CandidateEvaluationPlan,
    pub rollback: CandidateRollbackPlan,
    pub predecessor: Option<String>,
    pub conflicts: Vec<String>,
    pub evidence_links: Vec<String>,
    pub created_at_unix_ms: i64,
}

impl ImprovementCandidate {
    pub fn validate(&self) -> Result<(), CorrectionLoopError> {
        if self.id.trim().is_empty() {
            return Err(CorrectionLoopError::Validation(
                "candidate id must not be empty".to_owned(),
            ));
        }
        if self.source_observation_ids.is_empty() {
            return Err(CorrectionLoopError::Validation(
                "candidate must cite at least one source observation".to_owned(),
            ));
        }
        if self.root_cause_hypothesis.trim().is_empty() || self.generalized_rule.trim().is_empty() {
            return Err(CorrectionLoopError::Validation(
                "candidate must include root-cause and generalized-rule evidence".to_owned(),
            ));
        }
        if self.matching_predicates.is_empty() || self.exclusions.is_empty() {
            return Err(CorrectionLoopError::Validation(
                "candidate must define matching predicates and exclusions".to_owned(),
            ));
        }
        if self.artifact.is_empty() {
            return Err(CorrectionLoopError::Validation(
                "candidate must include a concrete behavior artifact".to_owned(),
            ));
        }
        if self.evaluation.baseline_case_ids.is_empty()
            || self.evaluation.candidate_case_ids.is_empty()
            || self.evaluation.negative_case_ids.is_empty()
            || self.evaluation.adjacent_case_ids.is_empty()
        {
            return Err(CorrectionLoopError::Validation(
                "candidate evaluation must include baseline, candidate, negative, and adjacent cases"
                    .to_owned(),
            ));
        }
        if !self.evaluation.same_cohort {
            return Err(CorrectionLoopError::Validation(
                "candidate evaluation must compare the same cohort".to_owned(),
            ));
        }
        if self.rollback.stop_conditions.is_empty() || self.rollback.rollback_action.trim().is_empty()
        {
            return Err(CorrectionLoopError::Validation(
                "candidate must define rollback stop conditions and an action".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CorrectionEpisode {
    pub id: String,
    pub session_id: String,
    pub signal_id: String,
    pub state: CorrectionEpisodeState,
    pub lesson: Option<LessonCandidate>,
    pub solution: Option<SolutionProposal>,
    pub replay_bundle: Option<ReplayBundle>,
    pub replay: Option<ReplayReport>,
    pub candidate: Option<ImprovementCandidate>,
    pub blocked_reasons: Vec<String>,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct CorrectionLoopState {
    schema_version: u32,
    episodes: Vec<CorrectionEpisode>,
}

impl Default for CorrectionLoopState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            episodes: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CorrectionEpisodeStore {
    path: PathBuf,
}

impl CorrectionEpisodeStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> Result<Vec<CorrectionEpisode>, CorrectionLoopError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(&self.path)?;
        let state: CorrectionLoopState = serde_json::from_slice(&bytes)?;
        if state.schema_version != STATE_SCHEMA_VERSION {
            return Err(CorrectionLoopError::Validation(format!(
                "unsupported correction-loop state schema version {}",
                state.schema_version
            )));
        }
        Ok(state.episodes)
    }

    pub fn put(&self, episode: CorrectionEpisode) -> Result<(), CorrectionLoopError> {
        let mut episodes = self.load()?;
        episodes.retain(|existing| existing.id != episode.id);
        episodes.push(episode);
        episodes.sort_by(|left, right| left.id.cmp(&right.id));
        if episodes.len() > MAX_EPISODES {
            let remove_count = episodes.len() - MAX_EPISODES;
            episodes.drain(0..remove_count);
        }
        let state = CorrectionLoopState {
            schema_version: STATE_SCHEMA_VERSION,
            episodes,
        };
        atomic_json(&self.path, &state)
    }
}

#[derive(Clone, Debug, Default)]
pub struct CorrectionLoopReport {
    pub signals: Vec<LearningSignal>,
    pub blocked_turns: Vec<usize>,
    pub episodes: Vec<CorrectionEpisode>,
}

#[derive(Clone, Debug, Default)]
pub struct CorrectionLoop {
    detector: CorrectionSignalDetector,
    inference: LessonInferenceEngine,
    selector: SolutionSelector,
    replay_builder: ReplayBundleBuilder,
    replay_validator: ReplayValidator,
}

impl CorrectionLoop {
    pub fn run<R: ReplayRunner>(
        &self,
        request: &CorrectionLoopRequest,
        runner: &R,
        repo: &RefinementAuthorityStore,
        store: &CorrectionEpisodeStore,
    ) -> Result<CorrectionLoopReport, CorrectionLoopError> {
        let batch = self.detector.detect(&request.turns, &request.policy);
        let mut episodes = Vec::new();

        for signal in &batch.signals {
            let signal_id = signal.id.clone();
            let source_observation_ids = request
                .provenance
                .observations
                .iter()
                .filter(|observation| observation.source_turn == signal.source_turn)
                .filter(|observation| observation.learning_eligible)
                .filter(|observation| observation.privacy_approved)
                .map(|observation| observation.id.clone())
                .collect::<Vec<_>>();
            let lesson = self.inference.infer(signal);
            let solution = self.selector.propose(&lesson);
            let mut episode = CorrectionEpisode {
                id: format!("correction-{}", signal.id),
                session_id: request.session_id.clone(),
                signal_id,
                state: CorrectionEpisodeState::Detected,
                lesson: Some(lesson.clone()),
                solution: Some(solution.clone()),
                replay_bundle: None,
                replay: None,
                candidate: None,
                blocked_reasons: Vec::new(),
                created_at_unix_ms: request.now_unix_ms,
                updated_at_unix_ms: request.now_unix_ms,
            };

            episode.state = CorrectionEpisodeState::Inferred;
            if lesson.promotion_blocked {
                episode.state = CorrectionEpisodeState::Blocked;
                episode.blocked_reasons = lesson.uncertainty.clone();
                episode.updated_at_unix_ms = request.now_unix_ms;
                store.put(episode.clone())?;
                episodes.push(episode);
                continue;
            }

            if source_observation_ids.is_empty() {
                episode.state = CorrectionEpisodeState::Blocked;
                episode.blocked_reasons.push(
                    "no privacy-approved user-correction observation is linked to this session"
                        .to_owned(),
                );
                store.put(episode.clone())?;
                episodes.push(episode);
                continue;
            }
            if solution.selected.contains(&SolutionType::NoPersistence)
                || solution
                    .selected
                    .iter()
                    .any(|kind| !supports_solution(*kind))
            {
                episode.state = CorrectionEpisodeState::Blocked;
                episode.blocked_reasons.push(
                    "scope, confidence, contradiction, or solution policy blocks durable behavior"
                        .to_owned(),
                );
                store.put(episode.clone())?;
                episodes.push(episode);
                continue;
            }

            episode.state = CorrectionEpisodeState::Selected;
            let bundle = self.replay_builder.build(
                &lesson,
                &solution,
                &request.repository_fixture,
                request.tool_capabilities.clone(),
            )?;
            let replay = self.replay_validator.validate(&bundle, &solution, runner);
            episode.replay_bundle = Some(bundle);
            episode.replay = Some(replay.clone());
            if replay.decision != ReplayDecision::Validated {
                episode.state = CorrectionEpisodeState::Blocked;
                episode.blocked_reasons = replay.diagnostics.clone();
                episode.updated_at_unix_ms = request.now_unix_ms;
                store.put(episode.clone())?;
                episodes.push(episode);
                continue;
            }

            let candidate = build_candidate(
                &lesson,
                &solution,
                &source_observation_ids,
                &request.provenance,
                &request.tool_capabilities,
                &request.repository_fixture,
                request.now_unix_ms,
            )?;
            candidate.validate()?;
            episode.candidate = Some(candidate.clone());
            episode.state = CorrectionEpisodeState::Evaluated;
            publish_evaluated(repo, &candidate, &lesson, &request.provenance, &replay)?;
            episode.state = CorrectionEpisodeState::AwaitingReview;
            episode.updated_at_unix_ms = request.now_unix_ms;
            store.put(episode.clone())?;
            episodes.push(episode);
        }

        Ok(CorrectionLoopReport {
            signals: batch.signals,
            blocked_turns: batch.blocked_turns,
            episodes,
        })
    }
}

fn build_candidate(
    lesson: &LessonCandidate,
    solution: &SolutionProposal,
    source_observation_ids: &[String],
    provenance: &ProvenanceGraph,
    tool_capabilities: &[String],
    repository_fixture: &str,
    now_unix_ms: i64,
) -> Result<ImprovementCandidate, CorrectionLoopError> {
    let regression_case_ids = lesson
        .regression_examples
        .iter()
        .enumerate()
        .map(|(index, _)| format!("origin-{index}"))
        .collect::<Vec<_>>();
    let baseline_case_ids = if regression_case_ids.is_empty() {
        vec!["origin-0".to_owned()]
    } else {
        regression_case_ids.clone()
    };
    let negative_case_ids = lesson
        .non_applicable_contexts
        .iter()
        .enumerate()
        .map(|(index, _)| format!("negative-{index}"))
        .collect::<Vec<_>>();
    let adjacent_case_ids = vec!["critical-safety".to_owned()];
    let root_trajectory_id = provenance
        .observations
        .iter()
        .find(|observation| source_observation_ids.contains(&observation.id))
        .map(|observation| observation.trajectory_id.clone())
        .unwrap_or_else(|| "unknown".to_owned());
    let evidence_links = source_observation_ids
        .iter()
        .map(|id| format!("provenance://observation/{id}"))
        .collect::<Vec<_>>();
    let matching_predicates = vec![format!("lesson_scope={:?}", lesson.scope).to_ascii_lowercase()];
    let exclusions = if lesson.non_applicable_contexts.is_empty() {
        vec!["outside explicitly matched correction contexts".to_owned()]
    } else {
        lesson.non_applicable_contexts.clone()
    };
    let safety_impact = if tool_capabilities.iter().any(|tool| tool == "write") {
        "candidate may influence repository writes; replay and explicit review are mandatory"
            .to_owned()
    } else {
        "candidate is constrained to non-writing behavior until explicit review".to_owned()
    };

    Ok(ImprovementCandidate {
        id: format!("candidate-{}", lesson.id),
        version: 1,
        artifact_type: candidate_artifact_type(solution),
        source_observation_ids: source_observation_ids.to_vec(),
        root_trajectory_id,
        root_cause_hypothesis: lesson.root_cause.clone(),
        generalized_rule: lesson.generalized_rule.clone(),
        intended_scope: lesson.scope,
        matching_predicates,
        exclusions,
        expiry_or_review_unix_ms: Some(now_unix_ms.saturating_add(30 * 24 * 60 * 60 * 1_000)),
        alternatives_considered: solution.selected.clone(),
        confidence_milli: lesson.confidence_milli,
        uncertainty: lesson.uncertainty.clone(),
        safety_impact,
        artifact: solution.artifacts.clone(),
        evaluation: CandidateEvaluationPlan {
            baseline_case_ids: baseline_case_ids.clone(),
            candidate_case_ids: baseline_case_ids,
            negative_case_ids,
            adjacent_case_ids,
            oracle: "candidate must fix the originating behavior without over-triggering or safety regressions"
                .to_owned(),
            cohort: format!("session:{}", provenance.session_id),
            same_cohort: true,
            environment: ReplayEnvironment::default(),
        },
        rollback: CandidateRollbackPlan {
            predecessor: None,
            stop_conditions: vec![
                "candidate fails deterministic replay".to_owned(),
                "candidate over-triggers outside intended scope".to_owned(),
                "critical safety behavior regresses".to_owned(),
            ],
            rollback_action: "deactivate candidate and restore predecessor authority".to_owned(),
        },
        predecessor: None,
        conflicts: Vec::new(),
        evidence_links,
        created_at_unix_ms: now_unix_ms,
    })
}

fn candidate_artifact_type(solution: &SolutionProposal) -> CandidateArtifactType {
    if solution.selected.contains(&SolutionType::RepositoryMemory) {
        CandidateArtifactType::RepositoryConvention
    } else if solution.selected.contains(&SolutionType::WorkflowGate) {
        CandidateArtifactType::Workflow
    } else if solution.selected.contains(&SolutionType::ProductCodeChange) {
        CandidateArtifactType::EngineeringProposal
    } else if solution.selected.contains(&SolutionType::UserPreference) {
        CandidateArtifactType::Memory
    } else {
        CandidateArtifactType::PromptGuidance
    }
}

fn publish_evaluated(
    repo: &RefinementAuthorityStore,
    candidate: &ImprovementCandidate,
    lesson: &LessonCandidate,
    provenance: &ProvenanceGraph,
    replay: &ReplayReport,
) -> Result<(), CorrectionLoopError> {
    let content = RefinementContent {
        title: format!("Correction candidate {}", candidate.id),
        summary: candidate.generalized_rule.clone(),
        body: serde_json::to_string_pretty(candidate)?,
        tags: vec![
            "correction-loop".to_owned(),
            format!("lesson-scope:{:?}", lesson.scope).to_ascii_lowercase(),
        ],
    };
    let scope = match candidate.intended_scope {
        LessonScope::UserPreference => RefinementScope::Global,
        LessonScope::RepositorySpecific => RefinementScope::Repository {
            repository: provenance.repository.clone(),
        },
        LessonScope::DomainGeneral => RefinementScope::Repository {
            repository: provenance.repository.clone(),
        },
    };
    let proposal = RefinementProposal {
        id: candidate.id.clone(),
        artifact: RefinementArtifactKind::Candidate,
        scope,
        proposer: ProposerMetadata {
            proposer: "correction-loop".to_owned(),
            session_id: Some(provenance.session_id.clone()),
            turn_id: None,
        },
        rationale: format!(
            "{}; deterministic replay decision={:?}",
            lesson.rationale, replay.decision
        ),
        evidence: candidate
            .evidence_links
            .iter()
            .map(|reference| EvidenceRef {
                reference: reference.clone(),
                kind: EvidenceKind::Provenance,
            })
            .collect(),
        uncertainty: candidate.uncertainty.clone(),
        risk: RefinementRisk::Elevated,
        content,
        predecessor: candidate.predecessor.clone(),
        supersedes: Vec::new(),
        conflicts: candidate.conflicts.clone(),
        proposed_at_ms: candidate.created_at_unix_ms,
        actor: "correction-loop".to_owned(),
        evidence_hash: String::new(),
    };
    repo.submit_proposal(proposal)
        .map_err(|error| CorrectionLoopError::Authority(error.to_string()))?;
    repo.record_evaluation(EvaluationResult {
        candidate_id: candidate.id.clone(),
        passed: replay.decision == ReplayDecision::Validated,
        confidence_bps: u32::from(candidate.confidence_milli) * 10,
        baseline_summary: format!(
            "{} replay comparisons",
            candidate.evaluation.baseline_case_ids.len()
        ),
        candidate_summary: format!("decision={:?}", replay.decision),
        evidence_hash: String::new(),
        evaluated_at_ms: candidate.created_at_unix_ms,
        evaluator: "correction-loop".to_owned(),
    })
    .map_err(|error| CorrectionLoopError::Authority(error.to_string()))?;
    Ok(())
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), CorrectionLoopError> {
    let parent = path
        .parent()
        .ok_or_else(|| CorrectionLoopError::Validation("state path has no parent".to_owned()))?;
    fs::create_dir_all(parent)?;
    let tmp = path.with_extension(format!("tmp-{}", ulid::Ulid::new()));
    fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::correction_signals::TurnAuthor;
    use crate::provenance::ProvenanceEdge;
    use tempfile::tempdir;

    #[derive(Default)]
    struct PassingReplay;

    impl ReplayRunner for PassingReplay {
        fn run(
            &self,
            scenario: &ReplayScenario,
            candidate: Option<&SolutionProposal>,
            _environment: &ReplayEnvironment,
        ) -> ReplayObservation {
            let behavior = if scenario.kind == ReplayScenarioKind::OriginatingFailure
                && candidate.is_none()
            {
                "baseline failure".to_owned()
            } else {
                scenario.expected_behavior.clone()
            };
            ReplayObservation {
                behavior,
                candidate_triggered: candidate.is_some() && scenario.candidate_should_trigger,
                critical_safety_passed: true,
                evidence_links: vec![format!("evidence://{}", scenario.id)],
                metrics: Default::default(),
            }
        }
    }

    fn request() -> CorrectionLoopRequest {
        let turns = vec![
            ConversationTurn {
                author: TurnAuthor::Assistant,
                text: "I updated just one fixture.".to_owned(),
                tool_backed: false,
            },
            ConversationTurn {
                author: TurnAuthor::User,
                text: "No, that is wrong. You missed the other authoritative fixtures.".to_owned(),
                tool_backed: false,
            },
        ];
        let mut provenance = ProvenanceGraph::new("session-1", "repo");
        provenance
            .add_observation(ProvenanceObservation {
                id: "obs-user".to_owned(),
                trajectory_id: "trajectory-1".to_owned(),
                source_turn: 1,
                source: ProvenanceSource::User,
                summary: "user corrected incomplete fixture inventory".to_owned(),
                learning_eligible: true,
                privacy_approved: true,
            })
            .unwrap();
        provenance
            .add_observation(ProvenanceObservation {
                id: "obs-assistant".to_owned(),
                trajectory_id: "trajectory-1".to_owned(),
                source_turn: 0,
                source: ProvenanceSource::Assistant,
                summary: "assistant made incomplete change".to_owned(),
                learning_eligible: false,
                privacy_approved: true,
            })
            .unwrap();
        provenance
            .add_edge(ProvenanceEdge {
                from: "obs-assistant".to_owned(),
                to: "obs-user".to_owned(),
                relation: "corrected_by".to_owned(),
            })
            .unwrap();
        CorrectionLoopRequest {
            session_id: "session-1".to_owned(),
            objective: "complete fixture inventory".to_owned(),
            turns,
            provenance,
            policy: LearningAdmissionPolicy {
                enabled: true,
                ..LearningAdmissionPolicy::default()
            },
            repository_fixture: "fixture token=secret".to_owned(),
            tool_capabilities: vec!["read".to_owned()],
            now_unix_ms: 100,
        }
    }

    #[test]
    fn correction_loop_emits_reviewable_candidate_with_provenance() {
        let dir = tempdir().unwrap();
        let repo = RefinementAuthorityStore::open(dir.path().join("refinement")).unwrap();
        let store = CorrectionEpisodeStore::new(dir.path().join("correction-loop.json"));
        let report = CorrectionLoop::default()
            .run(&request(), &PassingReplay, &repo, &store)
            .unwrap();
        assert_eq!(report.episodes.len(), 1);
        let episode = &report.episodes[0];
        assert_eq!(episode.state, CorrectionEpisodeState::AwaitingReview);
        let candidate = episode.candidate.as_ref().unwrap();
        assert_eq!(candidate.source_observation_ids, vec!["obs-user"]);
        assert_eq!(candidate.root_trajectory_id, "trajectory-1");
        assert_eq!(candidate.evaluation.cohort, "session:session-1");
        assert!(candidate.evaluation.same_cohort);
        assert!(
            candidate
                .evaluation
                .negative_case_ids
                .iter()
                .any(|case| case.starts_with("negative-"))
        );
        let queue = repo.review_queue().unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].proposal.id, candidate.id);
        assert_eq!(
            queue[0].proposal.lifecycle,
            medusa_context::refinement::RefinementLifecycle::Evaluated
        );
    }

    #[test]
    fn missing_learning_provenance_blocks_persistence() {
        let dir = tempdir().unwrap();
        let repo = RefinementAuthorityStore::open(dir.path().join("refinement")).unwrap();
        let store = CorrectionEpisodeStore::new(dir.path().join("correction-loop.json"));
        let mut request = request();
        request.provenance.observations[0].privacy_approved = false;
        let report = CorrectionLoop::default()
            .run(&request, &PassingReplay, &repo, &store)
            .unwrap();
        assert_eq!(report.episodes.len(), 1);
        assert_eq!(report.episodes[0].state, CorrectionEpisodeState::Blocked);
        assert!(report.episodes[0].candidate.is_none());
    }
}
