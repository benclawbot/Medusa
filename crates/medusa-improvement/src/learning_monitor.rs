//! Causal-ish monitoring for activated learned behavior.
//!
//! The monitor is deliberately conservative: availability is not exposure, multiple simultaneous
//! exposures are confounded rather than credited, cohorts are never pooled across runtime or
//! repository revisions, and low-sample beliefs remain uncertain. Harmful active refinements are
//! suspended and rolled back through the canonical authority when an exact predecessor exists.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use medusa_context::refinement::{RefinementArtifactKind, RefinementLifecycle};
use medusa_core::learning_policy::LearningAdmissionPolicy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::refinement_authority::{RefinementAuthorityStore, SelectionContext, SelectionResult};

const SCHEMA_VERSION: u32 = 1;
const MONITOR_ROOT: &str = ".medusa/learning-monitor";
const MIN_SAMPLES: usize = 3;
const NEGATIVE_RATE_MILLI: u16 = 500;
const COOLDOWN_MS: i64 = 15 * 60 * 1_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorArtifactKind {
    Memory,
    Prompt,
    Tool,
    Workflow,
    Skill,
    ModelRouting,
    CodeHarness,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExposureState {
    Available,
    Considered,
    Selected,
    Injected,
    Applied,
    Ignored,
    Overridden,
    Conflicted,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    Positive,
    Negative,
    Inconclusive,
    Censored,
    Confounded,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorActionKind {
    Keep,
    CollectMoreEvidence,
    RequestReview,
    Suspend,
    Rollback,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionMethod {
    DirectSingleExposure,
    StratifiedCohort,
    Confounded,
    Unattributed,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CohortKey {
    pub model: String,
    pub provider: String,
    pub harness: String,
    pub prompt_fingerprint: String,
    pub repository_revision: String,
    pub tool_cohort: String,
    pub simultaneous_exposures: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExposureRecord {
    pub id: String,
    pub artifact_id: String,
    pub artifact_version: u64,
    pub artifact_kind: MonitorArtifactKind,
    pub projection_revision: u64,
    pub state: ExposureState,
    pub matching_reason: String,
    pub root_task_id: String,
    pub trajectory_id: String,
    pub session_id: String,
    pub repository_identity: Option<String>,
    pub repository_revision: String,
    pub task_features: BTreeSet<String>,
    pub cohort: CohortKey,
    pub baseline_assignment: Option<String>,
    pub budget_tokens: Option<u64>,
    pub budget_millis: Option<u64>,
    pub recorded_at_unix_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OutcomeRecord {
    pub id: String,
    pub root_task_id: String,
    pub trajectory_id: String,
    pub session_id: String,
    pub exposure_ids: Vec<String>,
    pub status: OutcomeStatus,
    pub authoritative_receipt_ids: Vec<String>,
    pub evidence_ids: Vec<String>,
    pub task_features: BTreeSet<String>,
    pub repository_revision: String,
    pub cohort: CohortKey,
    pub authoritative_correct: Option<bool>,
    pub verification_passed: Option<bool>,
    pub user_correction_count: u32,
    pub parent_review_revisions: u32,
    pub retries: u32,
    pub tool_failures: u32,
    pub latency_millis: u64,
    pub token_cost: u64,
    pub privacy_violation: bool,
    pub safety_violation: bool,
    pub recorded_at_unix_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BeliefState {
    pub positive_evidence: u64,
    pub negative_evidence: u64,
    pub inconclusive_evidence: u64,
    pub estimate_milli: u16,
    pub uncertainty_milli: u16,
    pub last_updated_unix_ms: i64,
    pub last_exposure_unix_ms: i64,
    pub last_repository_revision: String,
}

impl Default for BeliefState {
    fn default() -> Self {
        Self {
            positive_evidence: 0,
            negative_evidence: 0,
            inconclusive_evidence: 0,
            estimate_milli: 500,
            uncertainty_milli: 1_000,
            last_updated_unix_ms: 0,
            last_exposure_unix_ms: 0,
            last_repository_revision: String::new(),
        }
    }
}

impl BeliefState {
    fn update(&mut self, status: OutcomeStatus, now: i64, revision: &str) {
        match status {
            OutcomeStatus::Positive => {
                self.positive_evidence = self.positive_evidence.saturating_add(1)
            }
            OutcomeStatus::Negative => {
                self.negative_evidence = self.negative_evidence.saturating_add(1)
            }
            OutcomeStatus::Inconclusive | OutcomeStatus::Censored | OutcomeStatus::Confounded => {
                self.inconclusive_evidence = self.inconclusive_evidence.saturating_add(1)
            }
        }
        let effective = self
            .positive_evidence
            .saturating_add(self.negative_evidence);
        self.estimate_milli = if effective == 0 {
            500
        } else {
            ((self.positive_evidence.saturating_mul(1_000)) / effective) as u16
        };
        self.uncertainty_milli = uncertainty_milli(effective);
        self.last_updated_unix_ms = now;
        self.last_exposure_unix_ms = now;
        self.last_repository_revision = revision.to_owned();
    }

    pub fn decay_for_drift(&mut self, now: i64, revision: &str) {
        if !self.last_repository_revision.is_empty() && self.last_repository_revision != revision {
            self.estimate_milli = 500 + ((i32::from(self.estimate_milli) - 500) / 2) as u16;
            self.uncertainty_milli = self.uncertainty_milli.saturating_add(250).min(1_000);
            self.last_updated_unix_ms = now;
            self.last_repository_revision = revision.to_owned();
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CohortReport {
    pub cohort: CohortKey,
    pub eligible_samples: usize,
    pub positive_samples: usize,
    pub negative_samples: usize,
    pub inconclusive_samples: usize,
    pub effect_estimate_milli: i16,
    pub uncertainty_milli: u16,
    pub coverage_milli: u16,
    pub confounders: Vec<String>,
    pub method: AttributionMethod,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AttributionReport {
    pub artifact_id: String,
    pub artifact_version: u64,
    pub method: AttributionMethod,
    pub eligible_samples: usize,
    pub effect_estimate_milli: i16,
    pub uncertainty_milli: u16,
    pub cohort_coverage_milli: u16,
    pub confounders: Vec<String>,
    pub cohorts: Vec<CohortReport>,
    pub evidence_ids: Vec<String>,
    pub generated_at_unix_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MonitorAction {
    pub artifact_id: String,
    pub artifact_version: u64,
    pub kind: MonitorActionKind,
    pub reason: String,
    pub exact_predecessor_id: Option<String>,
    pub exact_predecessor_version: Option<u64>,
    pub invalidate_prompt_context: bool,
    pub cooldown_until_unix_ms: Option<i64>,
    pub recorded_at_unix_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactMonitorState {
    pub artifact_id: String,
    pub artifact_version: u64,
    pub kind: MonitorArtifactKind,
    pub active: bool,
    pub predecessor_id: Option<String>,
    pub predecessor_version: Option<u64>,
    pub belief: BeliefState,
    pub exposures: Vec<ExposureRecord>,
    pub outcomes: Vec<OutcomeRecord>,
    pub reports: Vec<AttributionReport>,
    pub actions: Vec<MonitorAction>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct LearningMonitorSnapshot {
    pub schema_version: u32,
    pub revision: u64,
    pub artifacts: Vec<ArtifactMonitorState>,
    pub unattributed_outcomes: Vec<OutcomeRecord>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct MonitorResult {
    pub snapshot: LearningMonitorSnapshot,
    pub reports: Vec<AttributionReport>,
    pub actions: Vec<MonitorAction>,
}

#[derive(Debug, thiserror::Error)]
pub enum LearningMonitorError {
    #[error("learning monitor I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("learning monitor serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("learning monitor validation failed: {0}")]
    Validation(String),
    #[error("learning monitor authority operation failed: {0}")]
    Authority(String),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MonitorDocument {
    schema_version: u32,
    revision: u64,
    artifacts: BTreeMap<String, ArtifactMonitorState>,
    unattributed_outcomes: Vec<OutcomeRecord>,
}

impl Default for MonitorDocument {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 0,
            artifacts: BTreeMap::new(),
            unattributed_outcomes: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LearningMonitorStore {
    root: PathBuf,
    document: MonitorDocument,
}

impl LearningMonitorStore {
    pub fn open(repo: &Path) -> Result<Self, LearningMonitorError> {
        let root = repo.join(MONITOR_ROOT);
        let path = root.join("state.json");
        let document = if path.is_file() {
            let document: MonitorDocument = serde_json::from_slice(&fs::read(&path)?)?;
            if document.schema_version != SCHEMA_VERSION {
                return Err(LearningMonitorError::Validation(format!(
                    "unsupported monitor schema {}",
                    document.schema_version
                )));
            }
            document
        } else {
            MonitorDocument::default()
        };
        Ok(Self { root, document })
    }

    #[must_use]
    pub fn snapshot(&self) -> LearningMonitorSnapshot {
        LearningMonitorSnapshot {
            schema_version: self.document.schema_version,
            revision: self.document.revision,
            artifacts: self.document.artifacts.values().cloned().collect(),
            unattributed_outcomes: self.document.unattributed_outcomes.clone(),
        }
    }

    /// Records a terminal outcome, partitioning exposure ids by their exact cohort before
    /// updating beliefs. A single outcome may therefore produce separate stratified reports
    /// rather than pooling different model, provider, harness, or repository populations.
    pub fn record_outcome(
        &mut self,
        repo: &Path,
        outcome: OutcomeRecord,
    ) -> Result<MonitorResult, LearningMonitorError> {
        validate_outcome(&outcome)?;
        let groups = self.exposure_groups(&outcome);
        if groups.len() <= 1 {
            return self.record_outcome_single(repo, outcome);
        }

        let mut reports = Vec::new();
        let mut actions = Vec::new();
        for (cohort, exposure_ids) in groups {
            let mut grouped = outcome.clone();
            grouped.cohort = cohort.clone();
            grouped.exposure_ids = exposure_ids;
            grouped.id = format!(
                "{}:{}",
                outcome.id,
                digest(&serde_json::to_string(&cohort)?)
            );
            let result = self.record_outcome_single(repo, grouped)?;
            reports.extend(result.reports);
            actions.extend(result.actions);
        }
        Ok(MonitorResult {
            snapshot: self.snapshot(),
            reports,
            actions,
        })
    }

    /// Records an outcome using every applied exposure in the same session when the caller does
    /// not yet know the exposure ids. This is the production-path bridge from terminal session
    /// receipts to the monitor and still partitions exposures by cohort.
    pub fn record_session_outcome(
        &mut self,
        repo: &Path,
        mut outcome: OutcomeRecord,
    ) -> Result<MonitorResult, LearningMonitorError> {
        if outcome.exposure_ids.is_empty() {
            outcome.exposure_ids = self
                .document
                .artifacts
                .values()
                .flat_map(|state| state.exposures.iter())
                .filter(|exposure| {
                    exposure.state == ExposureState::Applied
                        && exposure.session_id == outcome.session_id
                })
                .map(|exposure| exposure.id.clone())
                .collect();
        }
        self.record_outcome(repo, outcome)
    }

    fn exposure_groups(&self, outcome: &OutcomeRecord) -> BTreeMap<CohortKey, Vec<String>> {
        let requested = outcome.exposure_ids.iter().collect::<BTreeSet<_>>();
        let mut groups = BTreeMap::<CohortKey, Vec<String>>::new();
        for state in self.document.artifacts.values() {
            for exposure in &state.exposures {
                let requested_match = !requested.is_empty() && requested.contains(&exposure.id);
                let session_match =
                    requested.is_empty() && exposure.session_id == outcome.session_id;
                if exposure.state == ExposureState::Applied && (requested_match || session_match) {
                    groups
                        .entry(exposure.cohort.clone())
                        .or_default()
                        .push(exposure.id.clone());
                }
            }
        }
        groups
    }

    pub fn record_selection(
        repo: &Path,
        context: &SelectionContext,
        result: &SelectionResult,
        projection_revision: u64,
        now_unix_ms: i64,
    ) -> Result<usize, LearningMonitorError> {
        let policy = LearningAdmissionPolicy::for_repository(repo).map_err(|error| {
            LearningMonitorError::Validation(format!("learning policy unavailable: {error}"))
        })?;
        if !policy.telemetry_enabled() || result.selected.is_empty() {
            return Ok(0);
        }
        let mut store = Self::open(repo)?;
        let mut count = 0;
        let simultaneous = result
            .selected
            .iter()
            .map(|selected| format!("{}:{}", selected.proposal.id, selected.proposal.version))
            .collect::<Vec<_>>();
        for selected in &result.selected {
            let id = stable_id(
                "exposure",
                &[
                    context.session_id.as_deref().unwrap_or("sessionless"),
                    &selected.proposal.id,
                    &selected.proposal.version.to_string(),
                    &projection_revision.to_string(),
                ],
            );
            let cohort = CohortKey {
                model: env_or("MEDUSA_MODEL", "unknown-model"),
                provider: env_or("MEDUSA_PROVIDER", "unknown-provider"),
                harness: format!("medusa-runtime/{}", env!("CARGO_PKG_VERSION")),
                prompt_fingerprint: digest(&context.objective),
                repository_revision: repository_revision(repo).unwrap_or_else(|| "unknown".into()),
                tool_cohort: context
                    .artifact_kind
                    .clone()
                    .unwrap_or_else(|| "unknown".into()),
                simultaneous_exposures: simultaneous.clone(),
            };
            let exposure = ExposureRecord {
                id: id.clone(),
                artifact_id: selected.proposal.id.clone(),
                artifact_version: selected.proposal.version,
                artifact_kind: monitor_kind(selected.proposal.artifact_kind),
                projection_revision,
                state: ExposureState::Applied,
                matching_reason: selected.selection_rationale.clone(),
                root_task_id: context.session_id.clone().unwrap_or_else(|| id.clone()),
                trajectory_id: context.session_id.clone().unwrap_or_else(|| id.clone()),
                session_id: context.session_id.clone().unwrap_or_else(|| id.clone()),
                repository_identity: context
                    .repository
                    .as_ref()
                    .map(|value| format!("{value:?}")),
                repository_revision: cohort.repository_revision.clone(),
                task_features: context.context_tags.clone(),
                cohort: cohort.clone(),
                baseline_assignment: None,
                budget_tokens: None,
                budget_millis: None,
                recorded_at_unix_ms: now_unix_ms,
            };
            let key = artifact_key(&exposure.artifact_id, exposure.artifact_version);
            let state =
                store
                    .document
                    .artifacts
                    .entry(key)
                    .or_insert_with(|| ArtifactMonitorState {
                        artifact_id: exposure.artifact_id.clone(),
                        artifact_version: exposure.artifact_version,
                        kind: exposure.artifact_kind,
                        active: true,
                        predecessor_id: None,
                        predecessor_version: None,
                        belief: BeliefState::default(),
                        exposures: Vec::new(),
                        outcomes: Vec::new(),
                        reports: Vec::new(),
                        actions: Vec::new(),
                    });
            if state.predecessor_id.is_none()
                && let Ok(authority) = RefinementAuthorityStore::open(repo)
                && let Ok(snapshot) = authority.snapshot()
                && let Some(record) = snapshot.records.iter().find(|record| {
                    record.proposal_id == state.artifact_id
                        && record.version == state.artifact_version
                })
            {
                state.predecessor_id = record.predecessor_proposal_id.clone();
                state.predecessor_version = record.predecessor_version;
            }
            if !state
                .exposures
                .iter()
                .any(|candidate| candidate.id == exposure.id)
            {
                state.exposures.push(exposure);
                state
                    .belief
                    .decay_for_drift(now_unix_ms, &cohort.repository_revision);
                count += 1;
            }
        }
        if count > 0 {
            store.commit_event("selection", now_unix_ms)?;
        }
        Ok(count)
    }

    fn record_outcome_single(
        &mut self,
        repo: &Path,
        outcome: OutcomeRecord,
    ) -> Result<MonitorResult, LearningMonitorError> {
        validate_outcome(&outcome)?;
        let recorded_at_unix_ms = now_unix_ms(&outcome);
        if self
            .document
            .unattributed_outcomes
            .iter()
            .any(|item| item.id == outcome.id)
            || self
                .document
                .artifacts
                .values()
                .any(|artifact| artifact.outcomes.iter().any(|item| item.id == outcome.id))
        {
            return Ok(MonitorResult {
                snapshot: self.snapshot(),
                ..MonitorResult::default()
            });
        }
        let mut reports = Vec::new();
        let mut actions = Vec::new();
        let mut eligible = Vec::new();
        for state in self.document.artifacts.values_mut() {
            let matched = state
                .exposures
                .iter()
                .filter(|exposure| {
                    outcome.exposure_ids.contains(&exposure.id)
                        && exposure.state == ExposureState::Applied
                        && exposure.cohort == outcome.cohort
                })
                .cloned()
                .collect::<Vec<_>>();
            if matched.is_empty() {
                continue;
            }
            eligible.extend(matched.iter().map(|exposure| exposure.id.clone()));
            let confounded = matched.len() > 1
                || matched
                    .iter()
                    .any(|exposure| exposure.cohort.simultaneous_exposures.len() > 1);
            let effective_status = if confounded {
                OutcomeStatus::Confounded
            } else {
                outcome.status
            };
            state.outcomes.push(outcome.clone());
            if effective_status != OutcomeStatus::Confounded {
                state.belief.update(
                    effective_status,
                    outcome.recorded_at_unix_ms,
                    &outcome.repository_revision,
                );
            } else {
                state.belief.inconclusive_evidence =
                    state.belief.inconclusive_evidence.saturating_add(1);
                state.belief.uncertainty_milli = 1_000;
            }
            let report = build_report(state, &outcome, effective_status);
            state.reports.push(report.clone());
            let action = decide_action(state, &outcome, effective_status);
            if action.kind != MonitorActionKind::Keep
                && action.kind != MonitorActionKind::CollectMoreEvidence
            {
                let action = action.clone();
                apply_authority_action(repo, state, &action)?;
                state.actions.push(action.clone());
                actions.push(action);
            } else {
                state.actions.push(action.clone());
                actions.push(action);
            }
            reports.push(report);
        }
        if eligible.is_empty() {
            self.document.unattributed_outcomes.push(outcome);
        }
        self.commit_event("outcome", recorded_at_unix_ms)?;
        Ok(MonitorResult {
            snapshot: self.snapshot(),
            reports,
            actions,
        })
    }

    fn commit_event(&mut self, kind: &str, now: i64) -> Result<(), LearningMonitorError> {
        self.document.revision = self.document.revision.saturating_add(1);
        fs::create_dir_all(&self.root)?;
        let state_path = self.root.join("state.json");
        let temporary = self.root.join(format!("state.tmp-{}", std::process::id()));
        fs::write(&temporary, serde_json::to_vec_pretty(&self.document)?)?;
        if state_path.exists() {
            fs::remove_file(&state_path)?;
        }
        fs::rename(temporary, state_path)?;
        let event = serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "revision": self.document.revision,
            "kind": kind,
            "recorded_at_unix_ms": now,
        });
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("events.jsonl"))?;
        serde_json::to_writer(&mut file, &event)?;
        use std::io::Write;
        file.write_all(b"\n")?;
        file.sync_data()?;
        Ok(())
    }
}

fn build_report(
    state: &ArtifactMonitorState,
    outcome: &OutcomeRecord,
    status: OutcomeStatus,
) -> AttributionReport {
    let eligible = usize::from(status != OutcomeStatus::Confounded);
    let positive = usize::from(status == OutcomeStatus::Positive);
    let negative = usize::from(status == OutcomeStatus::Negative);
    let estimate = if eligible == 0 {
        0
    } else {
        ((positive as i16 - negative as i16) * 1_000) / eligible as i16
    };
    let method = if status == OutcomeStatus::Confounded {
        AttributionMethod::Confounded
    } else {
        AttributionMethod::DirectSingleExposure
    };
    AttributionReport {
        artifact_id: state.artifact_id.clone(),
        artifact_version: state.artifact_version,
        method,
        eligible_samples: eligible,
        effect_estimate_milli: estimate,
        uncertainty_milli: uncertainty_milli(eligible as u64),
        cohort_coverage_milli: if outcome.task_features.is_empty() {
            500
        } else {
            1_000
        },
        confounders: if status == OutcomeStatus::Confounded {
            vec!["simultaneous applied exposures prevent individual attribution".into()]
        } else {
            Vec::new()
        },
        cohorts: vec![CohortReport {
            cohort: outcome.cohort.clone(),
            eligible_samples: eligible,
            positive_samples: positive,
            negative_samples: negative,
            inconclusive_samples: usize::from(
                status != OutcomeStatus::Positive && status != OutcomeStatus::Negative,
            ),
            effect_estimate_milli: estimate,
            uncertainty_milli: uncertainty_milli(eligible as u64),
            coverage_milli: if outcome.task_features.is_empty() {
                500
            } else {
                1_000
            },
            confounders: Vec::new(),
            method,
        }],
        evidence_ids: outcome.evidence_ids.clone(),
        generated_at_unix_ms: outcome.recorded_at_unix_ms,
    }
}

fn decide_action(
    state: &ArtifactMonitorState,
    outcome: &OutcomeRecord,
    status: OutcomeStatus,
) -> MonitorAction {
    let effective = state.belief.positive_evidence + state.belief.negative_evidence;
    let negative_rate = if effective == 0 {
        0
    } else {
        ((state.belief.negative_evidence * 1_000) / effective) as u16
    };
    let critical = outcome.privacy_violation || outcome.safety_violation;
    let kind = if critical && state.active {
        if state.predecessor_id.is_some() {
            MonitorActionKind::Rollback
        } else {
            MonitorActionKind::Suspend
        }
    } else if status == OutcomeStatus::Confounded || effective < MIN_SAMPLES as u64 {
        MonitorActionKind::CollectMoreEvidence
    } else if state.active && negative_rate >= NEGATIVE_RATE_MILLI {
        if state.predecessor_id.is_some() {
            MonitorActionKind::Rollback
        } else {
            MonitorActionKind::Suspend
        }
    } else if state.belief.uncertainty_milli > 700 {
        MonitorActionKind::RequestReview
    } else {
        MonitorActionKind::Keep
    };
    let reason = if critical {
        "critical privacy or safety evidence bypasses cooldown".to_owned()
    } else if status == OutcomeStatus::Confounded {
        "simultaneous exposures are retained as confounded and receive no individual credit"
            .to_owned()
    } else if negative_rate >= NEGATIVE_RATE_MILLI {
        format!("negative eligible outcome rate {negative_rate} exceeds rollback threshold")
    } else if effective < MIN_SAMPLES as u64 {
        "sample count remains below the minimum evidence threshold".to_owned()
    } else {
        "continue monitoring eligible scope-matched exposures".to_owned()
    };
    MonitorAction {
        artifact_id: state.artifact_id.clone(),
        artifact_version: state.artifact_version,
        kind,
        reason,
        exact_predecessor_id: state.predecessor_id.clone(),
        exact_predecessor_version: state.predecessor_version,
        invalidate_prompt_context: matches!(
            kind,
            MonitorActionKind::Suspend | MonitorActionKind::Rollback
        ),
        cooldown_until_unix_ms: (kind == MonitorActionKind::RequestReview)
            .then_some(outcome.recorded_at_unix_ms.saturating_add(COOLDOWN_MS)),
        recorded_at_unix_ms: outcome.recorded_at_unix_ms,
    }
}

fn apply_authority_action(
    repo: &Path,
    state: &mut ArtifactMonitorState,
    action: &MonitorAction,
) -> Result<(), LearningMonitorError> {
    if !matches!(
        action.kind,
        MonitorActionKind::Suspend | MonitorActionKind::Rollback
    ) {
        return Ok(());
    }
    let mut authority = RefinementAuthorityStore::open(repo)
        .map_err(|error| LearningMonitorError::Authority(error.to_string()))?;
    let snapshot = authority
        .snapshot()
        .map_err(|error| LearningMonitorError::Authority(error.to_string()))?;
    let Some(record) = snapshot.records.iter().find(|record| {
        record.proposal_id == state.artifact_id
            && record.version == state.artifact_version
            && record.lifecycle == RefinementLifecycle::Active
    }) else {
        return Err(LearningMonitorError::Authority(format!(
            "active canonical refinement {}:{} was not found",
            state.artifact_id, state.artifact_version
        )));
    };
    state.predecessor_id = record.predecessor_proposal_id.clone();
    state.predecessor_version = record.predecessor_version;
    if action.kind == MonitorActionKind::Rollback {
        let Some(predecessor_id) = state.predecessor_id.as_deref() else {
            return Err(LearningMonitorError::Authority(
                "harmful refinement has no exact predecessor; review is required".into(),
            ));
        };
        authority
            .rollback(
                &state.artifact_id,
                state.artifact_version,
                Some(predecessor_id),
                state.predecessor_version,
                "monitor rollback to exact direct predecessor",
                snapshot.revision,
            )
            .map_err(|error| LearningMonitorError::Authority(error.to_string()))?;
    } else {
        authority
            .suspend(
                &state.artifact_id,
                state.artifact_version,
                &action.reason,
                snapshot.revision,
            )
            .map_err(|error| LearningMonitorError::Authority(error.to_string()))?;
    }
    state.active = false;
    Ok(())
}

fn validate_outcome(outcome: &OutcomeRecord) -> Result<(), LearningMonitorError> {
    if outcome.id.trim().is_empty()
        || outcome.session_id.trim().is_empty()
        || outcome.root_task_id.trim().is_empty()
        || outcome.repository_revision.trim().is_empty()
        || outcome.authoritative_receipt_ids.is_empty() && outcome.status == OutcomeStatus::Positive
    {
        return Err(LearningMonitorError::Validation(
            "outcome lacks identity, repository revision, or authoritative receipt".into(),
        ));
    }
    Ok(())
}

fn monitor_kind(kind: RefinementArtifactKind) -> MonitorArtifactKind {
    match kind {
        RefinementArtifactKind::Memory | RefinementArtifactKind::RepositoryConvention => {
            MonitorArtifactKind::Memory
        }
        RefinementArtifactKind::WorkflowMetadata => MonitorArtifactKind::Workflow,
        RefinementArtifactKind::TeamRoleMetadata => MonitorArtifactKind::Tool,
        RefinementArtifactKind::PromptGuidance => MonitorArtifactKind::Prompt,
    }
}

fn artifact_key(id: &str, version: u64) -> String {
    format!("{id}:{version}")
}

fn uncertainty_milli(samples: u64) -> u16 {
    if samples == 0 {
        1_000
    } else {
        (1_000.0 / (samples as f64).sqrt()).round().min(1_000.0) as u16
    }
}

fn now_unix_ms(outcome: &OutcomeRecord) -> i64 {
    outcome.recorded_at_unix_ms
}

fn repository_revision(repo: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_owned())
}

fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn stable_id(kind: &str, values: &[&str]) -> String {
    let mut bytes = kind.as_bytes().to_vec();
    for value in values {
        bytes.push(0);
        bytes.extend_from_slice(value.as_bytes());
    }
    format!("{kind}-{}", digest(&String::from_utf8_lossy(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refinement_authority::{
        ApprovalActorClass, RefinementAuthorityStore, SelectionContext,
    };
    use medusa_context::refinement::{
        EvaluationResult, EvidenceKind, EvidenceRef, ProposerMetadata, RefinementContent,
        RefinementProposal, RefinementRisk,
    };
    use medusa_core::learning_policy::LearningPrivacyPolicy;
    use tempfile::tempdir;

    fn cohort() -> CohortKey {
        CohortKey {
            model: "m".into(),
            provider: "p".into(),
            harness: "h".into(),
            prompt_fingerprint: "prompt".into(),
            repository_revision: "rev-1".into(),
            tool_cohort: "tools".into(),
            simultaneous_exposures: vec!["refinement-1:1".into()],
        }
    }

    fn outcome(id: &str, exposure: &str, status: OutcomeStatus) -> OutcomeRecord {
        OutcomeRecord {
            id: id.into(),
            root_task_id: "root".into(),
            trajectory_id: "trajectory".into(),
            session_id: "session".into(),
            exposure_ids: vec![exposure.into()],
            status,
            authoritative_receipt_ids: vec!["receipt".into()],
            evidence_ids: vec!["evidence".into()],
            task_features: BTreeSet::from(["coding".into()]),
            repository_revision: "rev-1".into(),
            cohort: cohort(),
            authoritative_correct: Some(status == OutcomeStatus::Positive),
            verification_passed: Some(status == OutcomeStatus::Positive),
            user_correction_count: 0,
            parent_review_revisions: 0,
            retries: 0,
            tool_failures: 0,
            latency_millis: 10,
            token_cost: 20,
            privacy_violation: false,
            safety_violation: false,
            recorded_at_unix_ms: 1,
        }
    }

    fn exposure(id: &str, artifact: &str, simultaneous: Vec<String>) -> ExposureRecord {
        let mut cohort = cohort();
        cohort.simultaneous_exposures = simultaneous;
        ExposureRecord {
            id: id.into(),
            artifact_id: artifact.into(),
            artifact_version: 1,
            artifact_kind: MonitorArtifactKind::Prompt,
            projection_revision: 1,
            state: ExposureState::Applied,
            matching_reason: "objective matched".into(),
            root_task_id: "root".into(),
            trajectory_id: "trajectory".into(),
            session_id: "session".into(),
            repository_identity: Some("repo".into()),
            repository_revision: "rev-1".into(),
            task_features: BTreeSet::from(["coding".into()]),
            cohort,
            baseline_assignment: None,
            budget_tokens: None,
            budget_millis: None,
            recorded_at_unix_ms: 1,
        }
    }

    fn authority_proposal(id: &str, version: u64, value: &str) -> RefinementProposal {
        RefinementProposal {
            id: id.into(),
            version,
            artifact_kind: RefinementArtifactKind::RepositoryConvention,
            scope: medusa_context::refinement::RefinementScope::Repository,
            evidence: vec![EvidenceRef {
                id: format!("evidence-{id}"),
                kind: EvidenceKind::UserCorrection,
                trajectory_id: "trajectory".into(),
                start_sequence: 1,
                end_sequence: 1,
            }],
            before: None,
            after: RefinementContent::RepositoryConvention {
                key: "workflow".into(),
                value: value.into(),
            },
            rationale: "verified correction".into(),
            expected_outcome: "matching work improves".into(),
            proposer: ProposerMetadata {
                model: "test".into(),
                route: "test".into(),
                version: "1".into(),
            },
            risk: RefinementRisk::Low,
        }
    }

    #[test]
    fn only_applied_single_exposure_receives_eligible_evidence() {
        let repo = tempdir().expect("repo");
        let mut store = LearningMonitorStore::open(repo.path()).expect("store");
        let state = ArtifactMonitorState {
            artifact_id: "refinement-1".into(),
            artifact_version: 1,
            kind: MonitorArtifactKind::Prompt,
            active: true,
            predecessor_id: None,
            predecessor_version: None,
            belief: BeliefState::default(),
            exposures: vec![exposure(
                "applied",
                "refinement-1",
                vec!["refinement-1:1".into()],
            )],
            outcomes: Vec::new(),
            reports: Vec::new(),
            actions: Vec::new(),
        };
        store
            .document
            .artifacts
            .insert("refinement-1:1".into(), state);
        let result = store
            .record_outcome(
                repo.path(),
                outcome("outcome", "applied", OutcomeStatus::Positive),
            )
            .expect("outcome");
        assert_eq!(result.reports[0].eligible_samples, 1);
        assert_eq!(result.snapshot.unattributed_outcomes.len(), 0);
    }

    #[test]
    fn simultaneous_exposures_are_confounded_and_do_not_change_confidence() {
        let repo = tempdir().expect("repo");
        let mut store = LearningMonitorStore::open(repo.path()).expect("store");
        let mut left = exposure("left", "left", vec!["left:1".into(), "right:1".into()]);
        let right = exposure("right", "right", vec!["left:1".into(), "right:1".into()]);
        left.cohort = right.cohort.clone();
        store.document.artifacts.insert(
            "left:1".into(),
            ArtifactMonitorState {
                artifact_id: "left".into(),
                artifact_version: 1,
                kind: MonitorArtifactKind::Prompt,
                active: true,
                predecessor_id: None,
                predecessor_version: None,
                belief: BeliefState::default(),
                exposures: vec![left],
                outcomes: Vec::new(),
                reports: Vec::new(),
                actions: Vec::new(),
            },
        );
        store.document.artifacts.insert(
            "right:1".into(),
            ArtifactMonitorState {
                artifact_id: "right".into(),
                artifact_version: 1,
                kind: MonitorArtifactKind::Prompt,
                active: true,
                predecessor_id: None,
                predecessor_version: None,
                belief: BeliefState::default(),
                exposures: vec![right],
                outcomes: Vec::new(),
                reports: Vec::new(),
                actions: Vec::new(),
            },
        );
        let result = store
            .record_outcome(
                repo.path(),
                outcome("outcome", "left", OutcomeStatus::Positive),
            )
            .expect("outcome");
        assert!(
            result
                .reports
                .iter()
                .all(|report| report.method == AttributionMethod::Confounded)
        );
        assert!(
            result
                .snapshot
                .artifacts
                .iter()
                .all(|state| state.belief.positive_evidence == 0)
        );
    }

    #[test]
    fn different_runtime_cohorts_are_reported_separately() {
        let repo = tempdir().expect("repo");
        let mut store = LearningMonitorStore::open(repo.path()).expect("store");
        let first = exposure("first", "refinement-1", vec!["refinement-1:1".into()]);
        let mut second = exposure("second", "refinement-1", vec!["refinement-1:1".into()]);
        second.cohort.model = "different-model".into();
        store.document.artifacts.insert(
            "refinement-1:1".into(),
            ArtifactMonitorState {
                artifact_id: "refinement-1".into(),
                artifact_version: 1,
                kind: MonitorArtifactKind::Prompt,
                active: true,
                predecessor_id: None,
                predecessor_version: None,
                belief: BeliefState::default(),
                exposures: vec![first, second],
                outcomes: Vec::new(),
                reports: Vec::new(),
                actions: Vec::new(),
            },
        );
        let mut terminal = outcome("cohort-outcome", "first", OutcomeStatus::Positive);
        terminal.exposure_ids = vec!["first".into(), "second".into()];
        let result = store
            .record_outcome(repo.path(), terminal)
            .expect("outcome");
        assert_eq!(result.reports.len(), 2);
        assert_eq!(
            result
                .reports
                .iter()
                .map(|report| report.cohorts[0].cohort.model.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["m", "different-model"])
        );
    }

    #[test]
    fn repository_drift_halves_estimate_and_increases_uncertainty() {
        let mut belief = BeliefState::default();
        belief.update(OutcomeStatus::Positive, 1, "rev-1");
        belief.update(OutcomeStatus::Negative, 1, "rev-1");
        let before = belief.uncertainty_milli;
        belief.decay_for_drift(2, "rev-2");
        assert!(belief.estimate_milli < 1_000);
        assert!(belief.uncertainty_milli > before);
        assert_eq!(belief.last_repository_revision, "rev-2");
    }

    #[test]
    fn repeated_negative_evidence_rolls_back_the_exact_predecessor() {
        let repo = tempdir().expect("repo");
        let mut authority = RefinementAuthorityStore::open(repo.path()).expect("authority");
        authority
            .initialize_privacy(LearningPrivacyPolicy {
                telemetry_enabled: true,
                ..LearningPrivacyPolicy::private_by_default()
            })
            .expect("privacy");
        let mut snapshot = authority
            .propose(authority_proposal("previous", 1, "safe"), 0)
            .expect("previous proposal");
        snapshot = authority
            .validate("previous", 1, snapshot.revision)
            .expect("previous validation");
        snapshot = authority
            .record_evaluation(
                "previous",
                1,
                EvaluationResult {
                    evaluator: "test".into(),
                    validation_passed: true,
                    regression_passed: true,
                    effectiveness_passed: true,
                    notes: "passed".into(),
                },
                snapshot.revision,
            )
            .expect("previous evaluation");
        snapshot = authority
            .approve(
                "previous",
                1,
                ApprovalActorClass::User,
                "previous-approval",
                1,
                snapshot.revision,
            )
            .expect("previous approval");
        snapshot = authority
            .activate("previous", 1, snapshot.revision)
            .expect("previous activation");
        snapshot = authority
            .propose(
                authority_proposal("harmful", 1, "harmful"),
                snapshot.revision,
            )
            .expect("harmful proposal");
        snapshot = authority
            .validate("harmful", 1, snapshot.revision)
            .expect("harmful validation");
        snapshot = authority
            .record_evaluation(
                "harmful",
                1,
                EvaluationResult {
                    evaluator: "test".into(),
                    validation_passed: true,
                    regression_passed: true,
                    effectiveness_passed: true,
                    notes: "passed".into(),
                },
                snapshot.revision,
            )
            .expect("harmful evaluation");
        snapshot = authority
            .approve(
                "harmful",
                1,
                ApprovalActorClass::User,
                "harmful-approval",
                1,
                snapshot.revision,
            )
            .expect("harmful approval");
        snapshot = authority
            .supersede("previous", 1, "harmful", 1, snapshot.revision)
            .expect("supersede");
        authority
            .activate("harmful", 1, snapshot.revision)
            .expect("harmful activation");

        let mut store = LearningMonitorStore::open(repo.path()).expect("monitor");
        let exposures = (1..=3)
            .map(|index| {
                exposure(
                    &format!("applied-{index}"),
                    "harmful",
                    vec!["refinement-1:1".into()],
                )
            })
            .collect();
        store.document.artifacts.insert(
            "harmful:1".into(),
            ArtifactMonitorState {
                artifact_id: "harmful".into(),
                artifact_version: 1,
                kind: MonitorArtifactKind::Prompt,
                active: true,
                predecessor_id: Some("previous".into()),
                predecessor_version: Some(1),
                belief: BeliefState::default(),
                exposures,
                outcomes: Vec::new(),
                reports: Vec::new(),
                actions: Vec::new(),
            },
        );
        for index in 1..=3 {
            store
                .record_outcome(
                    repo.path(),
                    outcome(
                        &format!("negative-{index}"),
                        &format!("applied-{index}"),
                        OutcomeStatus::Negative,
                    ),
                )
                .expect("negative outcome");
        }
        let restored = RefinementAuthorityStore::open(repo.path())
            .expect("reopen authority")
            .snapshot()
            .expect("authority snapshot");
        assert!(
            restored
                .active
                .iter()
                .any(|proposal| proposal.id == "previous"),
            "restored active: {:?}; records: {:?}",
            restored.active,
            restored.records
        );
        assert!(
            !restored
                .active
                .iter()
                .any(|proposal| proposal.id == "harmful")
        );
        let harmful = restored
            .records
            .iter()
            .find(|record| record.proposal_id == "harmful")
            .expect("harmful record");
        assert_eq!(harmful.lifecycle, RefinementLifecycle::RolledBack);
    }

    #[test]
    fn sparse_and_negative_evidence_remain_uncertain_and_request_rollback_only_after_threshold() {
        let repo = tempdir().expect("repo");
        let mut store = LearningMonitorStore::open(repo.path()).expect("store");
        store.document.artifacts.insert(
            "refinement-1:1".into(),
            ArtifactMonitorState {
                artifact_id: "refinement-1".into(),
                artifact_version: 1,
                kind: MonitorArtifactKind::Prompt,
                active: true,
                predecessor_id: None,
                predecessor_version: None,
                belief: BeliefState::default(),
                exposures: vec![exposure(
                    "applied",
                    "refinement-1",
                    vec!["refinement-1:1".into()],
                )],
                outcomes: Vec::new(),
                reports: Vec::new(),
                actions: Vec::new(),
            },
        );
        let first = store
            .record_outcome(
                repo.path(),
                outcome("one", "applied", OutcomeStatus::Negative),
            )
            .expect("first");
        assert_eq!(
            first.actions[0].kind,
            MonitorActionKind::CollectMoreEvidence
        );
        assert_eq!(first.snapshot.artifacts[0].belief.uncertainty_milli, 1_000);
    }

    #[test]
    fn selection_audit_records_only_policy_allowed_selected_refinements() {
        let repo = tempdir().expect("repo");
        let authority = RefinementAuthorityStore::open(repo.path()).expect("authority");
        authority
            .initialize_privacy(LearningPrivacyPolicy {
                telemetry_enabled: true,
                ..LearningPrivacyPolicy::private_by_default()
            })
            .expect("privacy");
        let context = SelectionContext {
            repository: None,
            user_id: "user".into(),
            session_id: Some("session".into()),
            task_kind: Some("coding".into()),
            artifact_kind: Some("repository_convention".into()),
            context_tags: BTreeSet::new(),
            explicit_exclusions: BTreeSet::new(),
            objective: "objective".into(),
            now_unix_ms: 1,
        };
        let result = SelectionResult::default();
        assert_eq!(
            LearningMonitorStore::record_selection(repo.path(), &context, &result, 0, 1)
                .expect("selection"),
            0
        );
    }
}
