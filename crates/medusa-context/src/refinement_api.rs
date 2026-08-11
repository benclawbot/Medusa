use std::collections::BTreeSet;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{refinement_core as core, ContextItem, ContextKind, ContextLedger};

pub use core::{
    ApprovalReceipt, EvaluationResult, EvidenceKind, EvidenceRef, ProposerMetadata,
    RefinementArtifactKind, RefinementContent, RefinementError, RefinementRisk, RefinementScope,
};

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
        let core: core::RefinementProposal = convert(self)?;
        core.validate()?;
        if self.artifact_kind != kind_for(&self.after)
            || self
                .before
                .as_ref()
                .is_some_and(|before| self.artifact_kind != kind_for(before))
        {
            return Err(RefinementError::InvalidProposal);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RefinementEvent {
    Proposed { proposal: RefinementProposal },
    Validated { proposal_id: String, version: u64 },
    Evaluated { proposal_id: String, version: u64, result: EvaluationResult },
    Approved { proposal_id: String, version: u64, receipt: ApprovalReceipt },
    Superseded {
        proposal_id: String,
        version: u64,
        by_proposal_id: String,
        by_version: u64,
    },
    Activated { proposal_id: String, version: u64 },
    RolledBack {
        proposal_id: String,
        version: u64,
        restore_proposal_id: Option<String>,
        restore_version: Option<u64>,
        reason: String,
    },
    Rejected { proposal_id: String, version: u64, reason: String },
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

pub trait ApprovalAuthority {
    fn authorizes(&self, proposal: &RefinementProposal, receipt: &ApprovalReceipt) -> bool;
}

type ApprovalKey = (String, u64, String);

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RefinementJournal {
    #[serde(default)]
    entries: Vec<JournalEntry>,
    #[serde(skip)]
    authorized_approvals: BTreeSet<ApprovalKey>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RefinementProjection {
    active: Vec<RefinementProposal>,
}

impl RefinementProjection {
    #[must_use]
    pub fn active(&self) -> Vec<&RefinementProposal> {
        self.active.iter().collect()
    }

    #[must_use]
    pub fn active_for_scope(&self, scope: RefinementScope) -> Vec<&RefinementProposal> {
        self.active.iter().filter(|proposal| proposal.scope == scope).collect()
    }

    #[must_use]
    pub fn conflicts(&self, proposal: &RefinementProposal) -> Vec<&RefinementProposal> {
        self.active
            .iter()
            .filter(|active| {
                active.artifact_kind == proposal.artifact_kind
                    && identity(&active.after) == identity(&proposal.after)
            })
            .collect()
    }

    pub fn context_items(
        &self,
        mut next_sequence: u64,
        recorded_at: OffsetDateTime,
    ) -> Result<Vec<ContextItem>, &'static str> {
        let mut items = Vec::new();
        for proposal in &self.active {
            items.push(context_item(proposal, next_sequence, recorded_at)?);
            next_sequence += 1;
        }
        Ok(items)
    }

    pub fn append_to_ledger(
        &self,
        ledger: &mut ContextLedger,
        recorded_at: OffsetDateTime,
    ) -> Result<usize, &'static str> {
        let mut appended = 0;
        for proposal in &self.active {
            let id = context_id(proposal);
            if ledger.items().iter().any(|item| item.id == id) {
                continue;
            }
            let sequence = ledger.items().len() as u64 + 1;
            ledger.append(context_item(proposal, sequence, recorded_at)?)?;
            appended += 1;
        }
        Ok(appended)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryResult {
    pub projection: RefinementProjection,
    pub accepted_entries: usize,
    pub quarantined_entries: usize,
}

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
        match &event {
            RefinementEvent::Proposed { proposal } => proposal.validate()?,
            RefinementEvent::Approved { .. } => return Err(RefinementError::ApprovalRequired),
            RefinementEvent::RolledBack { .. } => validate_direct_restore(&event, &self.entries)?,
            _ => {}
        }
        self.append_core(event, recorded_at)
    }

    pub fn append_approved<A: ApprovalAuthority>(
        &mut self,
        proposal_id: impl Into<String>,
        version: u64,
        receipt: ApprovalReceipt,
        recorded_at: OffsetDateTime,
        authority: &A,
    ) -> Result<(), RefinementError> {
        self.validate_chain()?;
        let proposal_id = proposal_id.into();
        let proposal = self.proposal(&proposal_id, version).ok_or(RefinementError::UnknownProposal)?;
        if !authority.authorizes(proposal, &receipt) {
            return Err(RefinementError::ApprovalRequired);
        }
        let key = approval_key(&proposal_id, version, &receipt);
        self.authorized_approvals.insert(key.clone());
        let result = self.append_core(
            RefinementEvent::Approved { proposal_id, version, receipt },
            recorded_at,
        );
        if result.is_err() {
            self.authorized_approvals.remove(&key);
        }
        result
    }

    pub fn revalidate_approvals<A: ApprovalAuthority>(
        &mut self,
        authority: &A,
    ) -> Result<(), RefinementError> {
        let mut authorized = BTreeSet::new();
        for entry in &self.entries {
            let RefinementEvent::Approved { proposal_id, version, receipt } = &entry.event else {
                continue;
            };
            let proposal = self.proposal(proposal_id, *version).ok_or(RefinementError::UnknownProposal)?;
            if !authority.authorizes(proposal, receipt) {
                return Err(RefinementError::ApprovalRequired);
            }
            authorized.insert(approval_key(proposal_id, *version, receipt));
        }
        self.authorized_approvals = authorized;
        self.validate_chain()
    }

    pub fn validate_chain(&self) -> Result<(), RefinementError> {
        for (index, entry) in self.entries.iter().enumerate() {
            match &entry.event {
                RefinementEvent::Proposed { proposal } => proposal.validate()?,
                RefinementEvent::Approved { proposal_id, version, receipt } => {
                    if !self.authorized_approvals.contains(&approval_key(proposal_id, *version, receipt)) {
                        return Err(RefinementError::ApprovalRequired);
                    }
                }
                RefinementEvent::RolledBack { .. } => {
                    validate_direct_restore(&entry.event, &self.entries[..index])?;
                }
                _ => {}
            }
        }
        self.to_core()?.validate_chain()
    }

    pub fn projection(&self) -> Result<RefinementProjection, RefinementError> {
        self.validate_chain()?;
        let core = self.to_core()?.projection()?;
        let active = core.active().into_iter().map(convert).collect::<Result<Vec<_>, _>>()?;
        Ok(RefinementProjection { active })
    }

    pub fn append_active_to_ledger(
        &self,
        ledger: &mut ContextLedger,
        recorded_at: OffsetDateTime,
    ) -> Result<usize, &'static str> {
        self.projection()
            .map_err(|_| "refinement journal validation failed")?
            .append_to_ledger(ledger, recorded_at)
    }

    #[must_use]
    pub fn recover(entries: &[JournalEntry]) -> RecoveryResult {
        recover(entries, None::<&DenyAll>)
    }

    #[must_use]
    pub fn recover_with_approval_authority<A: ApprovalAuthority>(
        entries: &[JournalEntry],
        authority: &A,
    ) -> RecoveryResult {
        recover(entries, Some(authority))
    }

    fn append_core(
        &mut self,
        event: RefinementEvent,
        recorded_at: OffsetDateTime,
    ) -> Result<(), RefinementError> {
        let mut core = self.to_core()?;
        core.append(convert(&event)?, recorded_at)?;
        self.entries = core.entries().iter().map(convert).collect::<Result<Vec<_>, _>>()?;
        Ok(())
    }

    fn to_core(&self) -> Result<core::RefinementJournal, RefinementError> {
        #[derive(Serialize)]
        struct Stored<'a> { entries: &'a [JournalEntry] }
        convert(&Stored { entries: &self.entries })
    }

    fn proposal(&self, id: &str, version: u64) -> Option<&RefinementProposal> {
        self.entries.iter().find_map(|entry| match &entry.event {
            RefinementEvent::Proposed { proposal } if proposal.id == id && proposal.version == version => Some(proposal),
            _ => None,
        })
    }
}

