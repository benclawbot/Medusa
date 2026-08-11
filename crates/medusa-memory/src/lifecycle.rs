use std::{
    collections::BTreeSet,
    fs,
    path::{Component, PathBuf},
};

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

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SupersedeJournal {
    old_path: PathBuf,
    old_markdown: String,
    new_path: PathBuf,
    new_markdown: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DeleteJournal {
    paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum LifecycleJournal {
    Supersede(SupersedeJournal),
    Delete(DeleteJournal),
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
        let encoded = serde_json::to_vec(&LifecycleJournal::Supersede(journal.clone()))
            .map_err(|error| internal(error.to_string()))?;
        atomic_write(&self.lifecycle_journal_path(), &encoded)?;
        self.apply_supersede_journal(&journal)?;
        // Keep the journal until the rebuild is durable. A crash between Markdown mutation
        // and index rebuild must replay the lifecycle operation instead of leaving stale hits.
        self.rebuild_index()?;
        durable_remove(&self.lifecycle_journal_path())
    }

    /// Deletes canonical memory and every compacted summary that derives from it.
    ///
    /// The operation is journaled before the first removal, is idempotent on recovery, and
    /// rebuilds the disposable index before clearing its journal. This prevents a stale index
    /// or derived summary from making deleted memory retrievable after a crash.
    pub fn delete(&self, id: &str) -> MedusaResult<Vec<String>> {
        let _lock = LifecycleLock::acquire(&self.root)?;
        self.recover_lifecycle_journal()?;
        let documents = self.documents()?;
        if !documents.iter().any(|(_, document)| document.id == id) {
            return Err(invalid(format!("memory document not found: {id}")));
        }

        let mut deleted = BTreeSet::from([id.to_owned()]);
        loop {
            let before = deleted.len();
            for (_, document) in &documents {
                if deleted.contains(&document.id) {
                    continue;
                }
                if document.sources.iter().any(|source| {
                    source
                        .strip_prefix("memory://")
                        .is_some_and(|source_id| deleted.contains(source_id))
                }) {
                    deleted.insert(document.id.clone());
                }
            }
            if deleted.len() == before {
                break;
            }
        }

        let mut paths = documents
            .iter()
            .filter(|(_, document)| deleted.contains(&document.id))
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        paths.sort();
        let journal = DeleteJournal { paths };
        let encoded = serde_json::to_vec(&LifecycleJournal::Delete(journal.clone()))
            .map_err(|error| internal(error.to_string()))?;
        atomic_write(&self.lifecycle_journal_path(), &encoded)?;
        self.apply_delete_journal(&journal)?;
        self.rebuild_index()?;
        durable_remove(&self.lifecycle_journal_path())?;
        Ok(deleted.into_iter().collect())
    }

    fn lifecycle_journal_path(&self) -> PathBuf {
        self.root.join("lifecycle-journal.json")
    }

    pub(crate) fn recover_lifecycle_journal(&self) -> MedusaResult<()> {
        let path = self.lifecycle_journal_path();
        let raw = match fs::read(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let journal = serde_json::from_slice::<LifecycleJournal>(&raw)
            .map_err(|error| internal(format!("invalid lifecycle recovery journal: {error}")))?;
        match &journal {
            LifecycleJournal::Supersede(journal) => self.apply_supersede_journal(journal)?,
            LifecycleJournal::Delete(journal) => self.apply_delete_journal(journal)?,
        }
        self.rebuild_index()?;
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

    fn apply_delete_journal(&self, journal: &DeleteJournal) -> MedusaResult<()> {
        for path in &journal.paths {
            ensure_memory_path(&self.root, path)?;
        }
        for path in &journal.paths {
            durable_remove(path)?;
        }
        Ok(())
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
    let relative = path
        .strip_prefix(root)
        .map_err(|_| invalid("lifecycle journal path escapes the memory root"))?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid("lifecycle journal path escapes the memory root"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(title: &str, claim: &str) -> MemoryProposal {
        MemoryProposal {
            memory_type: "command".into(),
            title: title.into(),
            claim: claim.into(),
            evidence: vec!["artifact://sessions/ses-delete/verification".into()],
            confidence_milli: 950,
            validation: Validation::TestVerified,
            scope: Scope::Project,
            project_id: Some("sha256:delete-test".into()),
            session_id: Some("ses-delete".into()),
            tags: vec!["lifecycle".into()],
        }
    }

    #[test]
    fn deletion_removes_source_dependent_summaries_and_index_hits() {
        let directory = tempfile::tempdir().expect("tempdir");
        let engine = MemoryEngine::new(directory.path()).expect("engine");
        let first = engine
            .commit_proposal(&proposal(
                "Sensitive source",
                "Unique lifecycle deletion marker.",
            ))
            .expect("first");
        let second = engine
            .commit_proposal(&proposal("Other source", "Independent retained memory."))
            .expect("second");
        let summary = engine
            .compact(&[first.id.clone(), second.id.clone()], "Derived summary")
            .expect("summary");

        let deleted = engine.delete(&first.id).expect("delete");
        assert!(deleted.contains(&first.id));
        assert!(deleted.contains(&summary.id));
        assert!(!deleted.contains(&second.id));
        assert!(engine.read_by_id(&first.id).is_err());
        assert!(engine.read_by_id(&summary.id).is_err());
        assert!(engine.read_by_id(&second.id).is_ok());
        assert!(
            engine
                .search("Unique lifecycle deletion marker", Scope::Project, 10)
                .expect("search")
                .is_empty()
        );
    }

    #[test]
    fn recovery_replays_delete_before_rebuilding_index() {
        let directory = tempfile::tempdir().expect("tempdir");
        let engine = MemoryEngine::new(directory.path()).expect("engine");
        let document = engine
            .commit_proposal(&proposal("Crash deletion", "Crash-safe deletion marker."))
            .expect("document");
        let (path, _) = engine.read_by_id(&document.id).expect("document path");
        let journal = LifecycleJournal::Delete(DeleteJournal { paths: vec![path] });
        atomic_write(
            &engine.lifecycle_journal_path(),
            &serde_json::to_vec(&journal).expect("journal json"),
        )
        .expect("journal");

        engine.recover_lifecycle_journal().expect("recover");
        assert!(engine.read_by_id(&document.id).is_err());
        assert!(
            engine
                .search("Crash-safe deletion marker", Scope::Project, 10)
                .expect("search")
                .is_empty()
        );
        assert!(!engine.lifecycle_journal_path().exists());
    }

    #[test]
    fn recovery_rejects_parent_directory_escape_without_deleting_outside_file() {
        let directory = tempfile::tempdir().expect("tempdir");
        let engine = MemoryEngine::new(directory.path()).expect("engine");
        let outside = directory.path().join("outside.md");
        std::fs::write(&outside, "must survive").expect("outside file");
        let escaped = engine.root.join("..").join("..").join("outside.md");
        let journal = LifecycleJournal::Delete(DeleteJournal {
            paths: vec![escaped],
        });
        atomic_write(
            &engine.lifecycle_journal_path(),
            &serde_json::to_vec(&journal).expect("journal json"),
        )
        .expect("journal");

        assert!(engine.recover_lifecycle_journal().is_err());
        assert_eq!(
            std::fs::read_to_string(&outside).expect("outside survives"),
            "must survive"
        );
        assert!(engine.lifecycle_journal_path().exists());
    }
}
