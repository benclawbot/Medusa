use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::{ContextItem, ContextKind};

const GENESIS_HASH: &str = "genesis";
const IMMUTABLE_ROOTS: &[&str] = &[
    "system",
    "security",
    "authority",
    "containment",
    "approval",
    "capability",
    "verification",
    "repository_mutation",
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefinementArtifactKind {
    Memory,
    RepositoryConvention,
    WorkflowMetadata,
    TeamRoleMetadata,
    PromptGuidance,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefinementScope {
    Session,
    Repository,
    User,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefinementRisk {
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    UserMessage,
    UserCorrection,
    ToolEvent,
    ExplicitOutcome,
    RepositoryContent,
    WebContent,
    ProviderThinking,
}

impl EvidenceKind {
    fn trusted_for_persistent_instruction(self) -> bool {
        matches!(
            self,
            Self::UserMessage | Self::UserCorrection | Self::ToolEvent | Self::ExplicitOutcome
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceRef {
    pub id: String,
    pub kind: EvidenceKind,
    pub trajectory_id: String,
    pub start_sequence: u64,
    pub end_sequence: u64,
}

impl EvidenceRef {
    fn validate(&self) -> Result<(), RefinementError> {
        if self.id.trim().is_empty()
            || self.trajectory_id.trim().is_empty()
            || self.start_sequence == 0
            || self.end_sequence < self.start_sequence
        {
            return Err(RefinementError::InvalidEvidence);
        }
        if self.kind == EvidenceKind::ProviderThinking {
            return Err(RefinementError::HiddenReasoningEvidence);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RefinementContent {
    Memory { key: String, value: String },
    RepositoryConvention { key: String, value: String },
    WorkflowMetadata { name: String, summary: String },
    TeamRoleMetadata { name: String, guidance: String },
    PromptGuidance { key: String, guidance: String },
}

impl RefinementContent {
    fn identity(&self) -> &str {
        match self {
            Self::Memory { key, .. }
            | Self::RepositoryConvention { key, .. }
            | Self::PromptGuidance { key, .. } => key,
            Self::WorkflowMetadata { name, .. } | Self::TeamRoleMetadata { name, .. } => name,
        }
    }

    fn body(&self) -> &str {
        match self {
            Self::Memory { value, .. } | Self::RepositoryConvention { value, .. } => value,
            Self::WorkflowMetadata { summary, .. } => summary,
            Self::TeamRoleMetadata { guidance, .. } | Self::PromptGuidance { guidance, .. } => {
                guidance
            }
        }
    }

    fn validate(&self) -> Result<(), RefinementError> {
        if self.identity().trim().is_empty() || self.body().trim().is_empty() {
            return Err(RefinementError::InvalidProposal);
        }
        let identity = self
            .identity()
            .trim()
            .to_ascii_lowercase()
            .replace('-', "_");
        if IMMUTABLE_ROOTS
            .iter()
            .any(|root| identity == *root || identity.starts_with(&format!("{root}.")))
        {
            return Err(RefinementError::ImmutableAuthorityRoot);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProposerMetadata {
    pub model: String,
    pub route: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RefinementProposal {
    pub id: String,
    pub version: u64,
    pub artifact_kind: RefinementArtifactKind,
    pub scope: RefinementScope,
    pub evidence: Vec<EvidenceRef>,
    pub before: Option<RefinementContent>,
    pub after: RefinementContent,
    pub rationale: String,
    pub expected_outcome: String,
    pub proposer: ProposerMetadata,
    pub risk: RefinementRisk,
}

impl RefinementProposal {
    pub fn validate(&self) -> Result<(), RefinementError> {
        if self.id.trim().is_empty()
            || self.version == 0
            || self.rationale.trim().is_empty()
            || self.expected_outcome.trim().is_empty()
            || self.proposer.model.trim().is_empty()
            || self.proposer.route.trim().is_empty()
            || self.proposer.version.trim().is_empty()
        {
            return Err(RefinementError::InvalidProposal);
        }
        if self.evidence.is_empty() {
            return Err(RefinementError::InvalidEvidence);
        }
        for evidence in &self.evidence {
            evidence.validate()?;
        }
        if !self
            .evidence
            .iter()
            .any(|evidence| evidence.kind.trusted_for_persistent_instruction())
        {
            return Err(RefinementError::UntrustedEvidenceOnly);
        }
        self.after.validate()?;
        if let Some(before) = &self.before {
            before.validate()?;
            if before.identity() != self.after.identity() {
                return Err(RefinementError::InvalidProposal);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvaluationResult {
    pub evaluator: String,
    pub validation_passed: bool,
    pub regression_passed: bool,
    pub effectiveness_passed: bool,
    pub notes: String,
}

impl EvaluationResult {
    fn is_well_formed(&self) -> bool {
        !self.evaluator.trim().is_empty()
    }

    fn passed(&self) -> bool {
        self.is_well_formed()
            && self.validation_passed
            && self.regression_passed
            && self.effectiveness_passed
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApprovalReceipt {
    pub approver: String,
    pub receipt_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RefinementEvent {
    Proposed {
        proposal: RefinementProposal,
    },
    Validated {
        proposal_id: String,
        version: u64,
    },
    Evaluated {
        proposal_id: String,
        version: u64,
        result: EvaluationResult,
    },
    Approved {
        proposal_id: String,
        version: u64,
        receipt: ApprovalReceipt,
    },
    Superseded {
        proposal_id: String,
        version: u64,
        by_proposal_id: String,
        by_version: u64,
    },
    Activated {
        proposal_id: String,
        version: u64,
    },
    RolledBack {
        proposal_id: String,
        version: u64,
        restore_proposal_id: Option<String>,
        restore_version: Option<u64>,
        reason: String,
    },
    Rejected {
        proposal_id: String,
        version: u64,
        reason: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JournalEntry {
    pub sequence: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    pub previous_hash: String,
    pub event: RefinementEvent,
    pub hash: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RefinementJournal {
    #[serde(default)]
    entries: Vec<JournalEntry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Lifecycle {
    Proposed,
    Validated,
    Evaluated,
    Approved,
    Active,
    Superseded,
    RolledBack,
    Rejected,
}

#[derive(Clone, Debug)]
struct ProposalState {
    proposal: RefinementProposal,
    lifecycle: Lifecycle,
    evaluation_passed: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RefinementProjection {
    active: BTreeMap<(RefinementScope, RefinementArtifactKind, String), RefinementProposal>,
}

impl RefinementProjection {
    #[must_use]
    pub fn active(&self) -> Vec<&RefinementProposal> {
        self.active.values().collect()
    }

    #[must_use]
    pub fn active_for_scope(&self, scope: RefinementScope) -> Vec<&RefinementProposal> {
        self.active
            .values()
            .filter(|proposal| proposal.scope == scope)
            .collect()
    }

    #[must_use]
    pub fn conflicts(&self, proposal: &RefinementProposal) -> Vec<&RefinementProposal> {
        self.active
            .values()
            .filter(|active| {
                active.artifact_kind == proposal.artifact_kind
                    && active.after.identity() == proposal.after.identity()
            })
            .collect()
    }

    pub fn context_items(
        &self,
        mut next_sequence: u64,
        recorded_at: OffsetDateTime,
    ) -> Result<Vec<ContextItem>, &'static str> {
        let mut items = Vec::new();
        for proposal in self.active.values() {
            let evidence = proposal
                .evidence
                .iter()
                .map(|reference| reference.id.as_str())
                .collect::<Vec<_>>()
                .join(",");
            items.push(ContextItem::new(
                format!("refinement:{}:{}", proposal.id, proposal.version),
                ContextKind::Evidence,
                format!(
                    concat!(
                        "active_refinement id={} version={} scope={:?} kind={:?} ",
                        "key={} value={} evidence=[{}]"
                    ),
                    proposal.id,
                    proposal.version,
                    proposal.scope,
                    proposal.artifact_kind,
                    proposal.after.identity(),
                    proposal.after.body(),
                    evidence
                ),
                next_sequence,
                recorded_at,
            )?);
            next_sequence += 1;
        }
        Ok(items)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryResult {
    pub projection: RefinementProjection,
    pub accepted_entries: usize,
    pub quarantined_entries: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefinementError {
    InvalidProposal,
    InvalidEvidence,
    HiddenReasoningEvidence,
    UntrustedEvidenceOnly,
    ImmutableAuthorityRoot,
    DuplicateProposalVersion,
    UnknownProposal,
    InvalidTransition,
    EvaluationFailed,
    ApprovalRequired,
    Conflict,
    CorruptJournal,
}

type ProposalKey = (String, u64);

impl RefinementJournal {
    #[must_use]
    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    pub fn append(
        &mut self,
        event: RefinementEvent,
        recorded_at: OffsetDateTime,
    ) -> Result<(), RefinementError> {
        self.validate_chain()?;
        let (states, projection) = replay(&self.entries)?;
        validate_transition(&event, &states, &projection)?;
        let sequence = self.entries.len() as u64 + 1;
        let previous_hash = self
            .entries
            .last()
            .map_or_else(|| GENESIS_HASH.to_owned(), |entry| entry.hash.clone());
        let hash = entry_hash(sequence, recorded_at, &previous_hash, &event);
        self.entries.push(JournalEntry {
            sequence,
            recorded_at,
            previous_hash,
            event,
            hash,
        });
        Ok(())
    }

    pub fn validate_chain(&self) -> Result<(), RefinementError> {
        let mut previous = GENESIS_HASH.to_owned();
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.sequence != index as u64 + 1 || entry.previous_hash != previous {
                return Err(RefinementError::CorruptJournal);
            }
            if entry.hash
                != entry_hash(
                    entry.sequence,
                    entry.recorded_at,
                    &entry.previous_hash,
                    &entry.event,
                )
            {
                return Err(RefinementError::CorruptJournal);
            }
            previous.clone_from(&entry.hash);
        }
        replay(&self.entries)?;
        Ok(())
    }

    pub fn projection(&self) -> Result<RefinementProjection, RefinementError> {
        self.validate_chain()?;
        Ok(replay(&self.entries)?.1)
    }

    #[must_use]
    pub fn recover(entries: &[JournalEntry]) -> RecoveryResult {
        let mut accepted = Vec::new();
        for entry in entries {
            let mut candidate = accepted.clone();
            candidate.push(entry.clone());
            if (Self { entries: candidate }).validate_chain().is_err() {
                break;
            }
            accepted.push(entry.clone());
        }
        let projection = replay(&accepted)
            .map_or_else(|_| RefinementProjection::default(), |value| value.1);
        RecoveryResult {
            projection,
            accepted_entries: accepted.len(),
            quarantined_entries: entries.len().saturating_sub(accepted.len()),
        }
    }
}

fn proposal_key(id: &str, version: u64) -> ProposalKey {
    (id.to_owned(), version)
}

fn projection_key(
    proposal: &RefinementProposal,
) -> (RefinementScope, RefinementArtifactKind, String) {
    (
        proposal.scope,
        proposal.artifact_kind,
        proposal.after.identity().to_owned(),
    )
}

fn event_key(event: &RefinementEvent) -> ProposalKey {
    match event {
        RefinementEvent::Proposed { proposal } => proposal_key(&proposal.id, proposal.version),
        RefinementEvent::Validated {
            proposal_id,
            version,
        }
        | RefinementEvent::Evaluated {
            proposal_id,
            version,
            ..
        }
        | RefinementEvent::Approved {
            proposal_id,
            version,
            ..
        }
        | RefinementEvent::Superseded {
            proposal_id,
            version,
            ..
        }
        | RefinementEvent::Activated {
            proposal_id,
            version,
        }
        | RefinementEvent::RolledBack {
            proposal_id,
            version,
            ..
        }
        | RefinementEvent::Rejected {
            proposal_id,
            version,
            ..
        } => proposal_key(proposal_id, *version),
    }
}

fn state_for<'a>(
    event: &RefinementEvent,
    states: &'a BTreeMap<ProposalKey, ProposalState>,
) -> Result<&'a ProposalState, RefinementError> {
    states
        .get(&event_key(event))
        .ok_or(RefinementError::UnknownProposal)
}

fn validate_transition(
    event: &RefinementEvent,
    states: &BTreeMap<ProposalKey, ProposalState>,
    projection: &RefinementProjection,
) -> Result<(), RefinementError> {
    match event {
        RefinementEvent::Proposed { proposal } => {
            proposal.validate()?;
            if states.contains_key(&proposal_key(&proposal.id, proposal.version)) {
                return Err(RefinementError::DuplicateProposalVersion);
            }
            if let Some(before) = &proposal.before {
                let conflicts = projection.conflicts(proposal);
                if conflicts.len() != 1 || conflicts[0].after != *before {
                    return Err(RefinementError::Conflict);
                }
            }
            Ok(())
        }
        RefinementEvent::Validated { .. } => {
            require_lifecycle(event, states, Lifecycle::Proposed)
        }
        RefinementEvent::Evaluated { result, .. } => {
            require_lifecycle(event, states, Lifecycle::Validated)?;
            if !result.is_well_formed() {
                return Err(RefinementError::InvalidTransition);
            }
            Ok(())
        }
        RefinementEvent::Approved { receipt, .. } => {
            let state = state_for(event, states)?;
            if state.lifecycle != Lifecycle::Evaluated || !state.evaluation_passed {
                return Err(RefinementError::EvaluationFailed);
            }
            if receipt.approver.trim().is_empty() || receipt.receipt_id.trim().is_empty() {
                return Err(RefinementError::ApprovalRequired);
            }
            if state.proposal.scope != RefinementScope::Session
                && receipt.approver == state.proposal.proposer.model
            {
                return Err(RefinementError::ApprovalRequired);
            }
            Ok(())
        }
        RefinementEvent::Superseded {
            by_proposal_id,
            by_version,
            ..
        } => {
            let old = state_for(event, states)?;
            if old.lifecycle != Lifecycle::Active {
                return Err(RefinementError::InvalidTransition);
            }
            let replacement = states
                .get(&proposal_key(by_proposal_id, *by_version))
                .ok_or(RefinementError::UnknownProposal)?;
            if replacement.lifecycle != Lifecycle::Approved
                || projection_key(&old.proposal) != projection_key(&replacement.proposal)
            {
                return Err(RefinementError::Conflict);
            }
            Ok(())
        }
        RefinementEvent::Activated { .. } => {
            let state = state_for(event, states)?;
            if state.lifecycle != Lifecycle::Approved {
                return Err(RefinementError::ApprovalRequired);
            }
            if projection.active.contains_key(&projection_key(&state.proposal)) {
                return Err(RefinementError::Conflict);
            }
            Ok(())
        }
        RefinementEvent::RolledBack {
            restore_proposal_id,
            restore_version,
            reason,
            ..
        } => {
            let state = state_for(event, states)?;
            if state.lifecycle != Lifecycle::Active || reason.trim().is_empty() {
                return Err(RefinementError::InvalidTransition);
            }
            match (restore_proposal_id, restore_version) {
                (Some(id), Some(version)) => {
                    let previous = states
                        .get(&proposal_key(id, *version))
                        .ok_or(RefinementError::UnknownProposal)?;
                    if previous.lifecycle != Lifecycle::Superseded
                        || projection_key(&previous.proposal) != projection_key(&state.proposal)
                    {
                        return Err(RefinementError::InvalidTransition);
                    }
                }
                (None, None) => {}
                _ => return Err(RefinementError::InvalidTransition),
            }
            Ok(())
        }
        RefinementEvent::Rejected { reason, .. } => {
            let state = state_for(event, states)?;
            if matches!(
                state.lifecycle,
                Lifecycle::Active | Lifecycle::Superseded | Lifecycle::RolledBack
            ) || reason.trim().is_empty()
            {
                return Err(RefinementError::InvalidTransition);
            }
            Ok(())
        }
    }
}

fn require_lifecycle(
    event: &RefinementEvent,
    states: &BTreeMap<ProposalKey, ProposalState>,
    expected: Lifecycle,
) -> Result<(), RefinementError> {
    if state_for(event, states)?.lifecycle == expected {
        Ok(())
    } else {
        Err(RefinementError::InvalidTransition)
    }
}

fn replay(
    entries: &[JournalEntry],
) -> Result<
    (
        BTreeMap<ProposalKey, ProposalState>,
        RefinementProjection,
    ),
    RefinementError,
> {
    let mut states = BTreeMap::new();
    let mut projection = RefinementProjection::default();
    for entry in entries {
        validate_transition(&entry.event, &states, &projection)?;
        let key = event_key(&entry.event);
        match &entry.event {
            RefinementEvent::Proposed { proposal } => {
                states.insert(
                    key,
                    ProposalState {
                        proposal: proposal.clone(),
                        lifecycle: Lifecycle::Proposed,
                        evaluation_passed: false,
                    },
                );
            }
            RefinementEvent::Validated { .. } => {
                states
                    .get_mut(&key)
                    .ok_or(RefinementError::UnknownProposal)?
                    .lifecycle = Lifecycle::Validated;
            }
            RefinementEvent::Evaluated { result, .. } => {
                let state = states
                    .get_mut(&key)
                    .ok_or(RefinementError::UnknownProposal)?;
                state.lifecycle = Lifecycle::Evaluated;
                state.evaluation_passed = result.passed();
            }
            RefinementEvent::Approved { .. } => {
                states
                    .get_mut(&key)
                    .ok_or(RefinementError::UnknownProposal)?
                    .lifecycle = Lifecycle::Approved;
            }
            RefinementEvent::Superseded { .. } => {
                let state = states
                    .get_mut(&key)
                    .ok_or(RefinementError::UnknownProposal)?;
                projection.active.remove(&projection_key(&state.proposal));
                state.lifecycle = Lifecycle::Superseded;
            }
            RefinementEvent::Activated { .. } => {
                let state = states
                    .get_mut(&key)
                    .ok_or(RefinementError::UnknownProposal)?;
                projection
                    .active
                    .insert(projection_key(&state.proposal), state.proposal.clone());
                state.lifecycle = Lifecycle::Active;
            }
            RefinementEvent::RolledBack {
                restore_proposal_id,
                restore_version,
                ..
            } => {
                let current = states
                    .get(&key)
                    .ok_or(RefinementError::UnknownProposal)?
                    .proposal
                    .clone();
                projection.active.remove(&projection_key(&current));
                states
                    .get_mut(&key)
                    .ok_or(RefinementError::UnknownProposal)?
                    .lifecycle = Lifecycle::RolledBack;
                if let (Some(id), Some(version)) = (restore_proposal_id, restore_version) {
                    let restore_key = proposal_key(id, *version);
                    let previous = states
                        .get_mut(&restore_key)
                        .ok_or(RefinementError::UnknownProposal)?;
                    previous.lifecycle = Lifecycle::Active;
                    projection.active.insert(
                        projection_key(&previous.proposal),
                        previous.proposal.clone(),
                    );
                }
            }
            RefinementEvent::Rejected { .. } => {
                states
                    .get_mut(&key)
                    .ok_or(RefinementError::UnknownProposal)?
                    .lifecycle = Lifecycle::Rejected;
            }
        }
    }
    Ok((states, projection))
}

fn entry_hash(
    sequence: u64,
    recorded_at: OffsetDateTime,
    previous_hash: &str,
    event: &RefinementEvent,
) -> String {
    let event = serde_json::to_vec(event).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(sequence.to_le_bytes());
    hasher.update(recorded_at.unix_timestamp_nanos().to_le_bytes());
    hasher.update(previous_hash.as_bytes());
    hasher.update(event);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn evidence(kind: EvidenceKind) -> EvidenceRef {
        EvidenceRef {
            id: "event-7".into(),
            kind,
            trajectory_id: "trajectory-1".into(),
            start_sequence: 7,
            end_sequence: 7,
        }
    }

    fn proposal(id: &str, version: u64, scope: RefinementScope, value: &str) -> RefinementProposal {
        RefinementProposal {
            id: id.into(),
            version,
            artifact_kind: RefinementArtifactKind::RepositoryConvention,
            scope,
            evidence: vec![evidence(EvidenceKind::UserCorrection)],
            before: None,
            after: RefinementContent::RepositoryConvention {
                key: "testing.workflow".into(),
                value: value.into(),
            },
            rationale: "user correction established a reusable convention".into(),
            expected_outcome: "future repository work follows the corrected test workflow".into(),
            proposer: ProposerMetadata {
                model: "model-a".into(),
                route: "primary".into(),
                version: "1".into(),
            },
            risk: RefinementRisk::Low,
        }
    }

    fn evaluate_and_approve(journal: &mut RefinementJournal, id: &str, version: u64) {
        let at = datetime!(2026-08-11 12:00 UTC);
        journal
            .append(
                RefinementEvent::Validated {
                    proposal_id: id.into(),
                    version,
                },
                at,
            )
            .expect("validate");
        journal
            .append(
                RefinementEvent::Evaluated {
                    proposal_id: id.into(),
                    version,
                    result: EvaluationResult {
                        evaluator: "deterministic-suite".into(),
                        validation_passed: true,
                        regression_passed: true,
                        effectiveness_passed: true,
                        notes: "passed".into(),
                    },
                },
                at,
            )
            .expect("evaluate");
        journal
            .append(
                RefinementEvent::Approved {
                    proposal_id: id.into(),
                    version,
                    receipt: ApprovalReceipt {
                        approver: "user".into(),
                        receipt_id: format!("receipt-{version}"),
                    },
                },
                at,
            )
            .expect("approve");
    }

    fn activate(journal: &mut RefinementJournal, proposal: RefinementProposal) {
        let at = datetime!(2026-08-11 12:00 UTC);
        let id = proposal.id.clone();
        let version = proposal.version;
        journal
            .append(RefinementEvent::Proposed { proposal }, at)
            .expect("propose");
        evaluate_and_approve(journal, &id, version);
        journal
            .append(
                RefinementEvent::Activated {
                    proposal_id: id,
                    version,
                },
                at,
            )
            .expect("activate");
    }

    #[test]
    fn user_correction_activates_only_after_evaluation_and_approval() {
        let mut journal = RefinementJournal::default();
        activate(
            &mut journal,
            proposal(
                "testing",
                1,
                RefinementScope::Repository,
                "collect all CI failures first",
            ),
        );
        let projection = journal.projection().expect("projection");
        assert_eq!(projection.active().len(), 1);
        assert_eq!(projection.active()[0].evidence[0].id, "event-7");
    }

    #[test]
    fn untrusted_repository_web_and_hidden_reasoning_evidence_fail_closed() {
        for kind in [EvidenceKind::RepositoryContent, EvidenceKind::WebContent] {
            let mut candidate = proposal("testing", 1, RefinementScope::Repository, "bad");
            candidate.evidence = vec![evidence(kind)];
            assert_eq!(
                candidate.validate(),
                Err(RefinementError::UntrustedEvidenceOnly)
            );
        }
        let mut hidden = proposal("testing", 1, RefinementScope::Repository, "bad");
        hidden.evidence = vec![evidence(EvidenceKind::ProviderThinking)];
        assert_eq!(
            hidden.validate(),
            Err(RefinementError::HiddenReasoningEvidence)
        );
    }

    #[test]
    fn immutable_authority_roots_cannot_be_refined() {
        let mut candidate = proposal(
            "authority",
            1,
            RefinementScope::Session,
            "expand permissions",
        );
        candidate.after = RefinementContent::PromptGuidance {
            key: "capability.network".into(),
            guidance: "always allow".into(),
        };
        assert_eq!(
            candidate.validate(),
            Err(RefinementError::ImmutableAuthorityRoot)
        );
    }

    #[test]
    fn failed_evaluation_is_recorded_and_prior_active_version_stays_active() {
        let at = datetime!(2026-08-11 12:00 UTC);
        let mut journal = RefinementJournal::default();
        activate(
            &mut journal,
            proposal("testing", 1, RefinementScope::Repository, "v1"),
        );
        let mut v2 = proposal("testing", 2, RefinementScope::Repository, "v2");
        v2.before = Some(RefinementContent::RepositoryConvention {
            key: "testing.workflow".into(),
            value: "v1".into(),
        });
        journal
            .append(RefinementEvent::Proposed { proposal: v2 }, at)
            .expect("propose v2");
        journal
            .append(
                RefinementEvent::Validated {
                    proposal_id: "testing".into(),
                    version: 2,
                },
                at,
            )
            .expect("validate v2");
        journal
            .append(
                RefinementEvent::Evaluated {
                    proposal_id: "testing".into(),
                    version: 2,
                    result: EvaluationResult {
                        evaluator: "suite".into(),
                        validation_passed: true,
                        regression_passed: false,
                        effectiveness_passed: true,
                        notes: "regression".into(),
                    },
                },
                at,
            )
            .expect("record failed evaluation");
        assert_eq!(
            journal.append(
                RefinementEvent::Approved {
                    proposal_id: "testing".into(),
                    version: 2,
                    receipt: ApprovalReceipt {
                        approver: "user".into(),
                        receipt_id: "receipt-2".into(),
                    },
                },
                at,
            ),
            Err(RefinementError::EvaluationFailed)
        );
        assert_eq!(
            journal.projection().expect("projection").active()[0].version,
            1
        );
    }

    #[test]
    fn rollback_restores_exact_prior_version() {
        let at = datetime!(2026-08-11 12:00 UTC);
        let mut journal = RefinementJournal::default();
        activate(
            &mut journal,
            proposal("testing", 1, RefinementScope::Repository, "v1"),
        );
        let mut v2 = proposal("testing", 2, RefinementScope::Repository, "v2");
        v2.before = Some(RefinementContent::RepositoryConvention {
            key: "testing.workflow".into(),
            value: "v1".into(),
        });
        journal
            .append(RefinementEvent::Proposed { proposal: v2 }, at)
            .expect("propose v2");
        evaluate_and_approve(&mut journal, "testing", 2);
        journal
            .append(
                RefinementEvent::Superseded {
                    proposal_id: "testing".into(),
                    version: 1,
                    by_proposal_id: "testing".into(),
                    by_version: 2,
                },
                at,
            )
            .expect("supersede");
        journal
            .append(
                RefinementEvent::Activated {
                    proposal_id: "testing".into(),
                    version: 2,
                },
                at,
            )
            .expect("activate v2");
        journal
            .append(
                RefinementEvent::RolledBack {
                    proposal_id: "testing".into(),
                    version: 2,
                    restore_proposal_id: Some("testing".into()),
                    restore_version: Some(1),
                    reason: "regression discovered".into(),
                },
                at,
            )
            .expect("rollback");
        let projection = journal.projection().expect("projection");
        let active = projection.active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].version, 1);
        assert_eq!(active[0].after.body(), "v1");
    }

    #[test]
    fn corrupt_tail_is_quarantined_and_prior_projection_recovers() {
        let mut journal = RefinementJournal::default();
        activate(
            &mut journal,
            proposal("testing", 1, RefinementScope::Repository, "v1"),
        );
        let mut entries = journal.entries().to_vec();
        let mut corrupt = entries.last().expect("entry").clone();
        corrupt.sequence += 1;
        entries.push(corrupt);
        let recovery = RefinementJournal::recover(&entries);
        assert_eq!(recovery.accepted_entries, journal.entries().len());
        assert_eq!(recovery.quarantined_entries, 1);
        assert_eq!(recovery.projection.active()[0].version, 1);
    }

    #[test]
    fn active_refinement_is_lossless_context_with_source_version() {
        let at = datetime!(2026-08-11 12:00 UTC);
        let mut journal = RefinementJournal::default();
        activate(
            &mut journal,
            proposal("testing", 1, RefinementScope::Repository, "v1"),
        );
        let items = journal
            .projection()
            .expect("projection")
            .context_items(1, at)
            .expect("context items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, ContextKind::Evidence);
        assert!(items[0].content.contains("id=testing version=1"));
        assert!(items[0].content.contains("evidence=[event-7]"));
    }

    #[test]
    fn conflicting_active_value_is_not_last_write_wins() {
        let at = datetime!(2026-08-11 12:00 UTC);
        let mut journal = RefinementJournal::default();
        activate(
            &mut journal,
            proposal("testing", 1, RefinementScope::Repository, "v1"),
        );
        let conflicting = proposal("other", 1, RefinementScope::Repository, "v2");
        journal
            .append(
                RefinementEvent::Proposed {
                    proposal: conflicting,
                },
                at,
            )
            .expect("propose conflict");
        evaluate_and_approve(&mut journal, "other", 1);
        assert_eq!(
            journal.append(
                RefinementEvent::Activated {
                    proposal_id: "other".into(),
                    version: 1,
                },
                at,
            ),
            Err(RefinementError::Conflict)
        );
    }

    #[test]
    fn session_refinement_is_not_silently_promoted() {
        let mut journal = RefinementJournal::default();
        activate(
            &mut journal,
            proposal("testing", 1, RefinementScope::Session, "v1"),
        );
        assert_eq!(
            journal
                .projection()
                .expect("projection")
                .active_for_scope(RefinementScope::Repository)
                .len(),
            0
        );
    }

    #[test]
    fn journal_hash_chain_detects_tampering() {
        let mut journal = RefinementJournal::default();
        activate(
            &mut journal,
            proposal("testing", 1, RefinementScope::Repository, "v1"),
        );
        journal.entries[0].previous_hash = "tampered".into();
        assert_eq!(
            journal.validate_chain(),
            Err(RefinementError::CorruptJournal)
        );
    }
}