struct DenyAll;
impl ApprovalAuthority for DenyAll {
    fn authorizes(&self, _: &RefinementProposal, _: &ApprovalReceipt) -> bool { false }
}

fn recover<A: ApprovalAuthority>(entries: &[JournalEntry], authority: Option<&A>) -> RecoveryResult {
    let mut accepted = RefinementJournal::default();
    for entry in entries {
        let mut candidate = accepted.clone();
        candidate.entries.push(entry.clone());
        if let Some(authority) = authority {
            if candidate.revalidate_approvals(authority).is_err() { break; }
        }
        if candidate.validate_chain().is_err() { break; }
        accepted = candidate;
    }
    let projection = accepted.projection().unwrap_or_default();
    RecoveryResult {
        projection,
        accepted_entries: accepted.entries.len(),
        quarantined_entries: entries.len().saturating_sub(accepted.entries.len()),
    }
}

fn validate_direct_restore(event: &RefinementEvent, prior: &[JournalEntry]) -> Result<(), RefinementError> {
    let RefinementEvent::RolledBack {
        proposal_id,
        version,
        restore_proposal_id,
        restore_version,
        ..
    } = event else { return Ok(()); };
    match (restore_proposal_id, restore_version) {
        (Some(restore_id), Some(restore_version)) => {
            let successor = prior.iter().rev().find_map(|entry| match &entry.event {
                RefinementEvent::Superseded {
                    proposal_id,
                    version,
                    by_proposal_id,
                    by_version,
                } if proposal_id == restore_id && version == restore_version => Some((by_proposal_id.as_str(), *by_version)),
                _ => None,
            });
            if successor != Some((proposal_id.as_str(), *version)) {
                return Err(RefinementError::InvalidTransition);
            }
        }
        (None, None) => {}
        _ => return Err(RefinementError::InvalidTransition),
    }
    Ok(())
}

