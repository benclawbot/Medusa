use std::{fs, path::PathBuf};

use medusa_core::MedusaResult;
use walkdir::WalkDir;

use crate::{
    engine::MemoryEngine,
    schema::MemoryDocument,
    support::{atomic_write, invalid, sanitize_component},
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
        if !self.index_path.exists() {
            self.rebuild_index()?;
        }
        Ok(())
    }

    pub(crate) fn documents(&self) -> MedusaResult<Vec<(PathBuf, MemoryDocument)>> {
        let mut documents = Vec::new();
        for entry in WalkDir::new(&self.root).into_iter().filter_map(Result::ok) {
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

    pub(crate) fn read_by_id(&self, id: &str) -> MedusaResult<(PathBuf, MemoryDocument)> {
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
