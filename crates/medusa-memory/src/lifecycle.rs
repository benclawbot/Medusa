use std::{fs, path::PathBuf};

use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    engine::MemoryEngine,
    schema::{MemoryDocument, MemoryProposal, Scope, Status, Validation},
    support::{
        LifecycleLock, atomic_write, deduplicate, durable_remove, first_claim, internal, invalid,
    },
};

#[derive(Debug, Deserialize, Serialize)]
struct SupersedeJournal {
    old_path: PathBuf,
    old_markdown: String,
    new_path: PathBuf,
    new_markdown: String,
}

impl MemoryEngine {
    /// Records a successful reuse as durable Markdown evidence.
    pub fn record_reuse(&self, id: &str, evidence: &str) -> MedusaResult<()> {
        if evidence.trim().is_empty() {
            return Err(invalid("reuse evidence cannot be empty"));
        }
        let _lock = LifecycleLock::acquire(&self.root)?;
        self.recover_lifecycle_journal()?;
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
        let _lock = LifecycleLock::acquire(&self.root)?;
        self.recover_lifecycle_journal()?;
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

        let journal = SupersedeJournal {
            old_path,
            old_markdown: old_document.to_markdown(),
            new_path,
            new_markdown: new_document.to_markdown(),
        };
        let encoded = serde_json::to_vec(&journal).map_err(|error| internal(error.to_string()))?;
        atomic_write(&self.lifecycle_journal_path(), &encoded)?;
        self.apply_supersede_journal(&journal)?;
        durable_remove(&self.lifecycle_journal_path())?;
        self.rebuild_index()
    }

    fn lifecycle_journal_path(&self) -> PathBuf {
        self.root.join("lifecycle-journal.json")
    }

    fn recover_lifecycle_journal(&self) -> MedusaResult<()> {
        let path = self.lifecycle_journal_path();
        let raw = match fs::read(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let journal = serde_json::from_slice::<SupersedeJournal>(&raw)
            .map_err(|error| internal(format!("invalid lifecycle recovery journal: {error}")))?;
        self.apply_supersede_journal(&journal)?;
        durable_remove(&path)
    }

    fn apply_supersede_journal(&self, journal: &SupersedeJournal) -> MedusaResult<()> {
        ensure_memory_path(&self.root, &journal.old_path)?;
        ensure_memory_path(&self.root, &journal.new_path)?;
        MemoryDocument::from_markdown(&journal.old_markdown)
            .map_err(|error| internal(format!("invalid old journal document: {error}")))?;
        MemoryDocument::from_markdown(&journal.new_markdown)
            .map_err(|error| internal(format!("invalid new journal document: {error}")))?;
        atomic_write(&journal.old_path, journal.old_markdown.as_bytes())?;
        atomic_write(&journal.new_path, journal.new_markdown.as_bytes())
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

fn ensure_memory_path(root: &std::path::Path, path: &std::path::Path) -> MedusaResult<()> {
    if !path.starts_with(root) || path == root {
        return Err(invalid("lifecycle journal path escapes the memory root"));
    }
    Ok(())
}
