use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};

use crate::{
    language::CodeIndex,
    support::{hash, invalid, valid_identifier, validate_relative},
};

/// A byte-range replacement guarded by expected original content.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TextEdit {
    pub path: PathBuf,
    pub start_byte: usize,
    pub end_byte: usize,
    pub expected: String,
    pub replacement: String,
}

/// Evidence emitted by a committed patch transaction.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransactionReceipt {
    pub changed_paths: Vec<PathBuf>,
    pub before_hashes: BTreeMap<PathBuf, String>,
    pub after_hashes: BTreeMap<PathBuf, String>,
}

/// Multi-file transaction with overlap, stale-content, and containment checks.
#[derive(Clone, Debug, Default)]
pub struct PatchTransaction {
    edits: Vec<TextEdit>,
}

impl PatchTransaction {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_edit(&mut self, edit: TextEdit) -> MedusaResult<()> {
        validate_relative(&edit.path)?;
        if edit.start_byte > edit.end_byte {
            return Err(invalid("edit start exceeds end"));
        }
        self.edits.push(edit);
        Ok(())
    }

    /// Renames every indexed definition and reference for one identifier.
    pub fn rename_symbol(
        &mut self,
        index: &CodeIndex,
        old_name: &str,
        new_name: &str,
    ) -> MedusaResult<usize> {
        if !valid_identifier(new_name) {
            return Err(invalid(format!(
                "invalid replacement identifier: {new_name}"
            )));
        }
        let references = index.references(old_name);
        if references.is_empty() {
            return Err(invalid(format!("symbol not found: {old_name}")));
        }
        for reference in references {
            self.add_edit(TextEdit {
                path: reference.path.clone(),
                start_byte: reference.start_byte,
                end_byte: reference.end_byte,
                expected: old_name.to_owned(),
                replacement: new_name.to_owned(),
            })?;
        }
        Ok(references.len())
    }

    /// Validates and atomically stages all touched files before replacing originals.
    pub fn commit(self, repo: &Path) -> MedusaResult<TransactionReceipt> {
        if self.edits.is_empty() {
            return Err(invalid("transaction contains no edits"));
        }
        let mut grouped: BTreeMap<PathBuf, Vec<TextEdit>> = BTreeMap::new();
        for edit in self.edits {
            grouped.entry(edit.path.clone()).or_default().push(edit);
        }

        let mut staged = Vec::new();
        let mut before_hashes = BTreeMap::new();
        let mut after_hashes = BTreeMap::new();
        for (relative_path, mut edits) in grouped {
            validate_relative(&relative_path)?;
            let path = repo.join(&relative_path);
            let original = fs::read_to_string(&path)?;
            let original_permissions = fs::metadata(&path)?.permissions();
            before_hashes.insert(relative_path.clone(), hash(original.as_bytes()));
            edits.sort_by_key(|edit| edit.start_byte);
            for pair in edits.windows(2) {
                if pair[0].end_byte > pair[1].start_byte {
                    return Err(invalid(format!(
                        "overlapping edits in {}",
                        relative_path.display()
                    )));
                }
            }
            for edit in &edits {
                let actual = original
                    .get(edit.start_byte..edit.end_byte)
                    .ok_or_else(|| {
                        invalid(format!("edit range outside {}", relative_path.display()))
                    })?;
                if actual != edit.expected {
                    return Err(invalid(format!(
                        "stale edit in {}: expected {:?}, found {:?}",
                        relative_path.display(),
                        edit.expected,
                        actual
                    )));
                }
            }
            let mut updated = original;
            for edit in edits.into_iter().rev() {
                updated.replace_range(edit.start_byte..edit.end_byte, &edit.replacement);
            }
            after_hashes.insert(relative_path.clone(), hash(updated.as_bytes()));
            let temporary = path.with_extension("medusa-transaction");
            fs::write(&temporary, updated)?;
            fs::set_permissions(&temporary, original_permissions)?;
            staged.push((relative_path, path, temporary));
        }

        for (_, path, temporary) in &staged {
            if let Err(error) = fs::rename(temporary, path) {
                for (_, _, pending) in &staged {
                    let _ = fs::remove_file(pending);
                }
                return Err(error.into());
            }
        }
        Ok(TransactionReceipt {
            changed_paths: staged.into_iter().map(|(path, _, _)| path).collect(),
            before_hashes,
            after_hashes,
        })
    }
}
