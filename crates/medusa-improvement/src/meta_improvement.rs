//! Bounded meta-improvement routing for runtime refinements and engineering proposals.
//!
//! This module is intentionally a proposal boundary, not an autonomous code writer. It clusters
//! repeated typed friction, preserves every source signal, and routes declarative changes through
//! the refinement authority while routing executable or policy changes to ordinary engineering
//! review. No proposal can satisfy its own approval or independent-evaluation requirement.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use medusa_context::refinement::{
    EvaluationResult, EvidenceKind, EvidenceRef, ProposerMetadata, RefinementArtifactKind,
    RefinementContent, RefinementProposal, RefinementRisk, RefinementScope,
};
use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::provenance::{
    ProvenanceAuthority, ProvenanceGraph, ProvenanceObservation, ProvenanceOutcome,
    ProvenanceSource,
};
use crate::refinement_authority::{ApprovalActorClass, RefinementAuthorityStore};

const SCHEMA_VERSION: u32 = 1;
const META_ROOT: &str = ".medusa/improvements/meta-proposals";
const MIN_REPEATED_SIGNALS: usize = 2;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetaImprovementLane {
    RuntimeRefinement,
    EngineeringProposal,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetaImprovementTarget {
    PromptOverlay,
    ToolDescription,
    RetrievalPolicy,
    TestDiscovery,
    RecoveryPlaybook,
    OrchestrationPlaybook,
    ModelRouting,
    HarnessCode,
    CapabilityPolicy,
    EvaluationHarness,
}

impl MetaImprovementTarget {
    #[must_use]
    pub const fn runtime_safe(self) -> bool {
        matches!(
            self,
            Self::PromptOverlay
                | Self::RetrievalPolicy
                | Self::TestDiscovery
                | Self::RecoveryPlaybook
                | Self::OrchestrationPlaybook
                | Self::ModelRouting
        )
    }

    #[must_use]
    pub const fn capability_sensitive(self) -> bool {
        matches!(
            self,
            Self::ToolDescription
                | Self::HarnessCode
                | Self::CapabilityPolicy
                | Self::EvaluationHarness
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetaImprovementStatus {
    Proposed,
    AwaitingReview,
    Approved,
    Activated,
    RolledBack,
    Rejected,
    Escalated,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ImprovementCohort {
    pub model: String,
    pub provider: String,
    pub harness: String,
    pub repository_revision: String,
    pub task_class: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FrictionSignal {
    pub id: String,
    pub session_id: String,
    pub trajectory_id: String,
    pub repository_revision: String,
    pub kind: String,
    pub detail: String,
    pub evidence_id: String,
    pub success: bool,
    pub authoritative: bool,
    pub negative_example: bool,
    pub capability_impact: bool,
    pub task_class: String,
    pub cohort: ImprovementCohort,
    pub recorded_at_unix_ms: i64,
}

impl FrictionSignal {
    fn validate(&self) -> Result<(), MetaImprovementError> {
        if self.id.trim().is_empty()
            || self.session_id.trim().is_empty()
            || self.trajectory_id.trim().is_empty()
            || self.kind.trim().is_empty()
            || self.detail.trim().is_empty()
            || self.evidence_id.trim().is_empty()
            || self.task_class.trim().is_empty()
            || self.repository_revision.trim().is_empty()
        {
            return Err(MetaImprovementError::Validation(
                "friction signal is incomplete".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImprovementBoundary {
    pub component_owner: String,
    pub reviewer_class: String,
    pub protected_roots: BTreeSet<String>,
    pub permitted_scope: String,
    pub max_capability_impact: bool,
    pub release_path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetaImprovementProposal {
    pub id: String,
    pub lane: MetaImprovementLane,
    pub target: MetaImprovementTarget,
    pub component_owner: String,
    pub source_signal_ids: Vec<String>,
    pub authoritative_signal_ids: Vec<String>,
    pub source_trajectory_ids: Vec<String>,
    pub friction_cluster: String,
    pub frequency: usize,
    pub impact_milli: u16,
    pub current_behavior: String,
    pub hypothesized_mechanism: String,
    pub minimal_change: String,
    pub machine_diff: Option<String>,
    pub capability_impact: bool,
    pub privacy_impact: bool,
    pub security_impact: bool,
    pub resource_impact: bool,
    pub cohort: ImprovementCohort,
    pub source_task_set: Vec<String>,
    pub held_out_task_set: Vec<String>,
    pub adversarial_task_set: Vec<String>,
    pub negative_task_set: Vec<String>,
    pub simpler_alternatives: Vec<String>,
    pub evaluation_plan: String,
    pub approval_requirements: Vec<String>,
    pub release_requirements: Vec<String>,
    pub exact_rollback: String,
    pub stop_conditions: Vec<String>,
    pub boundary: ImprovementBoundary,
    pub status: MetaImprovementStatus,
    pub created_at_unix_ms: i64,
}

impl MetaImprovementProposal {
    pub fn validate(&self) -> Result<(), MetaImprovementError> {
        let distinct_sources = self.source_trajectory_ids.iter().collect::<BTreeSet<_>>();
        if self.id.trim().is_empty()
            || self.frequency < MIN_REPEATED_SIGNALS
            || self.source_signal_ids.len() < MIN_REPEATED_SIGNALS
            || distinct_sources.len() < MIN_REPEATED_SIGNALS
            || self.friction_cluster.trim().is_empty()
            || self.current_behavior.trim().is_empty()
            || self.hypothesized_mechanism.trim().is_empty()
            || self.minimal_change.trim().is_empty()
            || self.evaluation_plan.trim().is_empty()
            || self.exact_rollback.trim().is_empty()
            || self.boundary.component_owner.trim().is_empty()
            || self.boundary.reviewer_class.trim().is_empty()
            || self.boundary.protected_roots.is_empty()
            || self.boundary.permitted_scope.trim().is_empty()
            || self.boundary.release_path.trim().is_empty()
        {
            return Err(MetaImprovementError::Validation(
                "meta-improvement proposal is incomplete or lacks repeated source trajectories"
                    .into(),
            ));
        }
        if self.capability_impact && self.lane != MetaImprovementLane::EngineeringProposal {
            return Err(MetaImprovementError::Policy(
                "capability-affecting improvements must use the engineering lane".into(),
            ));
        }
        if self.target.capability_sensitive()
            && self.lane != MetaImprovementLane::EngineeringProposal
        {
            return Err(MetaImprovementError::Policy(
                "capability-sensitive targets cannot be runtime refinements".into(),
            ));
        }
        if self.lane == MetaImprovementLane::RuntimeRefinement && !self.target.runtime_safe() {
            return Err(MetaImprovementError::Policy(
                "runtime lane target is outside the declarative refinement boundary".into(),
            ));
        }
        if self.lane == MetaImprovementLane::EngineeringProposal
            && self.machine_diff.as_deref().is_none_or(str::is_empty)
        {
            return Err(MetaImprovementError::Validation(
                "engineering proposals require a machine-readable diff or explicit issue patch"
                    .into(),
            ));
        }
        Ok(())
    }

    pub fn validate_for_activation(
        &self,
        proof: &RuntimeActivationProof,
    ) -> Result<(), MetaImprovementError> {
        self.validate()?;
        if self.lane != MetaImprovementLane::RuntimeRefinement {
            return Err(MetaImprovementError::Policy(
                "only runtime refinements can be activated by this path".into(),
            ));
        }
        if self.capability_impact || self.privacy_impact || self.security_impact {
            return Err(MetaImprovementError::Policy(
                "capability, privacy, and security changes require the engineering lane".into(),
            ));
        }
        if proof.reviewer_id.trim().is_empty()
            || proof.reviewer_id == self.component_owner
            || !proof.reviewer_independent
            || proof.frozen_oracle_digest.trim().is_empty()
            || proof.held_out_task_ids.is_empty()
            || proof.adversarial_task_ids.is_empty()
            || proof.negative_task_ids.is_empty()
            || proof.exact_rollback.trim().is_empty()
            || proof.release_id.trim().is_empty()
        {
            return Err(MetaImprovementError::Policy(
                "runtime activation requires independent review, frozen held-out/adversarial/negative tasks, release, and rollback evidence".into(),
            ));
        }
        Ok(())
    }

    pub fn canonical_refinement(&self) -> Result<RefinementProposal, MetaImprovementError> {
        self.validate()?;
        if self.lane != MetaImprovementLane::RuntimeRefinement {
            return Err(MetaImprovementError::Policy(
                "engineering proposals cannot be projected as runtime refinements".into(),
            ));
        }
        let artifact_kind = match self.target {
            MetaImprovementTarget::PromptOverlay => RefinementArtifactKind::PromptGuidance,
            MetaImprovementTarget::RetrievalPolicy
            | MetaImprovementTarget::TestDiscovery
            | MetaImprovementTarget::RecoveryPlaybook
            | MetaImprovementTarget::OrchestrationPlaybook => {
                RefinementArtifactKind::WorkflowMetadata
            }
            _ => {
                return Err(MetaImprovementError::Policy(
                    "target has no declarative canonical refinement projection".into(),
                ));
            }
        };
        let evidence = self
            .source_signal_ids
            .iter()
            .map(|id| EvidenceRef {
                id: id.clone(),
                kind: EvidenceKind::ExplicitOutcome,
                trajectory_id: self
                    .source_trajectory_ids
                    .first()
                    .cloned()
                    .unwrap_or_default(),
                start_sequence: 1,
                end_sequence: 1,
            })
            .collect();
        let after = match artifact_kind {
            RefinementArtifactKind::PromptGuidance => RefinementContent::PromptGuidance {
                key: format!("meta.{}", self.id),
                guidance: self.minimal_change.clone(),
            },
            RefinementArtifactKind::WorkflowMetadata => RefinementContent::WorkflowMetadata {
                name: format!("meta.{}", self.id),
                summary: self.minimal_change.clone(),
            },
            _ => {
                return Err(MetaImprovementError::Policy(
                    "target has no declarative canonical refinement content".into(),
                ));
            }
        };
        Ok(RefinementProposal {
            id: self.id.clone(),
            version: 1,
            artifact_kind,
            scope: RefinementScope::Repository,
            evidence,
            before: None,
            after,
            rationale: self.hypothesized_mechanism.clone(),
            expected_outcome: self.evaluation_plan.clone(),
            proposer: ProposerMetadata {
                model: "medusa-meta-improvement".into(),
                route: format!("{}-lane", lane_name(self.lane)),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            risk: RefinementRisk::Low,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeActivationProof {
    pub reviewer_id: String,
    pub reviewer_independent: bool,
    pub frozen_oracle_digest: String,
    pub held_out_task_ids: Vec<String>,
    pub adversarial_task_ids: Vec<String>,
    pub negative_task_ids: Vec<String>,
    pub exact_rollback: String,
    pub release_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EngineeringRoute {
    pub proposal_id: String,
    pub issue_key: String,
    pub branch_name: String,
    pub requires_parent_review: bool,
    pub requires_independent_verification: bool,
    pub self_approval_forbidden: bool,
    pub deployment_forbidden: bool,
    pub escalation_required: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MetaImprovementRoute {
    Runtime {
        proposal_id: String,
        canonical_refinement_id: String,
        requires_external_review: bool,
        rollback: String,
    },
    Engineering(EngineeringRoute),
}

#[derive(Debug, thiserror::Error)]
pub enum MetaImprovementError {
    #[error("meta-improvement I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("meta-improvement serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("canonical refinement authority failed: {0}")]
    Authority(#[from] crate::refinement_authority::RefinementAuthorityError),
    #[error("meta-improvement validation failed: {0}")]
    Validation(String),
    #[error("meta-improvement policy denied: {0}")]
    Policy(String),
    #[error("meta-improvement proposal was not found: {0}")]
    NotFound(String),
}

fn lane_name(lane: MetaImprovementLane) -> &'static str {
    match lane {
        MetaImprovementLane::RuntimeRefinement => "runtime",
        MetaImprovementLane::EngineeringProposal => "engineering",
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetaImprovementSnapshot {
    pub schema_version: u32,
    pub revision: u64,
    pub signals: Vec<FrictionSignal>,
    pub proposals: Vec<MetaImprovementProposal>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MetaImprovementDocument {
    schema_version: u32,
    revision: u64,
    signals: BTreeMap<String, FrictionSignal>,
    proposals: BTreeMap<String, MetaImprovementProposal>,
}

impl Default for MetaImprovementDocument {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 0,
            signals: BTreeMap::new(),
            proposals: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MetaImprovementStore {
    root: PathBuf,
    document: MetaImprovementDocument,
}

impl MetaImprovementStore {
    pub fn open(repo: &Path) -> Result<Self, MetaImprovementError> {
        let root = repo.join(META_ROOT);
        let path = root.join("state.json");
        let document = if path.is_file() {
            let document: MetaImprovementDocument = serde_json::from_slice(&fs::read(path)?)?;
            if document.schema_version != SCHEMA_VERSION {
                return Err(MetaImprovementError::Validation(format!(
                    "unsupported meta-improvement schema {}",
                    document.schema_version
                )));
            }
            document
        } else {
            MetaImprovementDocument::default()
        };
        Ok(Self { root, document })
    }

    #[must_use]
    pub fn snapshot(&self) -> MetaImprovementSnapshot {
        MetaImprovementSnapshot {
            schema_version: self.document.schema_version,
            revision: self.document.revision,
            signals: self.document.signals.values().cloned().collect(),
            proposals: self.document.proposals.values().cloned().collect(),
        }
    }

    /// Ingests typed production friction and creates at most one proposal per stable cluster.
    /// Repeated observations remain individually addressable by signal id.
    pub fn record_signals(
        &mut self,
        repo: &Path,
        signals: impl IntoIterator<Item = FrictionSignal>,
        now_unix_ms: i64,
    ) -> Result<Vec<MetaImprovementProposal>, MetaImprovementError> {
        let mut added = false;
        for signal in signals {
            signal.validate()?;
            if let Some(existing) = self.document.signals.get(&signal.id) {
                if existing != &signal {
                    return Err(MetaImprovementError::Validation(format!(
                        "friction signal {} changed after recording",
                        signal.id
                    )));
                }
                continue;
            }
            self.document.signals.insert(signal.id.clone(), signal);
            added = true;
        }
        if !added {
            return Ok(Vec::new());
        }
        let mut created = Vec::new();
        for cluster in clusters(self.document.signals.values()) {
            let proposal_id = stable_id("meta", &[cluster.key.as_str()]);
            if self.document.proposals.contains_key(&proposal_id)
                || cluster.signals.len() < MIN_REPEATED_SIGNALS
            {
                continue;
            }
            let proposal = proposal_from_cluster(&cluster, now_unix_ms)?;
            proposal.validate()?;
            self.document
                .proposals
                .insert(proposal.id.clone(), proposal.clone());
            created.push(proposal);
        }
        self.commit(repo, "signals", now_unix_ms)?;
        Ok(created)
    }

    /// Converts typed provenance into conservative friction observations. Only failures,
    /// corrections, review revisions, and contradictory evidence become candidate-producing
    /// signals; successful evidence is retained as a negative/control example when it shares a
    /// source trajectory.
    pub fn record_provenance(
        &mut self,
        repo: &Path,
        graph: &ProvenanceGraph,
        model: &str,
        provider: &str,
        harness: &str,
        now_unix_ms: i64,
    ) -> Result<Vec<MetaImprovementProposal>, MetaImprovementError> {
        let signals = graph
            .observations
            .iter()
            .filter_map(|observation| {
                signal_from_observation(observation, model, provider, harness)
            })
            .collect::<Vec<_>>();
        if signals.is_empty() {
            return Ok(Vec::new());
        }
        self.record_signals(repo, signals, now_unix_ms)
    }

    pub fn route(&self, proposal_id: &str) -> Result<MetaImprovementRoute, MetaImprovementError> {
        let proposal = self
            .document
            .proposals
            .get(proposal_id)
            .ok_or_else(|| MetaImprovementError::NotFound(proposal_id.into()))?;
        proposal.validate()?;
        match proposal.lane {
            MetaImprovementLane::RuntimeRefinement => Ok(MetaImprovementRoute::Runtime {
                proposal_id: proposal.id.clone(),
                canonical_refinement_id: proposal.id.clone(),
                requires_external_review: true,
                rollback: proposal.exact_rollback.clone(),
            }),
            MetaImprovementLane::EngineeringProposal => {
                Ok(MetaImprovementRoute::Engineering(EngineeringRoute {
                    proposal_id: proposal.id.clone(),
                    issue_key: format!("medusa-meta-{}", proposal.id),
                    branch_name: format!("codex/meta-{}", proposal.id),
                    requires_parent_review: true,
                    requires_independent_verification: true,
                    self_approval_forbidden: true,
                    deployment_forbidden: true,
                    escalation_required: proposal.status == MetaImprovementStatus::Escalated,
                }))
            }
        }
    }

    pub fn activate_runtime(
        &mut self,
        repo: &Path,
        proposal_id: &str,
        proof: &RuntimeActivationProof,
        now_unix_ms: i64,
    ) -> Result<RefinementProposal, MetaImprovementError> {
        let proposal = self
            .document
            .proposals
            .get(proposal_id)
            .cloned()
            .ok_or_else(|| MetaImprovementError::NotFound(proposal_id.into()))?;
        if !matches!(
            proposal.status,
            MetaImprovementStatus::Proposed | MetaImprovementStatus::AwaitingReview
        ) {
            return Err(MetaImprovementError::Policy(
                "runtime activation requires a proposal awaiting external review".into(),
            ));
        }
        proposal.validate_for_activation(proof)?;
        let refinement = proposal.canonical_refinement()?;
        let mut authority = RefinementAuthorityStore::open(repo)?;
        let revision = authority.snapshot()?.revision;
        authority.propose(refinement.clone(), revision)?;
        let revision = authority.snapshot()?.revision;
        authority.validate(&refinement.id, refinement.version, revision)?;
        let revision = authority.snapshot()?.revision;
        authority.record_evaluation(
            &refinement.id,
            refinement.version,
            EvaluationResult {
                evaluator: proof.reviewer_id.clone(),
                validation_passed: true,
                regression_passed: true,
                effectiveness_passed: true,
                notes: proof.frozen_oracle_digest.clone(),
            },
            revision,
        )?;
        let revision = authority.snapshot()?.revision;
        let decision_id = format!("{}:{}", proposal.id, proof.release_id);
        authority.approve(
            &refinement.id,
            refinement.version,
            ApprovalActorClass::Reviewer,
            &decision_id,
            now_unix_ms,
            revision,
        )?;
        let revision = authority.snapshot()?.revision;
        authority.activate(&refinement.id, refinement.version, revision)?;
        let proposal = self
            .document
            .proposals
            .get_mut(proposal_id)
            .ok_or_else(|| MetaImprovementError::NotFound(proposal_id.into()))?;
        proposal.held_out_task_set = proof.held_out_task_ids.clone();
        proposal.adversarial_task_set = proof.adversarial_task_ids.clone();
        proposal.negative_task_set = proof.negative_task_ids.clone();
        proposal.exact_rollback = proof.exact_rollback.clone();
        proposal.release_requirements.push(proof.release_id.clone());
        proposal.status = MetaImprovementStatus::Activated;
        self.commit(repo, "runtime_activated", now_unix_ms)?;
        Ok(refinement)
    }

    pub fn mark_rolled_back(
        &mut self,
        repo: &Path,
        proposal_id: &str,
        exact_rollback: &str,
        now_unix_ms: i64,
    ) -> Result<(), MetaImprovementError> {
        let proposal = self
            .document
            .proposals
            .get_mut(proposal_id)
            .ok_or_else(|| MetaImprovementError::NotFound(proposal_id.into()))?;
        if proposal.status != MetaImprovementStatus::Activated
            || exact_rollback.trim().is_empty()
            || exact_rollback != proposal.exact_rollback
        {
            return Err(MetaImprovementError::Policy(
                "rollback must identify the exact activated proposal predecessor".into(),
            ));
        }
        let mut authority = RefinementAuthorityStore::open(repo)?;
        let revision = authority.snapshot()?.revision;
        authority.rollback(proposal_id, 1, None, None, exact_rollback, revision)?;
        proposal.status = MetaImprovementStatus::RolledBack;
        self.commit(repo, "runtime_rolled_back", now_unix_ms)
    }

    fn commit(
        &mut self,
        _repo: &Path,
        kind: &str,
        now_unix_ms: i64,
    ) -> Result<(), MetaImprovementError> {
        self.document.revision = self.document.revision.saturating_add(1);
        fs::create_dir_all(&self.root)?;
        let state = self.root.join("state.json");
        let temporary = self.root.join(format!("state.tmp-{}", std::process::id()));
        fs::write(&temporary, serde_json::to_vec_pretty(&self.document)?)?;
        if state.exists() {
            fs::remove_file(&state)?;
        }
        fs::rename(temporary, state)?;
        let event = serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "revision": self.document.revision,
            "kind": kind,
            "recorded_at_unix_ms": now_unix_ms,
        });
        let mut events = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("events.jsonl"))?;
        serde_json::to_writer(&mut events, &event)?;
        use std::io::Write;
        events.write_all(b"\n")?;
        events.sync_data()?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct SignalCluster {
    key: String,
    signals: Vec<FrictionSignal>,
}

fn clusters<'a>(signals: impl Iterator<Item = &'a FrictionSignal>) -> Vec<SignalCluster> {
    let mut grouped = BTreeMap::<String, Vec<FrictionSignal>>::new();
    for signal in signals {
        let key = cluster_key(signal);
        grouped.entry(key).or_default().push(signal.clone());
    }
    grouped
        .into_iter()
        .map(|(key, signals)| SignalCluster { key, signals })
        .collect()
}

fn cluster_key(signal: &FrictionSignal) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}",
        signal.kind.trim().to_ascii_lowercase(),
        normalize_detail(&signal.detail),
        signal.cohort.model,
        signal.cohort.provider,
        signal.cohort.harness,
        signal.cohort.repository_revision,
        signal.cohort.task_class,
        signal.capability_impact
    )
}

fn normalize_detail(detail: &str) -> String {
    detail
        .split_whitespace()
        .map(|part| part.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

fn proposal_from_cluster(
    cluster: &SignalCluster,
    created_at_unix_ms: i64,
) -> Result<MetaImprovementProposal, MetaImprovementError> {
    let first = cluster
        .signals
        .first()
        .ok_or_else(|| MetaImprovementError::Validation("empty friction cluster".into()))?;
    let target = target_for_signal(first);
    let lane = if target.runtime_safe() && !first.capability_impact {
        MetaImprovementLane::RuntimeRefinement
    } else {
        MetaImprovementLane::EngineeringProposal
    };
    let source_signal_ids = cluster
        .signals
        .iter()
        .map(|signal| signal.id.clone())
        .collect::<Vec<_>>();
    let authoritative_signal_ids = cluster
        .signals
        .iter()
        .filter(|signal| signal.authoritative)
        .map(|signal| signal.id.clone())
        .collect::<Vec<_>>();
    let mut source_trajectory_ids = cluster
        .signals
        .iter()
        .map(|signal| signal.trajectory_id.clone())
        .collect::<Vec<_>>();
    source_trajectory_ids.sort();
    source_trajectory_ids.dedup();
    let mut source_task_set = cluster
        .signals
        .iter()
        .map(|signal| signal.task_class.clone())
        .collect::<Vec<_>>();
    source_task_set.sort();
    source_task_set.dedup();
    let component_owner = owner_for_target(target).to_owned();
    let capability_impact = first.capability_impact || target.capability_sensitive();
    let friction_cluster = cluster.key.clone();
    let minimal_change = minimal_change_for_target(target);
    let machine_diff = (lane == MetaImprovementLane::EngineeringProposal)
        .then(|| "attach an ordinary reviewed patch or issue diff before activation".to_owned());
    let mut protected_roots = BTreeSet::from([
        "crates/medusa-capabilities".to_owned(),
        "crates/medusa-process-containment".to_owned(),
        "crates/medusa-transaction-coordinator".to_owned(),
        ".github".to_owned(),
        "Cargo.toml".to_owned(),
    ]);
    if lane == MetaImprovementLane::RuntimeRefinement {
        protected_roots.insert("eval/frozen".to_owned());
    }
    let mut approval_requirements = vec![
        "parent review of source trajectories and negative examples".to_owned(),
        "independent verification outside the proposal-producing path".to_owned(),
    ];
    if lane == MetaImprovementLane::EngineeringProposal {
        approval_requirements.push("ordinary issue, branch, review, and merge authority".into());
    } else {
        approval_requirements.push("canonical refinement authority activation receipt".into());
    }
    Ok(MetaImprovementProposal {
        id: stable_id("meta", &[&friction_cluster]),
        lane,
        target,
        component_owner: component_owner.clone(),
        source_signal_ids,
        authoritative_signal_ids,
        source_trajectory_ids,
        friction_cluster,
        frequency: cluster.signals.len(),
        impact_milli: impact_milli(&cluster.signals),
        current_behavior: format!("repeated {}: {}", first.kind, first.detail),
        hypothesized_mechanism: format!(
            "the same typed {} friction recurs across matching {} cohorts",
            first.kind, first.task_class
        ),
        minimal_change,
        machine_diff,
        capability_impact,
        privacy_impact: false,
        security_impact: target.capability_sensitive(),
        resource_impact: matches!(target, MetaImprovementTarget::ModelRouting),
        cohort: first.cohort.clone(),
        source_task_set,
        held_out_task_set: Vec::new(),
        adversarial_task_set: Vec::new(),
        negative_task_set: Vec::new(),
        simpler_alternatives: vec![
            "retain the evidence as scoped memory or a skill without changing harness behavior"
                .into(),
            "continue collecting matched evidence without a durable change".into(),
        ],
        evaluation_plan: "compare source, held-out, adversarial, and negative task sets within the same model/provider/harness/repository cohort".into(),
        approval_requirements,
        release_requirements: vec!["canary release with #822 monitor and exact rollback receipt".into()],
        exact_rollback: "restore the prior canonical projection or revert the reviewed commit".into(),
        stop_conditions: vec![
            "privacy, capability, approval, or safety violation".into(),
            "negative matched outcome exceeds the predeclared monitor threshold".into(),
            "cohort or repository drift without reevaluation".into(),
        ],
        boundary: ImprovementBoundary {
            component_owner,
            reviewer_class: "independent-human-or-maintainer".into(),
            protected_roots,
            permitted_scope: if lane == MetaImprovementLane::RuntimeRefinement {
                "repository-scoped declarative runtime projection only".into()
            } else {
                "ordinary reviewed repository transaction only".into()
            },
            max_capability_impact: false,
            release_path: if lane == MetaImprovementLane::RuntimeRefinement {
                "canonical refinement authority plus monitored canary".into()
            } else {
                "normal issue, branch, CI, merge, and release authority".into()
            },
        },
        status: if requires_escalation(first) {
            MetaImprovementStatus::Escalated
        } else {
            MetaImprovementStatus::AwaitingReview
        },
        created_at_unix_ms,
    })
}

fn target_for_signal(signal: &FrictionSignal) -> MetaImprovementTarget {
    let kind = signal.kind.to_ascii_lowercase();
    let detail = signal.detail.to_ascii_lowercase();
    if detail.contains("change the evaluator")
        || detail.contains("modify the evaluator")
        || detail.contains("approval rule")
        || detail.contains("protected root")
    {
        return MetaImprovementTarget::EvaluationHarness;
    }
    if signal.capability_impact
        || [
            "permission",
            "capability",
            "approval",
            "credential",
            "policy",
        ]
        .iter()
        .any(|marker| kind.contains(marker) || detail.contains(marker))
    {
        return MetaImprovementTarget::CapabilityPolicy;
    }
    if kind.contains("context") || kind.contains("retrieval") {
        MetaImprovementTarget::RetrievalPolicy
    } else if kind.contains("tool") {
        MetaImprovementTarget::ToolDescription
    } else if kind.contains("command") {
        MetaImprovementTarget::RecoveryPlaybook
    } else if kind.contains("test") {
        MetaImprovementTarget::TestDiscovery
    } else if kind.contains("recover") || kind.contains("retry") {
        MetaImprovementTarget::RecoveryPlaybook
    } else if kind.contains("orches") || kind.contains("handoff") || kind.contains("review") {
        MetaImprovementTarget::OrchestrationPlaybook
    } else if kind.contains("routing") || kind.contains("model") {
        MetaImprovementTarget::ModelRouting
    } else if kind.contains("harness") || kind.contains("code") {
        MetaImprovementTarget::HarnessCode
    } else {
        MetaImprovementTarget::PromptOverlay
    }
}

fn requires_escalation(signal: &FrictionSignal) -> bool {
    let detail = signal.detail.to_ascii_lowercase();
    detail.contains("change the evaluator")
        || detail.contains("modify the evaluator")
        || detail.contains("approval rule")
        || detail.contains("protected root")
}

fn owner_for_target(target: MetaImprovementTarget) -> &'static str {
    match target {
        MetaImprovementTarget::PromptOverlay => "prompt-runtime",
        MetaImprovementTarget::ToolDescription => "tool-authority",
        MetaImprovementTarget::RetrievalPolicy => "retrieval-runtime",
        MetaImprovementTarget::TestDiscovery => "verification-runtime",
        MetaImprovementTarget::RecoveryPlaybook => "recovery-runtime",
        MetaImprovementTarget::OrchestrationPlaybook => "orchestration-runtime",
        MetaImprovementTarget::ModelRouting => "provider-routing",
        MetaImprovementTarget::HarnessCode => "harness-maintainers",
        MetaImprovementTarget::CapabilityPolicy => "capability-maintainers",
        MetaImprovementTarget::EvaluationHarness => "evaluation-maintainers",
    }
}

fn minimal_change_for_target(target: MetaImprovementTarget) -> String {
    match target {
        MetaImprovementTarget::PromptOverlay => {
            "add a bounded repository-scoped prompt overlay for the repeated task class".into()
        }
        MetaImprovementTarget::ToolDescription => {
            "clarify the existing tool description without adding capability or authority".into()
        }
        MetaImprovementTarget::RetrievalPolicy => {
            "adjust bounded retrieval matching/ranking weights and preserve abstention".into()
        }
        MetaImprovementTarget::TestDiscovery => {
            "add repository-scoped test or command discovery knowledge".into()
        }
        MetaImprovementTarget::RecoveryPlaybook => {
            "add a non-executable recovery playbook with explicit stop conditions".into()
        }
        MetaImprovementTarget::OrchestrationPlaybook => {
            "add a declarative orchestration checklist without changing authority".into()
        }
        MetaImprovementTarget::ModelRouting => {
            "propose routing changes only inside the approved provider/model/cost envelope".into()
        }
        MetaImprovementTarget::HarnessCode => {
            "prepare an ordinary reviewed code change in an isolated branch".into()
        }
        MetaImprovementTarget::CapabilityPolicy => {
            "escalate for ordinary policy review; do not change the policy from the learning lane"
                .into()
        }
        MetaImprovementTarget::EvaluationHarness => {
            "prepare a separately reviewed evaluation-harness change with a frozen oracle".into()
        }
    }
}

fn impact_milli(signals: &[FrictionSignal]) -> u16 {
    let failures = signals.iter().filter(|signal| !signal.success).count();
    let authoritative = signals.iter().filter(|signal| signal.authoritative).count();
    (u16::try_from(failures.saturating_mul(150) + authoritative.saturating_mul(50))
        .unwrap_or(u16::MAX))
    .min(1_000)
}

fn signal_from_observation(
    observation: &ProvenanceObservation,
    model: &str,
    provider: &str,
    harness: &str,
) -> Option<FrictionSignal> {
    let kind = match observation.source {
        ProvenanceSource::UserCorrection => "user_correction",
        ProvenanceSource::ToolExecution | ProvenanceSource::ToolTelemetry
            if observation.outcome != ProvenanceOutcome::Positive =>
        {
            "tool_failure"
        }
        ProvenanceSource::Verification if observation.outcome != ProvenanceOutcome::Positive => {
            "verification_failure"
        }
        ProvenanceSource::Recovery if observation.outcome != ProvenanceOutcome::Positive => {
            "recovery_failure"
        }
        ProvenanceSource::ParentReview => "parent_review",
        ProvenanceSource::Integration if observation.outcome != ProvenanceOutcome::Positive => {
            "integration_failure"
        }
        _ => return None,
    };
    let detail = normalize_detail(&observation.summary);
    let capability_impact = ["permission", "capability", "approval", "credential"]
        .iter()
        .any(|marker| detail.contains(marker));
    Some(FrictionSignal {
        id: observation.id.clone(),
        session_id: observation.session_id.clone(),
        trajectory_id: observation.trajectory_id.clone(),
        repository_revision: observation
            .revision
            .clone()
            .unwrap_or_else(|| "unknown".into()),
        kind: kind.into(),
        detail,
        evidence_id: observation.id.clone(),
        success: observation.outcome == ProvenanceOutcome::Positive,
        authoritative: matches!(
            observation.authority,
            ProvenanceAuthority::IndependentVerification
                | ProvenanceAuthority::ParentReview
                | ProvenanceAuthority::CoordinatorReceipt
        ),
        negative_example: observation.outcome != ProvenanceOutcome::Positive,
        capability_impact,
        task_class: observation.scope.clone(),
        cohort: ImprovementCohort {
            model: model.to_owned(),
            provider: provider.to_owned(),
            harness: harness.to_owned(),
            repository_revision: observation
                .revision
                .clone()
                .unwrap_or_else(|| "unknown".into()),
            task_class: observation.scope.clone(),
        },
        recorded_at_unix_ms: observation.observed_at.unix_timestamp_nanos() as i64 / 1_000_000,
    })
}

fn stable_id(kind: &str, values: &[&str]) -> String {
    let mut bytes = kind.as_bytes().to_vec();
    for value in values {
        bytes.push(0);
        bytes.extend_from_slice(value.as_bytes());
    }
    let digest = sha2::Sha256::digest(&bytes);
    format!("{kind}-{}", crate::encode(digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(
        id: &str,
        trajectory_id: &str,
        kind: &str,
        detail: &str,
        capability: bool,
    ) -> FrictionSignal {
        FrictionSignal {
            id: id.into(),
            session_id: format!("session-{trajectory_id}"),
            trajectory_id: trajectory_id.into(),
            repository_revision: "revision-1".into(),
            kind: kind.into(),
            detail: detail.into(),
            evidence_id: format!("evidence-{id}"),
            success: false,
            authoritative: false,
            negative_example: true,
            capability_impact: capability,
            task_class: "repository-maintenance".into(),
            cohort: ImprovementCohort {
                model: "model-a".into(),
                provider: "provider-a".into(),
                harness: "harness-a".into(),
                repository_revision: "revision-1".into(),
                task_class: "repository-maintenance".into(),
            },
            recorded_at_unix_ms: 1,
        }
    }

    #[test]
    fn repeated_context_friction_creates_reviewed_runtime_route() {
        let repo = tempfile::tempdir().expect("repo");
        let mut store = MetaImprovementStore::open(repo.path()).expect("store");
        assert!(
            store
                .record_signals(
                    repo.path(),
                    [
                        signal(
                            "s1",
                            "trajectory-1",
                            "context_selection",
                            "retrieval missed repository map",
                            false
                        ),
                        signal(
                            "s2",
                            "trajectory-2",
                            "context_selection",
                            "retrieval missed repository map",
                            false
                        ),
                    ],
                    2,
                )
                .expect("record")
                .len()
                == 1
        );
        let proposal = store
            .snapshot()
            .proposals
            .into_iter()
            .next()
            .expect("proposal");
        assert_eq!(proposal.lane, MetaImprovementLane::RuntimeRefinement);
        assert_eq!(proposal.target, MetaImprovementTarget::RetrievalPolicy);
        assert!(matches!(
            store.route(&proposal.id).expect("route"),
            MetaImprovementRoute::Runtime {
                requires_external_review: true,
                ..
            }
        ));
        assert_eq!(
            proposal
                .canonical_refinement()
                .expect("refinement")
                .artifact_kind,
            RefinementArtifactKind::WorkflowMetadata
        );
    }

    #[test]
    fn capability_sensitive_friction_routes_to_ordinary_engineering_review() {
        let repo = tempfile::tempdir().expect("repo");
        let mut store = MetaImprovementStore::open(repo.path()).expect("store");
        let proposals = store
            .record_signals(
                repo.path(),
                [
                    signal(
                        "s1",
                        "trajectory-1",
                        "tool_failure",
                        "tool description caused capability ambiguity",
                        true,
                    ),
                    signal(
                        "s2",
                        "trajectory-2",
                        "tool_failure",
                        "tool description caused capability ambiguity",
                        true,
                    ),
                ],
                2,
            )
            .expect("record");
        let proposal = proposals.into_iter().next().expect("proposal");
        assert_eq!(proposal.lane, MetaImprovementLane::EngineeringProposal);
        let MetaImprovementRoute::Engineering(route) = store.route(&proposal.id).expect("route")
        else {
            panic!("capability-sensitive target escaped engineering lane");
        };
        assert!(route.requires_parent_review);
        assert!(route.requires_independent_verification);
        assert!(route.self_approval_forbidden);
        assert!(route.deployment_forbidden);
    }

    #[test]
    fn evaluator_or_protected_boundary_change_is_escalated() {
        let repo = tempfile::tempdir().expect("repo");
        let mut store = MetaImprovementStore::open(repo.path()).expect("store");
        let proposals = store
            .record_signals(
                repo.path(),
                [
                    signal(
                        "s1",
                        "trajectory-1",
                        "harness_code",
                        "attempt to change the evaluator",
                        false,
                    ),
                    signal(
                        "s2",
                        "trajectory-2",
                        "harness_code",
                        "attempt to change the evaluator",
                        false,
                    ),
                ],
                2,
            )
            .expect("record");
        let proposal = proposals.into_iter().next().expect("proposal");
        assert_eq!(proposal.status, MetaImprovementStatus::Escalated);
        let MetaImprovementRoute::Engineering(route) = store.route(&proposal.id).expect("route")
        else {
            panic!("governance change escaped engineering route");
        };
        assert!(route.escalation_required);
        assert!(route.self_approval_forbidden);
    }

    #[test]
    fn runtime_activation_requires_external_evaluation_and_exact_rollback() {
        let repo = tempfile::tempdir().expect("repo");
        let mut store = MetaImprovementStore::open(repo.path()).expect("store");
        let proposal = store
            .record_signals(
                repo.path(),
                [
                    signal(
                        "s1",
                        "trajectory-1",
                        "context_selection",
                        "retrieval missed repository map",
                        false,
                    ),
                    signal(
                        "s2",
                        "trajectory-2",
                        "context_selection",
                        "retrieval missed repository map",
                        false,
                    ),
                ],
                2,
            )
            .expect("record")
            .into_iter()
            .next()
            .expect("proposal");
        let mut proof = RuntimeActivationProof {
            reviewer_id: proposal.component_owner.clone(),
            reviewer_independent: true,
            frozen_oracle_digest: "oracle".into(),
            held_out_task_ids: vec!["held-out".into()],
            adversarial_task_ids: vec!["adversarial".into()],
            negative_task_ids: vec!["negative".into()],
            exact_rollback: "restore prior refinement".into(),
            release_id: "release-1".into(),
        };
        assert!(
            store
                .activate_runtime(repo.path(), &proposal.id, &proof, 3)
                .is_err()
        );
        proof.reviewer_id = "independent-maintainer".into();
        let refinement = store
            .activate_runtime(repo.path(), &proposal.id, &proof, 4)
            .expect("activate");
        assert_eq!(refinement.id, proposal.id);
        let authority = RefinementAuthorityStore::open(repo.path()).expect("authority");
        assert_eq!(
            authority
                .snapshot()
                .expect("authority snapshot")
                .active
                .len(),
            1
        );
        assert!(
            store
                .mark_rolled_back(repo.path(), &proposal.id, "wrong rollback", 5)
                .is_err()
        );
        store
            .mark_rolled_back(repo.path(), &proposal.id, &proof.exact_rollback, 6)
            .expect("rollback");
        let reopened = MetaImprovementStore::open(repo.path()).expect("reopen");
        assert_eq!(
            reopened.snapshot().proposals[0].status,
            MetaImprovementStatus::RolledBack
        );
        let authority = RefinementAuthorityStore::open(repo.path()).expect("authority");
        assert!(
            authority
                .snapshot()
                .expect("authority snapshot")
                .active
                .is_empty()
        );
    }

    #[test]
    fn one_observation_is_persisted_without_creating_a_durable_change() {
        let repo = tempfile::tempdir().expect("repo");
        let mut store = MetaImprovementStore::open(repo.path()).expect("store");
        let proposals = store
            .record_signals(
                repo.path(),
                [signal(
                    "s1",
                    "trajectory-1",
                    "command_failure",
                    "command failed",
                    false,
                )],
                2,
            )
            .expect("record");
        assert!(proposals.is_empty());
        let snapshot = MetaImprovementStore::open(repo.path())
            .expect("reopen")
            .snapshot();
        assert_eq!(snapshot.signals.len(), 1);
        assert!(snapshot.proposals.is_empty());
    }

    #[test]
    fn repeated_command_failures_become_a_repository_scoped_recovery_playbook() {
        let repo = tempfile::tempdir().expect("repo");
        let mut store = MetaImprovementStore::open(repo.path()).expect("store");
        let proposal = store
            .record_signals(
                repo.path(),
                [
                    signal(
                        "s1",
                        "trajectory-1",
                        "command_failure",
                        "cargo test command failed",
                        false,
                    ),
                    signal(
                        "s2",
                        "trajectory-2",
                        "command_failure",
                        "cargo test command failed",
                        false,
                    ),
                ],
                2,
            )
            .expect("record")
            .into_iter()
            .next()
            .expect("proposal");
        assert_eq!(proposal.target, MetaImprovementTarget::RecoveryPlaybook);
        assert_eq!(proposal.lane, MetaImprovementLane::RuntimeRefinement);
        assert_eq!(
            proposal.boundary.permitted_scope,
            "repository-scoped declarative runtime projection only"
        );
        assert!(proposal.minimal_change.contains("non-executable"));
    }

    #[test]
    fn model_routing_candidate_stays_inside_the_declared_envelope() {
        let repo = tempfile::tempdir().expect("repo");
        let mut store = MetaImprovementStore::open(repo.path()).expect("store");
        let proposal = store
            .record_signals(
                repo.path(),
                [
                    signal(
                        "s1",
                        "trajectory-1",
                        "model_routing",
                        "provider timeout",
                        false,
                    ),
                    signal(
                        "s2",
                        "trajectory-2",
                        "model_routing",
                        "provider timeout",
                        false,
                    ),
                ],
                2,
            )
            .expect("record")
            .into_iter()
            .next()
            .expect("proposal");
        assert_eq!(proposal.target, MetaImprovementTarget::ModelRouting);
        assert_eq!(proposal.lane, MetaImprovementLane::RuntimeRefinement);
        assert!(
            proposal
                .minimal_change
                .contains("approved provider/model/cost envelope")
        );
        assert!(proposal.resource_impact);
    }
}
