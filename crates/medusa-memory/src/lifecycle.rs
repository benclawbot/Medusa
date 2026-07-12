use medusa_core::MedusaResult;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::{
    engine::MemoryEngine,
    schema::{MemoryDocument, MemoryProposal, Scope, Status, Validation},
    support::{atomic_write, deduplicate, first_claim, internal, invalid},
};

impl MemoryEngine {
    /// Records a successful reuse as durable Markdown evidence.
    pub fn record_reuse(&self, id: &str, evidence: &str) -> MedusaResult<()> {
        if evidence.trim().is_empty() {
            return Err(invalid("reuse evidence cannot be empty"));
        }
        let (path, mut document) = self.read_by_id(id)?;
        document.successful_reuse_count = document.successful_reuse_count.saturating_add(1);
        document.updated_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| internal(error.to_string()))?;
        document.sources.push(evidence.to_owned());
        document.sources = deduplicate(document.sources);
        atomic_write(&path, document.to_markdown().as_bytes())?;
        self.rebuild_index()
    }

    /// Supersedes an active document while preserving both records for audit.
    pub fn supersede(&self, old_id: &str, new_id: &str) -> MedusaResult<()> {
        if old_id == new_id {
            return Err(invalid("memory cannot supersede itself"));
        }
        let (old_path, mut old_document) = self.read_by_id(old_id)?;
        let (new_path, mut new_document) = self.read_by_id(new_id)?;
        if old_document.status != Status::Active || new_document.status != Status::Active {
            return Err(invalid("supersession requires active documents"));
        }
        old_document.status = Status::Superseded;
        old_document.superseded_by.push(new_id.to_owned());
        old_document.superseded_by = deduplicate(old_document.superseded_by);
        new_document.supersedes.push(old_id.to_owned());
        new_document.supersedes = deduplicate(new_document.supersedes);
        let now = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| internal(error.to_string()))?;
        old_document.updated_at.clone_from(&now);
        new_document.updated_at = now;
        atomic_write(&old_path, old_document.to_markdown().as_bytes())?;
        atomic_write(&new_path, new_document.to_markdown().as_bytes())?;
        self.rebuild_index()
    }

    /// Compacts selected active documents into a summary without deleting source memory.
    pub fn compact(&self, ids: &[String], title: &str) -> MedusaResult<MemoryDocument> {
        if ids.len() < 2 {
            return Err(invalid("compaction requires at least two documents"));
        }
        let mut claims = Vec::new();
        let mut sources = Vec::new();
        let mut tags = Vec::new();
        let mut confidence = 1_000_u16;
        let mut project_id = None;
        for id in ids {
            let (_, document) = self.read_by_id(id)?;
            if document.status != Status::Active || !document.validation.high_confidence() {
                return Err(invalid("only active validated memory may be compacted"));
            }
            claims.push(format!(
                "- {}: {}",
                document.title,
                first_claim(&document.body)
            ));
            sources.push(format!("memory://{}", document.id));
            tags.extend(document.tags);
            confidence = confidence.min(document.confidence_milli);
            project_id = project_id.or(document.project_id);
        }
        let proposal = MemoryProposal {
            memory_type: "summary".into(),
            title: title.into(),
            claim: format!("Compacted validated memory:\n{}", claims.join("\n")),
            evidence: sources,
            confidence_milli: confidence,
            validation: Validation::Observed,
            scope: Scope::Project,
            project_id,
            session_id: None,
            tags,
        };
        self.commit_proposal(&proposal)
    }
}
