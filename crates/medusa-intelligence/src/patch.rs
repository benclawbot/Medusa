use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use medusa_core::{MedusaResult, repository_mutation};
use serde::{Deserialize, Serialize};

use crate::{
    language::CodeIndex,
    support::{hash, invalid, valid_identifier, validate_relative},
};

const JOURNAL_SCHEMA: u32 = 1;
const JOURNAL_ROOT: &str = ".medusa/patch-transactions";
static REPLACEMENT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A byte-range replacement guarded by expected original content.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TextEdit {
    pub path: PathBuf,
    pub start_byte: usize,
    pub end_byte: usize,
    pub expected: String,
    pub replacement: String,
}

/// Evidence emitted by an applied patch transaction.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransactionReceipt {
    pub transaction_id: String,
    pub changed_paths: Vec<PathBuf>,
    pub before_hashes: BTreeMap<PathBuf, String>,
    pub after_hashes: BTreeMap<PathBuf, String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum JournalState {
    Prepared,
    Applying,
    Applied,
    Verified,
    Committed,
    RollingBack,
    RolledBack,
}

impl JournalState {
    fn terminal(self) -> bool {
        matches!(self, Self::Committed | Self::RolledBack)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct JournalEntry {
    path: PathBuf,
    before_hash: String,
    after_hash: String,
    backup: String,
    staged: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct PatchJournal {
    schema: u32,
    transaction_id: String,
    state: JournalState,
    entries: Vec<JournalEntry>,
    applied_paths: BTreeSet<PathBuf>,
}

/// Multi-file transaction with overlap, stale-content, containment, and crash-recovery checks.
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
        if !index.parse_errors.is_empty() {
            return Err(invalid(format!(
                "rename refused because the repository index contains parse errors: {:?}",
                index.parse_errors
            )));
        }
        let definitions = index.definitions(old_name);
        match definitions.as_slice() {
            [] => return Err(invalid(format!("symbol not found: {old_name}"))),
            [_] => {}
            _ => {
                return Err(invalid(format!(
                    "ambiguous symbol rename: {old_name} has {} indexed definitions",
                    definitions.len()
                )));
            }
        }
        let definition = definitions[0];
        if definition
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("rs")
        {
            return Err(invalid(
                "guarded symbol rename is currently production-certified for Rust only",
            ));
        }
        let references = index.references(old_name);
        if references.iter().any(|reference| {
            reference
                .path
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("rs")
        }) {
            return Err(invalid(format!(
                "ambiguous cross-language symbol occurrences prevent guarded rename: {old_name}"
            )));
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

    /// Validates and durably journals all touched files before replacing originals.
    pub fn commit(self, repo: &Path) -> MedusaResult<TransactionReceipt> {
        if self.edits.is_empty() {
            return Err(invalid("transaction contains no edits"));
        }
        let _repository_guard = repository_mutation::lock(repo);

        let mut grouped: BTreeMap<PathBuf, Vec<TextEdit>> = BTreeMap::new();
        for edit in self.edits {
            grouped.entry(edit.path.clone()).or_default().push(edit);
        }

        let transaction_id = transaction_id(&grouped)?;
        let directory = journal_directory(repo, &transaction_id);
        if directory.exists() {
            return Err(invalid(format!(
                "patch transaction already exists: {transaction_id}"
            )));
        }
        fs::create_dir_all(&directory)?;

        let mut staged = Vec::new();
        let mut entries = Vec::new();
        let mut before_hashes = BTreeMap::new();
        let mut after_hashes = BTreeMap::new();
        for (index, (relative_path, mut edits)) in grouped.into_iter().enumerate() {
            validate_relative(&relative_path)?;
            let path = repo.join(&relative_path);
            let original = fs::read(&path)?;
            let original_text = std::str::from_utf8(&original).map_err(|_| {
                invalid(format!(
                    "patch target is not UTF-8: {}",
                    relative_path.display()
                ))
            })?;
            let original_permissions = fs::metadata(&path)?.permissions();
            let before_hash = hash(&original);
            before_hashes.insert(relative_path.clone(), before_hash.clone());
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
                let actual = original_text
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
            let mut updated = original_text.to_owned();
            for edit in edits.into_iter().rev() {
                updated.replace_range(edit.start_byte..edit.end_byte, &edit.replacement);
            }
            let updated = updated.into_bytes();
            let after_hash = hash(&updated);
            after_hashes.insert(relative_path.clone(), after_hash.clone());

            let backup = format!("{index}.before");
            let staged_name = format!("{index}.after");
            write_synced(&directory.join(&backup), &original)?;
            fs::set_permissions(directory.join(&backup), original_permissions.clone())?;
            write_synced(&directory.join(&staged_name), &updated)?;
            fs::set_permissions(directory.join(&staged_name), original_permissions)?;
            entries.push(JournalEntry {
                path: relative_path.clone(),
                before_hash,
                after_hash,
                backup,
                staged: staged_name,
            });
            staged.push((relative_path, path));
        }

        let mut journal = PatchJournal {
            schema: JOURNAL_SCHEMA,
            transaction_id: transaction_id.clone(),
            state: JournalState::Prepared,
            entries,
            applied_paths: BTreeSet::new(),
        };
        persist_journal(&directory, &journal)?;
        journal.state = JournalState::Applying;
        persist_journal(&directory, &journal)?;

        for (relative, destination) in &staged {
            let entry = journal
                .entries
                .iter()
                .find(|entry| &entry.path == relative)
                .ok_or_else(|| invalid("journal entry disappeared during apply"))?;
            let current = fs::read(destination)?;
            if hash(&current) != entry.before_hash {
                return Err(invalid(format!(
                    "patch target changed before commit: {}",
                    relative.display()
                )));
            }
            replace_file(&directory.join(&entry.staged), destination)?;
            journal.applied_paths.insert(relative.clone());
            persist_journal(&directory, &journal)?;
        }
        journal.state = JournalState::Applied;
        persist_journal(&directory, &journal)?;

        Ok(TransactionReceipt {
            transaction_id,
            changed_paths: staged.into_iter().map(|(path, _)| path).collect(),
            before_hashes,
            after_hashes,
        })
    }
}

/// Recovers every non-terminal structured patch transaction to its exact pre-apply state.
pub fn recover_patch_transactions(repo: &Path) -> MedusaResult<Vec<String>> {
    let mut recovered = Vec::new();
    let mut directories = journal_directories(repo)?;
    directories.reverse();
    for directory in directories {
        let mut journal = load_journal(&directory)?;
        if journal.state.terminal() {
            continue;
        }
        rollback(repo, &directory, &mut journal)?;
        recovered.push(journal.transaction_id);
    }
    Ok(recovered)
}

/// Promotes applied transactions after verification, or restores them when verification fails.
pub fn finalize_patch_transactions(repo: &Path, verified: bool) -> MedusaResult<Vec<String>> {
    let mut directories = journal_directories(repo)?;
    if !verified {
        directories.reverse();
    }
    let mut finalized = Vec::new();
    for directory in directories {
        let mut journal = load_journal(&directory)?;
        if journal.state.terminal() {
            continue;
        }
        if verified && journal.state == JournalState::Applied {
            journal.state = JournalState::Verified;
            persist_journal(&directory, &journal)?;
            journal.state = JournalState::Committed;
            persist_journal(&directory, &journal)?;
        } else {
            rollback(repo, &directory, &mut journal)?;
        }
        finalized.push(journal.transaction_id);
    }
    Ok(finalized)
}

fn rollback(repo: &Path, directory: &Path, journal: &mut PatchJournal) -> MedusaResult<()> {
    journal.state = JournalState::RollingBack;
    persist_journal(directory, journal)?;
    for entry in journal.entries.iter().rev() {
        if !journal.applied_paths.contains(&entry.path) {
            continue;
        }
        let destination = repo.join(&entry.path);
        replace_file(&directory.join(&entry.backup), &destination)?;
        let restored = fs::read(&destination)?;
        if hash(&restored) != entry.before_hash {
            return Err(invalid(format!(
                "rollback hash mismatch for {}",
                entry.path.display()
            )));
        }
    }
    journal.state = JournalState::RolledBack;
    persist_journal(directory, journal)
}

fn transaction_id(grouped: &BTreeMap<PathBuf, Vec<TextEdit>>) -> MedusaResult<String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| invalid("system clock is before the Unix epoch"))?
        .as_nanos();
    let payload = serde_json::to_vec(&(timestamp, grouped))?;
    Ok(format!("{timestamp:032x}-{}", &hash(&payload)[..16]))
}

fn journal_directory(repo: &Path, transaction_id: &str) -> PathBuf {
    repo.join(JOURNAL_ROOT).join(transaction_id)
}

fn journal_directories(repo: &Path) -> MedusaResult<Vec<PathBuf>> {
    let root = repo.join(JOURNAL_ROOT);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut directories = fs::read_dir(root)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    directories.sort();
    Ok(directories)
}

fn load_journal(directory: &Path) -> MedusaResult<PatchJournal> {
    let journal: PatchJournal = serde_json::from_slice(&fs::read(directory.join("journal.json"))?)?;
    if journal.schema != JOURNAL_SCHEMA {
        return Err(invalid(format!(
            "unsupported patch journal schema: {}",
            journal.schema
        )));
    }
    let directory_id = directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("patch journal directory is not valid UTF-8"))?;
    if journal.transaction_id != directory_id {
        return Err(invalid("patch journal identity mismatch"));
    }
    Ok(journal)
}

