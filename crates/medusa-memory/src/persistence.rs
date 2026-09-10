use std::{fs, path::PathBuf};

use medusa_core::MedusaResult;
use walkdir::WalkDir;

use crate::{
    engine::MemoryEngine,
    index::recover_index_swap,
    schema::MemoryDocument,
    support::{atomic_write, internal, invalid, sanitize_component},
};

impl MemoryEngine {
    pub(crate) fn initialize_layout(&self) -> MedusaResult<()> {
        let readme = self.root.join("README.md");
        if !readme.exists() {
            atomic_write(
                &readme,
                b"# Medusa Memory\n\nCanonical semantic memory is Markdown. The SQLite index is disposable and rebuildable.\n",
            )?;
        }
        // Lifecycle operations keep their journal until the derived index is rebuilt. Replay
        // before serving reads so a crash cannot expose deleted/superseded content from SQLite.
        self.recover_lifecycle_journal()?;
        recover_index_swap(&self.index_path)?;
        if !self.index_path.exists() {
            self.rebuild_index()?;
        }
        Ok(())
    }

    pub(crate) fn documents(&self) -> MedusaResult<Vec<(PathBuf, MemoryDocument)>> {
        let mut documents = Vec::new();
        for entry in WalkDir::new(&self.root) {
            // WalkDir I/O failures (unreadable directories, vanishing paths) must
            // surface to the caller instead of being silently dropped: a partial
            // document list would otherwise make reads and index rebuilds wrong.
            let entry = entry.map_err(|error| {
                tracing::warn!(
                    root = %self.root.display(),
                    %error,
                    "memory walk failed"
                );
                internal(format!("memory directory walk failed: {error}"))
            })?;
            if !entry.file_type().is_file()
                || entry
                    .path()
                    .extension()
                    .is_none_or(|extension| extension != "md")
                || entry
                    .path()
                    .file_name()
                    .is_some_and(|name| name == "README.md")
                || entry
                    .path()
                    .components()
                    .any(|component| component.as_os_str() == "archive")
            {
                continue;
            }
            let text = fs::read_to_string(entry.path())?;
            documents.push((
                entry.path().to_path_buf(),
                MemoryDocument::from_markdown(&text)?,
            ));
        }
        documents.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(documents)
    }

    pub fn read_by_id(&self, id: &str) -> MedusaResult<(PathBuf, MemoryDocument)> {
        self.documents()?
            .into_iter()
            .find(|(_, document)| document.id == id)
            .ok_or_else(|| invalid(format!("memory document not found: {id}")))
    }

    pub(crate) fn path_for(&self, document: &MemoryDocument) -> PathBuf {
        let directory = match document.memory_type.as_str() {
            "lesson" | "command" => "lessons",
            "failure" => "failures",
            "pattern" => "patterns",
            "decision" => "decisions",
            "summary" => "summaries",
            _ => "entities",
        };
        self.root
            .join(directory)
            .join(format!("{}.md", sanitize_component(&document.id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_errors_surface_instead_of_silently_truncating_the_corpus() {
        let directory = tempfile::tempdir().expect("tempdir");
        let engine = MemoryEngine::new(directory.path()).expect("engine");
        // Vanishing the memory root makes the directory walk fail. The old
        // `filter_map(Result::ok)` turned that into an empty corpus, so reads
        // and index rebuilds silently operated on partial state.
        fs::remove_dir_all(directory.path().join(".medusa/memory")).expect("remove root");
        let error = engine
            .read_by_id("anything")
            .expect_err("walk failure must surface");
        assert!(error.to_string().contains("directory walk failed"));
    }
}
