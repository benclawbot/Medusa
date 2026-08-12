//! Durable canonical authority for proposed and production-active refinements.
//!
//! The journal in `medusa-context` remains a pure lifecycle engine. This module owns the durable
//! repository boundary, proposal-bound approval receipts, rebuildable projection, and deterministic
//! scope selection used by runtime callers.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use medusa_context::refinement::{
    ApprovalAuthority, ApprovalReceipt, EvaluationResult, RefinementArtifactKind, RefinementEvent,
    RefinementJournal, RefinementLifecycle, RefinementProposal, RefinementRecord,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::OffsetDateTime;

use crate::{
    refinement_persistence::{
        atomic_write, quarantine_bytes, read_optional, remove_file_if_present,
    },
    scoped_memory::RepositoryIdentity,
};

const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalActorClass {
    User,
    Reviewer,
    System,
    Runtime,
}

impl ApprovalActorClass {
    fn approver(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Reviewer => "reviewer",
            Self::System => "system",
            Self::Runtime => "runtime",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApprovalBinding {
    pub proposal_id: String,
    pub proposal_version: u64,
    pub proposal_digest: String,
    pub actor_class: ApprovalActorClass,
    pub decision_id: String,
    pub issued_at_unix_ms: i64,
    pub receipt_digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelectionContext {
    pub repository: Option<RepositoryIdentity>,
    pub user_id: String,
    pub session_id: Option<String>,
    pub task_kind: Option<String>,
    pub artifact_kind: Option<String>,
    pub context_tags: BTreeSet<String>,
    pub explicit_exclusions: BTreeSet<String>,
    pub objective: String,
    pub now_unix_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelectedRefinement {
    pub proposal: RefinementProposal,
    pub evidence_ids: Vec<String>,
    pub approval_receipt_id: String,
    pub selection_rationale: String,
    pub journal_head_hash: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelectionResult {
    pub selected: Vec<SelectedRefinement>,
    pub blocked_conflicts: Vec<String>,
    pub excluded_ids: Vec<String>,
    pub journal_head_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RefinementAuthoritySnapshot {
    pub schema_version: u32,
    pub revision: u64,
    pub journal_head_hash: String,
    pub active: Vec<RefinementProposal>,
    pub records: Vec<RefinementRecord>,
    pub conflict_keys: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct MigrationStatus {
    pub imported_sources: Vec<String>,
    pub quarantined_sources: Vec<String>,
}

#[derive(Debug, Error)]
pub enum RefinementAuthorityError {
    #[error("refinement authority I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("refinement authority serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("refinement lifecycle rejected the operation: {0}")]
    Lifecycle(#[from] medusa_context::refinement::RefinementError),
    #[error("refinement authority revision conflict: expected {expected}, actual {actual}")]
    Conflict { expected: u64, actual: u64 },
    #[error("refinement proposal was not found: {proposal_id}:{version}")]
    NotFound { proposal_id: String, version: u64 },
    #[error("refinement authority is corrupt at {path}: {reason}")]
    CorruptAuthority { path: String, reason: String },
    #[error("refinement projection could not be published: {reason}")]
    ProjectionFailure { reason: String },
    #[error("refinement authority validation failed: {0}")]
    Validation(String),
    #[error("approval receipt is required for proposal {proposal_id}:{version}")]
    ApprovalRequired { proposal_id: String, version: u64 },
}

type ApprovalKey = (String, u64, String);

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ApprovalDocument {
    schema_version: u32,
    bindings: Vec<ApprovalBinding>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ProjectionDocument {
    schema_version: u32,
    revision: u64,
    journal_head_hash: String,
    active: Vec<RefinementProposal>,
    records: Vec<RefinementRecord>,
    conflict_keys: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TransactionDocument {
    schema_version: u32,
    base_revision: u64,
    target_revision: u64,
    target_head_hash: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ExportDocument {
    schema_version: u32,
    snapshot: RefinementAuthoritySnapshot,
    journal: RefinementJournal,
    approvals: Vec<ApprovalBinding>,
}

#[derive(Clone, Debug)]
pub struct RefinementAuthorityStore {
    root: PathBuf,
    journal: RefinementJournal,
    approvals: BTreeMap<ApprovalKey, ApprovalBinding>,
}

impl RefinementAuthorityStore {
    pub fn open(repo: &Path) -> Result<Self, RefinementAuthorityError> {
        let root = repo.join(".medusa/refinement-authority");
        fs::create_dir_all(&root)?;
        let journal_path = root.join("journal.json");
        let journal_bytes = read_optional(&journal_path)?;
        let mut journal = match journal_bytes.as_deref() {
            None => RefinementJournal::default(),
            Some(bytes) => serde_json::from_slice(bytes)
                .map_err(|error| quarantine_corrupt(&root, "journal", bytes, error.to_string()))?,
        };
        let approvals = load_approvals(&root)?;
        let authority = DurableApprovalAuthority {
            approvals: &approvals,
        };
        journal.revalidate_approvals(&authority).map_err(|error| {
            authority_corrupt(
                &root,
                &journal_path,
                format!("approval revalidation failed: {error}"),
            )
        })?;
        let store = Self {
            root,
            journal,
            approvals,
        };
        store.reconcile_projection()?;
        remove_file_if_present(&store.transaction_path())?;
        Ok(store)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn snapshot(&self) -> Result<RefinementAuthoritySnapshot, RefinementAuthorityError> {
        snapshot_from_journal(&self.journal)
    }

    pub fn propose(
        &mut self,
        proposal: RefinementProposal,
        expected_revision: u64,
    ) -> Result<RefinementAuthoritySnapshot, RefinementAuthorityError> {
        self.commit_event(
            expected_revision,
            RefinementEvent::Proposed { proposal },
            None,
        )
    }

    pub fn validate(
        &mut self,
        proposal_id: &str,
        version: u64,
        expected_revision: u64,
    ) -> Result<RefinementAuthoritySnapshot, RefinementAuthorityError> {
        self.commit_event(
            expected_revision,
            RefinementEvent::Validated {
                proposal_id: proposal_id.to_owned(),
                version,
            },
            None,
        )
    }

    pub fn record_evaluation(
        &mut self,
        proposal_id: &str,
        version: u64,
        result: EvaluationResult,
        expected_revision: u64,
    ) -> Result<RefinementAuthoritySnapshot, RefinementAuthorityError> {
        self.commit_event(
            expected_revision,
            RefinementEvent::Evaluated {
                proposal_id: proposal_id.to_owned(),
                version,
                result,
            },
            None,
        )
    }

    pub fn approve(
        &mut self,
        proposal_id: &str,
        version: u64,
        actor_class: ApprovalActorClass,
        decision_id: &str,
        issued_at_unix_ms: i64,
        expected_revision: u64,
    ) -> Result<RefinementAuthoritySnapshot, RefinementAuthorityError> {
        if decision_id.trim().is_empty() {
            return Err(RefinementAuthorityError::Validation(
                "approval decision ID cannot be empty".into(),
            ));
        }
        let proposal = self.find_proposal(proposal_id, version)?;
        let receipt = ApprovalReceipt {
            approver: actor_class.approver().into(),
            receipt_id: decision_id.into(),
        };
        let binding = ApprovalBinding {
            proposal_id: proposal.id.clone(),
            proposal_version: proposal.version,
            proposal_digest: proposal_digest(&proposal)?,
            actor_class,
            decision_id: decision_id.into(),
            issued_at_unix_ms,
            receipt_digest: receipt_digest(&receipt)?,
        };
        self.commit_event(
            expected_revision,
            RefinementEvent::Approved {
                proposal_id: proposal_id.into(),
                version,
                receipt,
            },
            Some(binding),
        )
    }

    pub fn activate(
        &mut self,
        proposal_id: &str,
        version: u64,
        expected_revision: u64,
    ) -> Result<RefinementAuthoritySnapshot, RefinementAuthorityError> {
        self.commit_event(
            expected_revision,
            RefinementEvent::Activated {
                proposal_id: proposal_id.into(),
                version,
            },
            None,
        )
    }

    pub fn supersede(
        &mut self,
        proposal_id: &str,
        version: u64,
        by_proposal_id: &str,
        by_version: u64,
        expected_revision: u64,
    ) -> Result<RefinementAuthoritySnapshot, RefinementAuthorityError> {
        self.commit_event(
            expected_revision,
            RefinementEvent::Superseded {
                proposal_id: proposal_id.into(),
                version,
                by_proposal_id: by_proposal_id.into(),
                by_version,
            },
            None,
        )
    }

    pub fn defer(
        &mut self,
        proposal_id: &str,
        version: u64,
        reason: &str,
        expected_revision: u64,
    ) -> Result<RefinementAuthoritySnapshot, RefinementAuthorityError> {
        self.commit_event(
            expected_revision,
            RefinementEvent::Deferred {
                proposal_id: proposal_id.into(),
                version,
                reason: reason.into(),
            },
            None,
        )
    }

    pub fn suspend(
        &mut self,
        proposal_id: &str,
        version: u64,
        reason: &str,
        expected_revision: u64,
    ) -> Result<RefinementAuthoritySnapshot, RefinementAuthorityError> {
        self.commit_event(
            expected_revision,
            RefinementEvent::Suspended {
                proposal_id: proposal_id.into(),
                version,
                reason: reason.into(),
            },
            None,
        )
    }

    pub fn rollback(
        &mut self,
        proposal_id: &str,
        version: u64,
        restore_proposal_id: Option<&str>,
        restore_version: Option<u64>,
        reason: &str,
        expected_revision: u64,
    ) -> Result<RefinementAuthoritySnapshot, RefinementAuthorityError> {
        self.commit_event(
            expected_revision,
            RefinementEvent::RolledBack {
                proposal_id: proposal_id.into(),
                version,
                restore_proposal_id: restore_proposal_id.map(str::to_owned),
                restore_version,
                reason: reason.into(),
            },
            None,
        )
    }

    pub fn reject(
        &mut self,
        proposal_id: &str,
        version: u64,
        reason: &str,
        expected_revision: u64,
    ) -> Result<RefinementAuthoritySnapshot, RefinementAuthorityError> {
        self.commit_event(
            expected_revision,
            RefinementEvent::Rejected {
                proposal_id: proposal_id.into(),
                version,
                reason: reason.into(),
            },
            None,
        )
    }

    pub fn tombstone(
        &mut self,
        proposal_id: &str,
        version: u64,
        reason: &str,
        expected_revision: u64,
    ) -> Result<RefinementAuthoritySnapshot, RefinementAuthorityError> {
        self.commit_event(
            expected_revision,
            RefinementEvent::Tombstoned {
                proposal_id: proposal_id.into(),
                version,
                reason: reason.into(),
            },
            None,
        )
    }

    pub fn select(
        &self,
        context: &SelectionContext,
    ) -> Result<SelectionResult, RefinementAuthorityError> {
        let snapshot = self.snapshot()?;
        let projection = self.journal.projection()?;
        let mut result = SelectionResult {
            blocked_conflicts: snapshot.conflict_keys.clone(),
            journal_head_hash: snapshot.journal_head_hash.clone(),
            ..SelectionResult::default()
        };
        let records = snapshot.records.iter().filter_map(|record| {
            (record.lifecycle == RefinementLifecycle::Active).then_some(record)
        });
        for record in records {
            let Some(proposal) = record.proposal.as_ref() else {
                continue;
            };
            if context.explicit_exclusions.contains(&record.proposal_id)
                || context
                    .explicit_exclusions
                    .contains(content_identity(proposal))
            {
                result.excluded_ids.push(record.proposal_id.clone());
                continue;
            }
            if !scope_matches(proposal, context)
                || !artifact_matches(proposal.artifact_kind, context.artifact_kind.as_deref())
                || !objective_matches(proposal, &context.objective)
            {
                result.excluded_ids.push(record.proposal_id.clone());
                continue;
            }
            let Some(receipt_id) = record.approval_receipt_id.clone() else {
                continue;
            };
            result.selected.push(SelectedRefinement {
                evidence_ids: proposal
                    .evidence
                    .iter()
                    .map(|item| item.id.clone())
                    .collect(),
                approval_receipt_id: receipt_id,
                selection_rationale: "scope, artifact, objective, and conflict checks passed"
                    .into(),
                journal_head_hash: snapshot.journal_head_hash.clone(),
                proposal: proposal.clone(),
            });
        }
        result.selected.sort_by(|left, right| {
            content_identity(&left.proposal)
                .cmp(content_identity(&right.proposal))
                .then_with(|| left.proposal.id.cmp(&right.proposal.id))
        });
        let _ = projection;
        Ok(result)
    }

    pub fn export(&self) -> Result<Vec<u8>, RefinementAuthorityError> {
        let document = ExportDocument {
            schema_version: SCHEMA_VERSION,
            snapshot: self.snapshot()?,
            journal: self.journal.clone(),
            approvals: self.approvals.values().cloned().collect(),
        };
        Ok(serde_json::to_vec_pretty(&document)?)
    }

    pub fn migration_status(&self) -> Result<MigrationStatus, RefinementAuthorityError> {
        let path = self.root.join("migrations.jsonl");
        let Some(bytes) = read_optional(&path)? else {
            return Ok(MigrationStatus::default());
        };
        let text = String::from_utf8(bytes)
            .map_err(|error| RefinementAuthorityError::Validation(error.to_string()))?;
        let mut status = MigrationStatus::default();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let receipt: MigrationLine = serde_json::from_str(line)?;
            match receipt.disposition.as_str() {
                "quarantined" => status.quarantined_sources.push(receipt.source),
                _ => status.imported_sources.push(receipt.source),
            }
        }
        status.imported_sources.sort();
        status.quarantined_sources.sort();
        Ok(status)
    }

    fn commit_event(
        &mut self,
        expected_revision: u64,
        event: RefinementEvent,
        binding: Option<ApprovalBinding>,
    ) -> Result<RefinementAuthoritySnapshot, RefinementAuthorityError> {
        let actual_revision = self.journal.entries().len() as u64;
        ensure_revision(actual_revision, expected_revision)?;
        let mut candidate_journal = self.journal.clone();
        let mut candidate_approvals = self.approvals.clone();
        if let Some(binding) = binding {
            candidate_approvals.insert(
                (
                    binding.proposal_id.clone(),
                    binding.proposal_version,
                    binding.decision_id.clone(),
                ),
                binding,
            );
        }
        let authority = DurableApprovalAuthority {
            approvals: &candidate_approvals,
        };
        let recorded_at = OffsetDateTime::now_utc();
        match &event {
            RefinementEvent::Approved {
                proposal_id,
                version,
                receipt,
            } => candidate_journal.append_approved(
                proposal_id.clone(),
                *version,
                receipt.clone(),
                recorded_at,
                &authority,
            )?,
            _ => candidate_journal.append(event, recorded_at)?,
        }
        candidate_journal.revalidate_approvals(&authority)?;
        let candidate_snapshot = snapshot_from_journal(&candidate_journal)?;
        self.persist_candidate(
            &candidate_journal,
            &candidate_approvals,
            &candidate_snapshot,
            actual_revision,
        )?;
        self.journal = candidate_journal;
        self.approvals = candidate_approvals;
        Ok(candidate_snapshot)
    }

    fn persist_candidate(
        &self,
        journal: &RefinementJournal,
        approvals: &BTreeMap<ApprovalKey, ApprovalBinding>,
        snapshot: &RefinementAuthoritySnapshot,
        base_revision: u64,
    ) -> Result<(), RefinementAuthorityError> {
        let transaction = TransactionDocument {
            schema_version: SCHEMA_VERSION,
            base_revision,
            target_revision: snapshot.revision,
            target_head_hash: snapshot.journal_head_hash.clone(),
        };
        let transaction_path = self.transaction_path();
        atomic_write(&transaction_path, &serde_json::to_vec_pretty(&transaction)?)?;
        let projection = ProjectionDocument {
            schema_version: SCHEMA_VERSION,
            revision: snapshot.revision,
            journal_head_hash: snapshot.journal_head_hash.clone(),
            active: snapshot.active.clone(),
            records: snapshot.records.clone(),
            conflict_keys: snapshot.conflict_keys.clone(),
        };
        if let Err(error) = atomic_write(
            &self.projection_path(),
            &serde_json::to_vec_pretty(&projection)?,
        ) {
            let _ = remove_file_if_present(&transaction_path);
            return Err(RefinementAuthorityError::ProjectionFailure {
                reason: error.to_string(),
            });
        }
        let journal_result =
            atomic_write(&self.journal_path(), &serde_json::to_vec_pretty(journal)?);
        if let Err(error) = journal_result {
            let _ = remove_file_if_present(&transaction_path);
            return Err(RefinementAuthorityError::Io(error));
        }
        let approval_document = ApprovalDocument {
            schema_version: SCHEMA_VERSION,
            bindings: approvals.values().cloned().collect(),
        };
        if let Err(error) = atomic_write(
            &self.approvals_path(),
            &serde_json::to_vec_pretty(&approval_document)?,
        ) {
            let _ = remove_file_if_present(&transaction_path);
            return Err(RefinementAuthorityError::Io(error));
        }
        let _ = remove_file_if_present(&transaction_path);
        Ok(())
    }

    fn reconcile_projection(&self) -> Result<(), RefinementAuthorityError> {
        let snapshot = self.snapshot()?;
        let path = self.projection_path();
        let valid = read_optional(&path)?
            .and_then(|bytes| serde_json::from_slice::<ProjectionDocument>(&bytes).ok())
            .is_some_and(|projection| {
                projection.schema_version == SCHEMA_VERSION
                    && projection.revision == snapshot.revision
                    && projection.journal_head_hash == snapshot.journal_head_hash
                    && projection.active == snapshot.active
                    && projection.records == snapshot.records
                    && projection.conflict_keys == snapshot.conflict_keys
            });
        if valid {
            return Ok(());
        }
        let projection = ProjectionDocument {
            schema_version: SCHEMA_VERSION,
            revision: snapshot.revision,
            journal_head_hash: snapshot.journal_head_hash.clone(),
            active: snapshot.active.clone(),
            records: snapshot.records.clone(),
            conflict_keys: snapshot.conflict_keys.clone(),
        };
        atomic_write(&path, &serde_json::to_vec_pretty(&projection)?).map_err(|error| {
            RefinementAuthorityError::ProjectionFailure {
                reason: error.to_string(),
            }
        })
    }

    fn find_proposal(
        &self,
        proposal_id: &str,
        version: u64,
    ) -> Result<RefinementProposal, RefinementAuthorityError> {
        self.journal
            .entries()
            .iter()
            .find_map(|entry| match &entry.event {
                RefinementEvent::Proposed { proposal }
                    if proposal.id == proposal_id && proposal.version == version =>
                {
                    Some(proposal.clone())
                }
                _ => None,
            })
            .ok_or_else(|| RefinementAuthorityError::NotFound {
                proposal_id: proposal_id.into(),
                version,
            })
    }

    fn journal_path(&self) -> PathBuf {
        self.root.join("journal.json")
    }

    fn approvals_path(&self) -> PathBuf {
        self.root.join("approvals.json")
    }

    fn projection_path(&self) -> PathBuf {
        self.root.join("active.json")
    }

    fn transaction_path(&self) -> PathBuf {
        self.root.join("transactions/active.json")
    }
}

#[derive(Clone, Debug, Deserialize)]
struct MigrationLine {
    source: String,
    disposition: String,
}

struct DurableApprovalAuthority<'a> {
    approvals: &'a BTreeMap<ApprovalKey, ApprovalBinding>,
}

impl ApprovalAuthority for DurableApprovalAuthority<'_> {
    fn authorizes(&self, proposal: &RefinementProposal, receipt: &ApprovalReceipt) -> bool {
        let key = (
            proposal.id.clone(),
            proposal.version,
            receipt.receipt_id.clone(),
        );
        let Some(binding) = self.approvals.get(&key) else {
            return false;
        };
        binding.proposal_digest == proposal_digest(proposal).unwrap_or_default()
            && binding.receipt_digest == receipt_digest(receipt).unwrap_or_default()
            && binding.actor_class.approver() == receipt.approver
    }
}

fn load_approvals(
    root: &Path,
) -> Result<BTreeMap<ApprovalKey, ApprovalBinding>, RefinementAuthorityError> {
    let path = root.join("approvals.json");
    let Some(bytes) = read_optional(&path)? else {
        return Ok(BTreeMap::new());
    };
    let document: ApprovalDocument = serde_json::from_slice(&bytes)
        .map_err(|error| quarantine_corrupt(root, "approvals", &bytes, error.to_string()))?;
    if document.schema_version != SCHEMA_VERSION {
        return Err(authority_corrupt(
            root,
            &path,
            format!("unsupported approval schema {}", document.schema_version),
        ));
    }
    let mut approvals = BTreeMap::new();
    for binding in document.bindings {
        approvals.insert(
            (
                binding.proposal_id.clone(),
                binding.proposal_version,
                binding.decision_id.clone(),
            ),
            binding,
        );
    }
    Ok(approvals)
}

fn snapshot_from_journal(
    journal: &RefinementJournal,
) -> Result<RefinementAuthoritySnapshot, RefinementAuthorityError> {
    let projection = journal.projection()?;
    Ok(RefinementAuthoritySnapshot {
        schema_version: SCHEMA_VERSION,
        revision: journal.entries().len() as u64,
        journal_head_hash: projection.head_hash().to_owned(),
        active: projection.active().into_iter().cloned().collect(),
        records: projection.records().to_vec(),
        conflict_keys: projection.conflict_keys(),
    })
}

fn ensure_revision(actual: u64, expected: u64) -> Result<(), RefinementAuthorityError> {
    if actual == expected {
        Ok(())
    } else {
        Err(RefinementAuthorityError::Conflict { expected, actual })
    }
}

fn scope_matches(proposal: &RefinementProposal, context: &SelectionContext) -> bool {
    match proposal.scope {
        medusa_context::refinement::RefinementScope::Session => context.session_id.is_some(),
        medusa_context::refinement::RefinementScope::Repository => context.repository.is_some(),
        medusa_context::refinement::RefinementScope::User => !context.user_id.trim().is_empty(),
    }
}

fn artifact_matches(kind: RefinementArtifactKind, requested: Option<&str>) -> bool {
    requested.is_none_or(|requested| artifact_kind_key(kind) == requested)
}

fn objective_matches(proposal: &RefinementProposal, objective: &str) -> bool {
    let objective = objective.trim().to_ascii_lowercase();
    if objective.is_empty() {
        return true;
    }
    objective.contains(&content_identity(proposal).to_ascii_lowercase())
        || content_body(proposal).is_some_and(|body| objective.contains(&body.to_ascii_lowercase()))
}

fn content_identity(proposal: &RefinementProposal) -> &str {
    match &proposal.after {
        medusa_context::refinement::RefinementContent::Memory { key, .. }
        | medusa_context::refinement::RefinementContent::RepositoryConvention { key, .. }
        | medusa_context::refinement::RefinementContent::PromptGuidance { key, .. } => key,
        medusa_context::refinement::RefinementContent::WorkflowMetadata { name, .. }
        | medusa_context::refinement::RefinementContent::TeamRoleMetadata { name, .. } => name,
    }
}

fn content_body(proposal: &RefinementProposal) -> Option<&str> {
    Some(match &proposal.after {
        medusa_context::refinement::RefinementContent::Memory { value, .. }
        | medusa_context::refinement::RefinementContent::RepositoryConvention { value, .. } => {
            value
        }
        medusa_context::refinement::RefinementContent::WorkflowMetadata { summary, .. } => summary,
        medusa_context::refinement::RefinementContent::TeamRoleMetadata { guidance, .. }
        | medusa_context::refinement::RefinementContent::PromptGuidance { guidance, .. } => {
            guidance
        }
    })
}

fn artifact_kind_key(kind: RefinementArtifactKind) -> &'static str {
    match kind {
        RefinementArtifactKind::Memory => "memory",
        RefinementArtifactKind::RepositoryConvention => "repository_convention",
        RefinementArtifactKind::WorkflowMetadata => "workflow_metadata",
        RefinementArtifactKind::TeamRoleMetadata => "team_role_metadata",
        RefinementArtifactKind::PromptGuidance => "prompt_guidance",
    }
}

fn proposal_digest(proposal: &RefinementProposal) -> Result<String, serde_json::Error> {
    Ok(crate::encode(Sha256::digest(serde_json::to_vec(proposal)?)))
}

fn receipt_digest(receipt: &ApprovalReceipt) -> Result<String, serde_json::Error> {
    Ok(crate::encode(Sha256::digest(serde_json::to_vec(receipt)?)))
}

fn quarantine_corrupt(
    root: &Path,
    label: &str,
    bytes: &[u8],
    reason: String,
) -> RefinementAuthorityError {
    let quarantine = quarantine_bytes(root, label, bytes)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| root.join("quarantine").display().to_string());
    RefinementAuthorityError::CorruptAuthority {
        path: quarantine,
        reason,
    }
}

fn authority_corrupt(root: &Path, path: &Path, reason: String) -> RefinementAuthorityError {
    let bytes = fs::read(path).unwrap_or_default();
    quarantine_corrupt(root, "authority", &bytes, reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objective_match_is_case_insensitive() {
        let proposal = RefinementProposal {
            id: "test".into(),
            version: 1,
            artifact_kind: RefinementArtifactKind::Memory,
            scope: medusa_context::refinement::RefinementScope::User,
            evidence: Vec::new(),
            before: None,
            after: medusa_context::refinement::RefinementContent::Memory {
                key: "workflow".into(),
                value: "Run Tests".into(),
            },
            rationale: String::new(),
            expected_outcome: String::new(),
            proposer: medusa_context::refinement::ProposerMetadata {
                model: String::new(),
                route: String::new(),
                version: String::new(),
            },
            risk: medusa_context::refinement::RefinementRisk::Low,
        };
        assert!(objective_matches(&proposal, "run tests now"));
    }
}