fn persist_journal(directory: &Path, journal: &PatchJournal) -> MedusaResult<()> {
    let bytes = serde_json::to_vec_pretty(journal)?;
    let temporary = directory.join("journal.json.tmp");
    write_synced(&temporary, &bytes)?;
    fs::rename(&temporary, directory.join("journal.json"))?;
    sync_directory(directory)
}

fn write_synced(path: &Path, bytes: &[u8]) -> MedusaResult<()> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn replace_file(source: &Path, destination: &Path) -> MedusaResult<()> {
    let bytes = fs::read(source)?;
    let permissions = fs::metadata(source)?.permissions();
    let nonce = REPLACEMENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = destination.with_extension(format!(
        "medusa-transaction-replace-{}-{nonce}",
        std::process::id()
    ));
    write_synced(&temporary, &bytes)?;
    fs::set_permissions(&temporary, permissions)?;
    fs::rename(&temporary, destination)?;
    if let Some(parent) = destination.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> MedusaResult<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    use super::*;

    fn replace(repo: &Path, expected: &str, replacement: &str) {
        let mut transaction = PatchTransaction::new();
        transaction
            .add_edit(TextEdit {
                path: "file.rs".into(),
                start_byte: 0,
                end_byte: expected.len(),
                expected: expected.into(),
                replacement: replacement.into(),
            })
            .expect("edit");
        transaction.commit(repo).expect("apply");
    }

    fn transaction(expected: &str, replacement: &str) -> PatchTransaction {
        let mut transaction = PatchTransaction::new();
        transaction
            .add_edit(TextEdit {
                path: "file.rs".into(),
                start_byte: 0,
                end_byte: expected.len(),
                expected: expected.into(),
                replacement: replacement.into(),
            })
            .expect("edit");
        transaction
    }

    #[test]
    fn failed_verification_restores_before_state() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("file.rs"), "abcdef").expect("file");
        replace(directory.path(), "abc", "xyz");

        finalize_patch_transactions(directory.path(), false).expect("rollback");

        assert_eq!(
            fs::read_to_string(directory.path().join("file.rs")).expect("file"),
            "abcdef"
        );
    }

    #[test]
    fn startup_recovery_is_idempotent() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("file.rs"), "abcdef").expect("file");
        replace(directory.path(), "abc", "xyz");

        assert_eq!(
            recover_patch_transactions(directory.path())
                .expect("recover")
                .len(),
            1
        );
        assert!(
            recover_patch_transactions(directory.path())
                .expect("recover again")
                .is_empty()
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("file.rs")).expect("file"),
            "abcdef"
        );
    }

    #[test]
    fn stacked_transactions_rollback_newest_first() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("file.rs"), "aaa").expect("file");
        replace(directory.path(), "aaa", "bbb");
        replace(directory.path(), "bbb", "ccc");

        finalize_patch_transactions(directory.path(), false).expect("rollback");

        assert_eq!(
            fs::read_to_string(directory.path().join("file.rs")).expect("file"),
            "aaa"
        );
    }

    #[test]
    fn verification_commit_accepts_formatter_changes() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("file.rs"), "aaa").expect("file");
        replace(directory.path(), "aaa", "bbb");
        fs::write(directory.path().join("file.rs"), "bbb\n").expect("formatted");

        finalize_patch_transactions(directory.path(), true).expect("commit");

        assert_eq!(
            fs::read_to_string(directory.path().join("file.rs")).expect("file"),
            "bbb\n"
        );
    }

    #[test]
    fn overlapping_concurrent_transactions_never_silently_overwrite() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("file.rs"), "aaa").expect("file");
        let repo = Arc::new(directory.path().to_path_buf());
        let barrier = Arc::new(Barrier::new(3));

        let handles = ["bbb", "ccc"].map(|replacement| {
            let repo = Arc::clone(&repo);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let transaction = transaction("aaa", replacement);
                barrier.wait();
                transaction.commit(&repo)
            })
        });
        barrier.wait();

        let results = handles.map(|handle| handle.join().expect("worker"));
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        let final_content = fs::read_to_string(directory.path().join("file.rs")).expect("file");
        assert!(matches!(final_content.as_str(), "bbb" | "ccc"));
    }
}