fn kind_for(content: &RefinementContent) -> RefinementArtifactKind {
    match content {
        RefinementContent::Memory { .. } => RefinementArtifactKind::Memory,
        RefinementContent::RepositoryConvention { .. } => RefinementArtifactKind::RepositoryConvention,
        RefinementContent::WorkflowMetadata { .. } => RefinementArtifactKind::WorkflowMetadata,
        RefinementContent::TeamRoleMetadata { .. } => RefinementArtifactKind::TeamRoleMetadata,
        RefinementContent::PromptGuidance { .. } => RefinementArtifactKind::PromptGuidance,
    }
}

fn identity(content: &RefinementContent) -> &str {
    match content {
        RefinementContent::Memory { key, .. }
        | RefinementContent::RepositoryConvention { key, .. }
        | RefinementContent::PromptGuidance { key, .. } => key,
        RefinementContent::WorkflowMetadata { name, .. }
        | RefinementContent::TeamRoleMetadata { name, .. } => name,
    }
}

fn body(content: &RefinementContent) -> &str {
    match content {
        RefinementContent::Memory { value, .. }
        | RefinementContent::RepositoryConvention { value, .. } => value,
        RefinementContent::WorkflowMetadata { summary, .. } => summary,
        RefinementContent::TeamRoleMetadata { guidance, .. }
        | RefinementContent::PromptGuidance { guidance, .. } => guidance,
    }
}

fn context_id(proposal: &RefinementProposal) -> String {
    format!("refinement:{}:{}", proposal.id, proposal.version)
}

fn context_item(
    proposal: &RefinementProposal,
    sequence: u64,
    recorded_at: OffsetDateTime,
) -> Result<ContextItem, &'static str> {
    let evidence = proposal.evidence.iter().map(|item| item.id.as_str()).collect::<Vec<_>>().join(",");
    ContextItem::new(
        context_id(proposal),
        ContextKind::Evidence,
        format!(
            "active_refinement id={} version={} scope={:?} kind={:?} key={} value={} evidence=[{}]",
            proposal.id,
            proposal.version,
            proposal.scope,
            proposal.artifact_kind,
            identity(&proposal.after),
            body(&proposal.after),
            evidence
        ),
        sequence,
        recorded_at,
    )
}

fn approval_key(id: &str, version: u64, receipt: &ApprovalReceipt) -> ApprovalKey {
    (id.to_owned(), version, receipt.receipt_id.clone())
}

fn convert<T: Serialize, U: DeserializeOwned>(value: &T) -> Result<U, RefinementError> {
    let value = serde_json::to_value(value).map_err(|_| RefinementError::CorruptJournal)?;
    serde_json::from_value(value).map_err(|_| RefinementError::CorruptJournal)
}
