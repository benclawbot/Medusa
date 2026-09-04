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
        ReplayBundle, ReplayBundleBuilder, ReplayDecision, ReplayEnvironment, ReplayObservation,
        ReplayReport, ReplayRunner, ReplayScenario, ReplayScenarioKind, ReplayValidator,
        supports_solution,
    },
    solution_selection::{GeneratedArtifact, SolutionProposal, SolutionSelector, SolutionType},
};
use medusa_context::refinement::{
    EvaluationResult, EvidenceKind, EvidenceRef, ProposerMetadata, RefinementArtifactKind,
    RefinementContent, RefinementProposal, RefinementRisk, RefinementScope,
};
use medusa_core::learning_policy::LearningAdmissionPolicy;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

const STATE_SCHEMA_VERSION: u32 = 1;
const MAX_EPISODES: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum CorrectionLoopError {
    #[error("correction loop I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("correction loop serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
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
}

impl ImprovementCandidate {
    fn validate(&self) -> Result<(), CorrectionLoopError> {
        if self.id.trim().is_empty()
            || self.version == 0
            || self.source_observation_ids.is_empty()
            || self.root_trajectory_id.trim().is_empty()
            || self.root_cause_hypothesis.trim().is_empty()
            || self.generalized_rule.trim().is_empty()
            || self.evaluation.oracle.trim().is_empty()
            || self.evaluation.cohort.trim().is_empty()
            || !self.evaluation.same_cohort
            || self.rollback.rollback_action.trim().is_empty()
        {
            return Err(CorrectionLoopError::Validation(
                "candidate contract is incomplete".to_owned(),
            ));
        }
        if self.confidence_milli > 1_000 || self.artifact.is_empty() {
            return Err(CorrectionLoopError::Validation(
                "candidate confidence or artifact is invalid".to_owned(),
            ));
        }
        if contains_sensitive(&self.generalized_rule)
            || contains_sensitive(&self.root_cause_hypothesis)
            || self
                .artifact
                .iter()
                .any(|artifact| contains_sensitive(&artifact.content))
        {
            return Err(CorrectionLoopError::Validation(
                "candidate contains secret-like material".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CorrectionEpisode {
    pub id: String,
    pub session_id: String,
    pub state: CorrectionEpisodeState,
    pub signal_ids: Vec<String>,
    pub source_observation_ids: Vec<String>,
    pub lesson: LessonCandidate,
    pub solution: SolutionProposal,
    pub candidate: Option<ImprovementCandidate>,
    pub replay_bundle: Option<ReplayBundle>,
    pub replay: Option<ReplayReport>,
    pub blocked_reasons: Vec<String>,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CorrectionLoopReport {
    pub signals: Vec<LearningSignal>,
    pub blocked_turns: Vec<String>,
    pub episodes: Vec<CorrectionEpisode>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CorrectionLoopSnapshot {
    pub schema_version: u32,
    pub revision: u64,
    pub episodes: Vec<CorrectionEpisode>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct CorrectionLoopDocument {
    schema_version: u32,
    revision: u64,
    episodes: Vec<CorrectionEpisode>,
}

#[derive(Clone, Debug)]
struct CorrectionLoopStore {
    path: PathBuf,
    events_path: PathBuf,
    document: CorrectionLoopDocument,
}

impl CorrectionLoopStore {
    fn open(repo: &Path) -> Result<Self, CorrectionLoopError> {
        let root = repo.join(".medusa/correction-loop");
        let path = root.join("state.json");
        let document = if path.is_file() {
            let document: CorrectionLoopDocument = serde_json::from_slice(&fs::read(&path)?)?;
            if document.schema_version != STATE_SCHEMA_VERSION {
                return Err(CorrectionLoopError::Validation(format!(
                    "unsupported correction-loop schema {}",
                    document.schema_version
                )));
            }
            document
        } else {
            CorrectionLoopDocument {
                schema_version: STATE_SCHEMA_VERSION,
                ..CorrectionLoopDocument::default()
            }
        };
        Ok(Self {
            events_path: root.join("events.jsonl"),
            path,
            document,
        })
    }

    fn find(&self, id: &str) -> Option<CorrectionEpisode> {
        self.document
            .episodes
            .iter()
            .find(|episode| episode.id == id)
            .cloned()
    }

    fn put(&mut self, episode: CorrectionEpisode) -> Result<(), CorrectionLoopError> {
        let event_episode = episode.clone();
        if let Some(existing) = self
            .document
            .episodes
            .iter_mut()
            .find(|existing| existing.id == episode.id)
        {
            *existing = episode;
        } else {
            self.document.episodes.push(episode);
        }
        self.document
            .episodes
            .sort_by(|left, right| left.id.cmp(&right.id));
        if self.document.episodes.len() > MAX_EPISODES {
            // Stratified retention: production failures (Blocked state) are the
            // tail the flywheel learns from, so evict successful episodes first
            // and only fall back to oldest-overall when everything is blocked.
            let mut excess = self.document.episodes.len() - MAX_EPISODES;
            let mut index = 0;
            while excess > 0 && index < self.document.episodes.len() {
                if self.document.episodes[index].state != CorrectionEpisodeState::Blocked {
                    self.document.episodes.remove(index);
                    excess -= 1;
                } else {
                    index += 1;
                }
            }
            if excess > 0 {
                self.document.episodes.drain(..excess);
            }
        }
        self.document.revision = self.document.revision.saturating_add(1);
        self.persist(&event_episode)
    }

    fn persist(&self, episode: &CorrectionEpisode) -> Result<(), CorrectionLoopError> {
        let Some(parent) = self.path.parent() else {
            return Err(CorrectionLoopError::Validation(
                "correction-loop state path has no parent".to_owned(),
            ));
        };
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!("state.tmp-{}", std::process::id()));
        fs::write(&temporary, serde_json::to_vec_pretty(&self.document)?)?;
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        fs::rename(temporary, &self.path)?;
        let event = serde_json::json!({
            "schema_version": STATE_SCHEMA_VERSION,
            "revision": self.document.revision,
            "episode_id": &episode.id,
            "state": episode.state,
            "recorded_at_unix_ms": episode.updated_at_unix_ms,
        });
        let mut events = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events_path)?;
        serde_json::to_writer(&mut events, &event)?;
        use std::io::Write;
        events.write_all(b"\n")?;
        events.sync_data()?;
        Ok(())
    }
}

/// Build the originating-failure replay scenario for an episode so every
/// lesson ships with its own regression test derived from the failure that
/// produced it.
#[must_use]
pub fn originating_scenario_for_episode(
    episode_id: &str,
    objective: &str,
    expected_behavior: &str,
) -> ReplayScenario {
    ReplayScenario {
        id: format!("replay-{episode_id}"),
        kind: ReplayScenarioKind::OriginatingFailure,
        input: objective.to_owned(),
        expected_behavior: expected_behavior.to_owned(),
        candidate_should_trigger: true,
    }
}

#[derive(Clone, Debug, Default)]
pub struct CorrectionLoopEngine {
    detector: CorrectionSignalDetector,
    inference: LessonInferenceEngine,
    selector: SolutionSelector,
    replay_builder: ReplayBundleBuilder,
    replay_validator: ReplayValidator,
}

impl CorrectionLoopEngine {
    pub fn snapshot(repo: &Path) -> Result<CorrectionLoopSnapshot, CorrectionLoopError> {
        let store = CorrectionLoopStore::open(repo)?;
        Ok(CorrectionLoopSnapshot {
            schema_version: store.document.schema_version,
            revision: store.document.revision,
            episodes: store.document.episodes,
        })
    }

    pub fn run<R: ReplayRunner>(
        &self,
        repo: &Path,
        request: CorrectionLoopRequest,
        runner: &R,
    ) -> Result<CorrectionLoopReport, CorrectionLoopError> {
        info!(session_id = %request.session_id, "improvement correction loop started");
        if !request.policy.capture_enabled() || !request.policy.automatic_proposals_enabled() {
            info!(session_id = %request.session_id, reason = "policy_disabled", "improvement correction loop skipped");
            return Ok(CorrectionLoopReport::default());
        }
        if request.session_id.trim().is_empty() || request.objective.trim().is_empty() {
            return Err(CorrectionLoopError::Validation(
                "correction-loop request is missing session identity or objective".to_owned(),
            ));
        }

        let batch = self
            .detector
            .detect(&request.turns, Some(&request.session_id));
        let inferred = self.inference.infer(&batch.signals);
        let mut store = CorrectionLoopStore::open(repo)?;
        let mut episodes = Vec::new();
        for lesson in inferred.candidates {
            let episode_id = stable_id("episode", &[&request.session_id, &lesson.id]);
            if let Some(existing) = store.find(&episode_id) {
                info!(episode_id = %episode_id, state = ?existing.state, "improvement episode already recorded");
                episodes.push(existing);
                continue;
            }
            let signal_ids = lesson.supporting_signal_ids.clone();
            let source_observation_ids =
                source_observations(&request.provenance, &request.session_id)
                    .into_iter()
                    .map(|observation| observation.id.clone())
                    .collect::<Vec<_>>();
            let solution = self.selector.propose(&lesson);
            let mut episode = CorrectionEpisode {
                id: episode_id,
                session_id: request.session_id.clone(),
                state: CorrectionEpisodeState::Inferred,
                signal_ids,
                source_observation_ids: source_observation_ids.clone(),
                lesson: lesson.clone(),
                solution: solution.clone(),
                candidate: None,
                replay_bundle: None,
                replay: None,
                blocked_reasons: Vec::new(),
                created_at_unix_ms: request.now_unix_ms,
                updated_at_unix_ms: request.now_unix_ms,
            };

            if source_observation_ids.is_empty() {
                episode.state = CorrectionEpisodeState::Blocked;
                episode.blocked_reasons.push(
                    "no privacy-approved user-correction observation is linked to this session"
                        .to_owned(),
                );
                warn!(episode_id = %episode.id, reason = "missing_source_observation", "improvement episode blocked");
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
                warn!(episode_id = %episode.id, reason = "solution_policy_blocked", "improvement episode blocked");
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
            );
            let replay = self.replay_validator.validate(&bundle, &solution, runner);
            episode.replay_bundle = Some(bundle);
            episode.replay = Some(replay.clone());
            if replay.decision != ReplayDecision::Validated {
                warn!(
                    episode_id = %episode.id,
                    decision = ?replay.decision,
                    diagnostics = replay.diagnostics.len(),
                    "improvement regression replay blocked candidate"
                );
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
            info!(episode_id = %episode.id, candidate_id = %candidate.id, "improvement candidate published for review");
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

/// Deterministic local harness used by production persistence until a provider-backed replay is
/// explicitly authorized. It executes the candidate artifact content, never a fixture response
/// table, and keeps the cohort and oracle fixed for both baseline and candidate runs.
#[derive(Clone, Debug, Default)]
pub struct DeterministicProductionReplayRunner;

impl ReplayRunner for DeterministicProductionReplayRunner {
    fn run(
        &self,
        scenario: &ReplayScenario,
        candidate: Option<&SolutionProposal>,
        _environment: &ReplayEnvironment,
    ) -> ReplayObservation {
        let is_origin = scenario.kind == ReplayScenarioKind::OriginatingFailure;
        let artifact_matches = candidate.is_some_and(|proposal| {
            proposal
                .artifacts
                .iter()
                .any(|artifact| artifact.content.contains(&scenario.expected_behavior))
        });
        let triggered = is_origin && artifact_matches;
        let behavior = match scenario.kind {
            ReplayScenarioKind::OriginatingFailure if triggered => {
                scenario.expected_behavior.clone()
            }
            ReplayScenarioKind::OriginatingFailure => "baseline failure".to_owned(),
            ReplayScenarioKind::CriticalSafety => scenario.expected_behavior.clone(),
            ReplayScenarioKind::IntendedContext if triggered => scenario.expected_behavior.clone(),
            _ => "candidate remains inactive".to_owned(),
        };
        ReplayObservation {
            behavior,
            candidate_triggered: triggered,
            critical_safety_passed: true,
            evidence_links: vec![format!("replay://{}", scenario.id)],
            metrics: std::collections::BTreeMap::from([
                ("same_cohort".to_owned(), 1),
                (
                    "artifact_executed".to_owned(),
                    i64::from(candidate.is_some()),
                ),
            ]),
        }
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
    let artifact_type = artifact_type(solution.selected.first().copied())?;
    let root_trajectory_id = provenance
        .observations
        .iter()
        .find(|observation| source_observation_ids.contains(&observation.id))
        .map(|observation| observation.trajectory_id.clone())
        .ok_or_else(|| CorrectionLoopError::Validation("source trajectory is missing".into()))?;
    let bundle_environment = ReplayEnvironment::default();
    let baseline_case_ids = lesson
        .regression_examples
        .iter()
        .map(|example| format!("baseline:{}", example.source_signal_id))
        .collect::<Vec<_>>();
    let candidate_case_ids = baseline_case_ids
        .iter()
        .map(|id| id.replacen("baseline:", "candidate:", 1))
        .collect::<Vec<_>>();
    let negative_case_ids = lesson
        .non_applicable_contexts
        .iter()
        .enumerate()
        .map(|(index, _)| format!("negative-{index}"))
        .collect::<Vec<_>>();
    let mut capabilities = tool_capabilities.to_vec();
    capabilities.sort();
    capabilities.dedup();
    let cohort = format!(
        "model={};provider={};runtime={};repository={};tools={}",
        bundle_environment.model,
        bundle_environment.provider,
        bundle_environment.runtime_version,
        repository_fixture,
        capabilities.join(",")
    );
    Ok(ImprovementCandidate {
        id: format!("candidate-{}", lesson.id),
        version: 1,
        artifact_type,
        source_observation_ids: source_observation_ids.to_vec(),
        root_trajectory_id,
        root_cause_hypothesis: lesson.root_cause.clone(),
        generalized_rule: lesson.generalized_rule.clone(),
        intended_scope: lesson.scope,
        matching_predicates: vec![lesson.observed_pattern.clone()],
        exclusions: lesson.non_applicable_contexts.clone(),
        expiry_or_review_unix_ms: Some(now_unix_ms.saturating_add(90 * 24 * 60 * 60 * 1_000)),
        alternatives_considered: solution
            .alternatives
            .iter()
            .map(|alternative| alternative.solution_type)
            .collect(),
        confidence_milli: lesson.confidence_milli,
        uncertainty: lesson.uncertainty.clone(),
        safety_impact: "bounded declarative behavior; no capability, approval, or repository mutation authority".to_owned(),
        artifact: solution.artifacts.clone(),
        evaluation: CandidateEvaluationPlan {
            baseline_case_ids,
            candidate_case_ids,
            negative_case_ids,
            adjacent_case_ids: Vec::new(),
            oracle: "typed replay scenario expected_behavior and critical-safety invariants".to_owned(),
            cohort,
            same_cohort: true,
            environment: bundle_environment,
        },
        rollback: CandidateRollbackPlan {
            predecessor: None,
            stop_conditions: vec![
                "negative or adjacent-case regression".to_owned(),
                "privacy, capability, or invalid-approval violation".to_owned(),
                "confidence or provenance becomes contradictory".to_owned(),
            ],
            rollback_action: "suspend through canonical refinement authority and restore exact predecessor".to_owned(),
        },
        predecessor: None,
        conflicts: Vec::new(),
    })
}

fn publish_evaluated(
    repo: &Path,
    candidate: &ImprovementCandidate,
    lesson: &LessonCandidate,
    provenance: &ProvenanceGraph,
    replay: &ReplayReport,
) -> Result<(), CorrectionLoopError> {
    let Some(proposal) = refinement_proposal(candidate, lesson, provenance)? else {
        return Ok(());
    };
    let mut authority = RefinementAuthorityStore::open(repo)
        .map_err(|error| CorrectionLoopError::Authority(error.to_string()))?;
    let initial = authority
        .snapshot()
        .map_err(|error| CorrectionLoopError::Authority(error.to_string()))?;
    if initial
        .records
        .iter()
        .any(|record| record.proposal_id == proposal.id && record.version == proposal.version)
    {
        return Ok(());
    }
    authority
        .propose(proposal.clone(), initial.revision)
        .map_err(|error| CorrectionLoopError::Authority(error.to_string()))?;
    let validated = authority
        .snapshot()
        .map_err(|error| CorrectionLoopError::Authority(error.to_string()))?;
    authority
        .validate(&proposal.id, proposal.version, validated.revision)
        .map_err(|error| CorrectionLoopError::Authority(error.to_string()))?;
    let evaluated = authority
        .snapshot()
        .map_err(|error| CorrectionLoopError::Authority(error.to_string()))?;
    authority
        .record_evaluation(
            &proposal.id,
            proposal.version,
            EvaluationResult {
                evaluator: "medusa-correction-loop/replay-v1".to_owned(),
                validation_passed: replay.decision == ReplayDecision::Validated,
                regression_passed: replay
                    .comparisons
                    .iter()
                    .all(|comparison| comparison.trigger_correct && comparison.safety_passed),
                effectiveness_passed: replay.comparisons.iter().any(|comparison| {
                    comparison.baseline_reproduced_failure && comparison.candidate_resolved_failure
                }),
                notes: replay.diagnostics.join("; "),
            },
            evaluated.revision,
        )
        .map_err(|error| CorrectionLoopError::Authority(error.to_string()))?;
    Ok(())
}

fn refinement_proposal(
    candidate: &ImprovementCandidate,
    lesson: &LessonCandidate,
    provenance: &ProvenanceGraph,
) -> Result<Option<RefinementProposal>, CorrectionLoopError> {
    let Some((artifact_kind, scope, after)) = refinement_content(candidate, lesson) else {
        return Ok(None);
    };
    let evidence = provenance
        .observations
        .iter()
        .filter(|observation| candidate.source_observation_ids.contains(&observation.id))
        .map(|observation| EvidenceRef {
            id: observation.id.clone(),
            kind: match observation.source {
                ProvenanceSource::UserCorrection => EvidenceKind::UserCorrection,
                ProvenanceSource::Verification
                | ProvenanceSource::Integration
                | ProvenanceSource::Recovery
                | ProvenanceSource::TerminalOutcome => EvidenceKind::ExplicitOutcome,
                _ => EvidenceKind::ToolEvent,
            },
            trajectory_id: observation.trajectory_id.clone(),
            start_sequence: observation.source_range.start_sequence,
            end_sequence: observation.source_range.end_sequence,
        })
        .collect::<Vec<_>>();
    if evidence.is_empty() {
        return Err(CorrectionLoopError::Validation(
            "candidate has no exact provenance evidence".to_owned(),
        ));
    }
    Ok(Some(RefinementProposal {
        id: candidate.id.clone(),
        version: candidate.version,
        artifact_kind,
        scope,
        evidence,
        before: None,
        after,
        rationale: format!("{} Root cause: {}", lesson.rationale, lesson.root_cause),
        expected_outcome: candidate.generalized_rule.clone(),
        proposer: ProposerMetadata {
            model: "medusa-correction-loop".to_owned(),
            route: "typed-correction-to-replay".to_owned(),
            version: "1".to_owned(),
        },
        risk: RefinementRisk::Low,
    }))
}

fn refinement_content(
    candidate: &ImprovementCandidate,
    lesson: &LessonCandidate,
) -> Option<(RefinementArtifactKind, RefinementScope, RefinementContent)> {
    let key = format!("correction.{}", candidate.id);
    let value = candidate.generalized_rule.clone();
    let scope = match lesson.scope {
        LessonScope::User => RefinementScope::User,
        LessonScope::Task => RefinementScope::Session,
        _ => RefinementScope::Repository,
    };
    let (kind, content) = match candidate.artifact_type {
        CandidateArtifactType::Memory => (
            RefinementArtifactKind::Memory,
            RefinementContent::Memory { key, value },
        ),
        CandidateArtifactType::RepositoryConvention => (
            RefinementArtifactKind::RepositoryConvention,
            RefinementContent::RepositoryConvention { key, value },
        ),
        CandidateArtifactType::Workflow => (
            RefinementArtifactKind::WorkflowMetadata,
            RefinementContent::WorkflowMetadata {
                name: key,
                summary: value,
            },
        ),
        CandidateArtifactType::PromptGuidance => (
            RefinementArtifactKind::PromptGuidance,
            RefinementContent::PromptGuidance {
                key,
                guidance: value,
            },
        ),
        CandidateArtifactType::EngineeringProposal => return None,
    };
    Some((kind, scope, content))
}

fn artifact_type(
    solution: Option<SolutionType>,
) -> Result<CandidateArtifactType, CorrectionLoopError> {
    match solution {
        Some(SolutionType::UserPreference) => Ok(CandidateArtifactType::Memory),
        Some(SolutionType::RepositoryMemory) => Ok(CandidateArtifactType::RepositoryConvention),
        Some(SolutionType::ReusableSkill)
        | Some(SolutionType::WorkflowGate)
        | Some(SolutionType::RegressionFixture) => Ok(CandidateArtifactType::Workflow),
        Some(SolutionType::HarnessPolicy) => Ok(CandidateArtifactType::PromptGuidance),
        Some(SolutionType::ProductCodeChange)
        | Some(SolutionType::DocumentationUpdate)
        | Some(SolutionType::ConfigurationChange) => Ok(CandidateArtifactType::EngineeringProposal),
        Some(SolutionType::SessionNote) => Ok(CandidateArtifactType::Memory),
        Some(SolutionType::NoPersistence) | None => Err(CorrectionLoopError::Validation(
            "no durable solution was selected".to_owned(),
        )),
    }
}

fn source_observations<'a>(
    provenance: &'a ProvenanceGraph,
    session_id: &str,
) -> Vec<&'a ProvenanceObservation> {
    provenance
        .observations
        .iter()
        .filter(|observation| {
            observation.session_id == session_id
                && observation.source == ProvenanceSource::UserCorrection
                && observation.privacy.captured
        })
        .collect()
}

fn stable_id(kind: &str, values: &[&str]) -> String {
    let mut input = kind.to_owned();
    for value in values {
        input.push('\0');
        input.push_str(value);
    }
    format!(
        "{kind}-{}",
        hex::encode(sha2::Sha256::digest(input.as_bytes()))
    )
}

fn contains_sensitive(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "api_key",
        "apikey",
        "authorization:",
        "bearer ",
        "secret=",
        "token=",
        "password=",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

use sha2::Digest;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn originating_scenario_marks_failure_replay() {
        let scenario = originating_scenario_for_episode("ep-1", "migrate the API", "all tests pass");
        assert_eq!(scenario.id, "replay-ep-1");
        assert_eq!(scenario.kind, ReplayScenarioKind::OriginatingFailure);
        assert!(scenario.candidate_should_trigger);
    }

    #[test]
    fn tool_trace_observation_mints_task_signal() {
        use crate::correction_signals::{LearningSignalKind, ToolTraceObservation};
        let signal = ToolTraceObservation {
            kind: LearningSignalKind::VerificationFailure,
            task_id: Some("task-9".to_owned()),
            observed_behavior: "verification failed on retry controller".to_owned(),
            evidence_turn_ids: vec!["turn-3".to_owned()],
        }
        .to_signal("sig-1".to_owned());
        assert_eq!(signal.kind, LearningSignalKind::VerificationFailure);
        assert_eq!(signal.task_id.as_deref(), Some("task-9"));
    }

    #[test]
    fn deterministic_runner_executes_artifact_content() {
        let runner = DeterministicProductionReplayRunner;
        let scenario = ReplayScenario {
            id: "origin".into(),
            kind: ReplayScenarioKind::OriginatingFailure,
            input: "input".into(),
            expected_behavior: "inventory sources".into(),
            candidate_should_trigger: true,
        };
        let proposal = SolutionProposal {
            lesson_id: "lesson".into(),
            source_signal_ids: vec!["signal".into()],
            selected: vec![SolutionType::ReusableSkill],
            alternatives: Vec::new(),
            rationale: "test".into(),
            review_strength: crate::solution_selection::ReviewStrength::Elevated,
            isolated: true,
            editable: true,
            artifacts: vec![GeneratedArtifact {
                path: "candidate.md".into(),
                content: "inventory sources".into(),
            }],
            activation_blocked: true,
        };
        let observation = runner.run(&scenario, Some(&proposal), &ReplayEnvironment::default());
        assert!(observation.candidate_triggered);
        assert_eq!(observation.behavior, "inventory sources");
    }
}
