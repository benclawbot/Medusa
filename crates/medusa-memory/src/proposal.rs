use std::path::PathBuf;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use ulid::Ulid;

use crate::{
    engine::MemoryEngine,
    schema::{MemoryDocument, MemoryProposal, Scope, Status},
    support::{atomic_write, deduplicate, internal, invalid, normalize, sanitize_component},
};

impl MemoryEngine {
    /// Validates an untrusted proposal before any canonical write.
    pub fn validate_proposal(&self, proposal: &MemoryProposal) -> MedusaResult<()> {
        if proposal.memory_type.trim().is_empty()
            || proposal.title.trim().is_empty()
            || proposal.claim.trim().is_empty()
        {
            return Err(invalid("memory type, title, and claim are required"));
        }
        if proposal.confidence_milli > 1_000 {
            return Err(invalid("confidence_milli must be at most 1000"));
        }
        if proposal.evidence.is_empty() {
            return Err(invalid("memory proposal requires provenance evidence"));
        }
        if proposal.scope == Scope::Project && proposal.project_id.is_none() {
            return Err(invalid("project memory requires project_id"));
        }
        let serialized = serde_json::to_string(proposal)?;
        if contains_secret(&serialized) {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                "memory proposal appears to contain a secret",
            ));
        }
        if !proposal.validation.high_confidence() && proposal.confidence_milli >= 800 {
            return Err(invalid(
                "unverified or inferred memory cannot claim high confidence",
            ));
        }
        Ok(())
    }

    /// Stores a proposal for review without modifying canonical memory.
    pub fn propose(&self, proposal: &MemoryProposal) -> MedusaResult<PathBuf> {
        self.validate_proposal(proposal)?;
        let path = self
            .root
            .join("proposals")
            .join(format!("proposal-{}.json", Ulid::new()));
        atomic_write(&path, &serde_json::to_vec_pretty(proposal)?)?;
        Ok(path)
    }

    /// Commits a validated proposal to canonical Markdown and refreshes the index.
    pub fn commit_proposal(&self, proposal: &MemoryProposal) -> MedusaResult<MemoryDocument> {
        self.validate_proposal(proposal)?;
        let duplicate = self.documents()?.into_iter().find(|(_, document)| {
            document.status == Status::Active
                && normalize(&document.title) == normalize(&proposal.title)
                && normalize(&document.body).contains(&normalize(&proposal.claim))
        });
        if duplicate.is_some() {
            return Err(invalid("duplicate active memory claim"));
        }
        let now = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| internal(error.to_string()))?;
        let id = format!(
            "{}-{}",
            sanitize_component(&proposal.memory_type),
            Ulid::new()
        );
        let document = MemoryDocument {
            id: id.clone(),
            memory_type: proposal.memory_type.clone(),
            title: proposal.title.clone(),
            created_at: now.clone(),
            updated_at: now.clone(),
            scope: proposal.scope,
            project_id: proposal.project_id.clone(),
            session_id: proposal.session_id.clone(),
            status: Status::Active,
            confidence_milli: proposal.confidence_milli,
            validation: proposal.validation,
            sources: proposal.evidence.clone(),
            supersedes: Vec::new(),
            superseded_by: Vec::new(),
            tags: deduplicate(proposal.tags.clone()),
            expires_at: None,
            last_validated_at: now,
            successful_reuse_count: 0,
            body: format!("# {}\n\n{}\n", proposal.title, proposal.claim),
        };
        let path = self.path_for(&document);
        atomic_write(&path, document.to_markdown().as_bytes())?;
        self.rebuild_index()?;
        Ok(document)
    }
}

fn contains_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        ["api", "_key="].concat(),
        ["api", "-key:"].concat(),
        ["authorization", ": bearer"].concat(),
        ["private", " key-----"].concat(),
        ["secret", "_access_key"].concat(),
        ["gh", "p_"].concat(),
        ["s", "k-"].concat(),
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}
