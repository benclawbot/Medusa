use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    FileSnapshot, StructuredEditAudit, StructuredEditPlan, StructuredFileOperation,
    StructuredTextEdit,
    support::hash,
};

const JOURNAL_SCHEMA: u32 = 1;
const JOURNAL_ROOT: &str = ".medusa/structured-transactions";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredTransactionPhase {
    Prepared,
    Staged,
    Applying,
    Applied,
    Committed,
    RollingBack,
    RolledBack,
}

impl StructuredTransactionPhase {
    fn terminal(self) -> bool {
        matches!(self, Self::Committed | Self::RolledBack)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureInjectionPoint {
    AfterPrepare,
    AfterStage,
    DuringApply(usize),
    AfterApply,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StructuredTransactionReceipt {
    pub transaction_id: String,
    pub plan_id: String,
    pub phase: StructuredTransactionPhase,
    pub touched_paths: Vec<PathBuf>,
    pub before_hashes: BTreeMap<PathBuf, Option<String>>,
    pub after_hashes: BTreeMap<PathBuf, Option<String>>,
    pub audit: StructuredEditAudit,
}

#[derive(Debug)]
pub enum StructuredTransactionError {
    Validation(Vec<crate::StructuredEditError>),
    Io(std::io::Error),
    Serialization(serde_json::Error),
    InvalidJournal(String),
    ExternalModification { path: PathBuf },
    InjectedFailure(FailureInjectionPoint),
}

impl std::fmt::Display for StructuredTransactionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for StructuredTransactionError {}

impl From<std::io::Error> for StructuredTransactionError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for StructuredTransactionError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct JournalEntry {
    path: PathBuf,
    before_exists: bool,
    before_hash: Option<String>,
    after_exists: bool,
    after_hash: Option<String>,
    backup: Option<String>,
    staged: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StructuredJournal {
    schema: u32,
    transaction_id: String,
    plan_id: String,
    phase: StructuredTransactionPhase,
    entries: Vec<JournalEntry>,
    applied_paths: BTreeSet<PathBuf>,
    audit: StructuredEditAudit,
}

pub struct StructuredTransaction {
    plan: StructuredEditPlan,
    failure_injection: Option<FailureInjectionPoint>,
}

impl StructuredTransaction {
    #[must_use]
    pub fn new(plan: StructuredEditPlan) -> Self {
        Self {
            plan,
            failure_injection: None,
        }
    }

    #[must_use]
    pub fn with_failure_injection(mut self, point: FailureInjectionPoint) -> Self {
        self.failure_injection = Some(point);
        self
    }

    pub fn preview(
        &self,
        repo: &Path,
    ) -> Result<StructuredEditAudit, StructuredTransactionError> {
        let snapshots = load_snapshots(repo, &self.plan)?;
        self.plan
            .validate(&snapshots)
            .map_err(StructuredTransactionError::Validation)
    }

    pub fn commit(
        self,
        repo: &Path,
    ) -> Result<StructuredTransactionReceipt, StructuredTransactionError> {
        let snapshots = load_snapshots(repo, &self.plan)?;
        let audit = self
            .plan
            .validate(&snapshots)
            .map_err(StructuredTransactionError::Validation)?;
        let transaction_id = transaction_id(&self.plan, &audit)?;
        let directory = repo.join(JOURNAL_ROOT).join(&transaction_id);
        if directory.exists() {
            return Err(StructuredTransactionError::InvalidJournal(format!(
                "transaction already exists: {transaction_id}"
            )));
        }
        fs::create_dir_all(&directory)?;

        let planned = render_plan(repo, &self.plan, &snapshots)?;
        let mut journal = prepare_journal(&directory, &transaction_id, &self.plan, audit, planned)?;
        persist_journal(&directory, &journal)?;
        self.fail_if(FailureInjectionPoint::AfterPrepare)?;

        journal.phase = StructuredTransactionPhase::Staged;
        persist_journal(&directory, &journal)?;
        self.fail_if(FailureInjectionPoint::AfterStage)?;

        journal.phase = StructuredTransactionPhase::Applying;
        persist_journal(&directory, &journal)?;
        for (index, entry) in journal.entries.clone().into_iter().enumerate() {
            verify_current(repo, &entry)?;
            apply_entry(repo, &directory, &entry)?;
            journal.applied_paths.insert(entry.path.clone());
            persist_journal(&directory, &journal)?;
            self.fail_if(FailureInjectionPoint::DuringApply(index))?;
        }

        journal.phase = StructuredTransactionPhase::Applied;
        persist_journal(&directory, &journal)?;
        self.fail_if(FailureInjectionPoint::AfterApply)?;

        journal.phase = StructuredTransactionPhase::Committed;
        persist_journal(&directory, &journal)?;
        receipt(&journal)
    }

    fn fail_if(&self, point: FailureInjectionPoint) -> Result<(), StructuredTransactionError> {
        if self.failure_injection == Some(point) {
            return Err(StructuredTransactionError::InjectedFailure(point));
        }
        Ok(())
    }
}

pub fn recover_structured_transactions(
    repo: &Path,
) -> Result<Vec<StructuredTransactionReceipt>, StructuredTransactionError> {
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

    let mut receipts = Vec::new();
    for directory in directories {
        let mut journal = load_journal(&directory)?;
        if journal.phase.terminal() {
            receipts.push(receipt(&journal)?);
            continue;
        }
        rollback(repo, &directory, &mut journal)?;
        receipts.push(receipt(&journal)?);
    }
    Ok(receipts)
}

fn prepare_journal(
    directory: &Path,
    transaction_id: &str,
    plan: &StructuredEditPlan,
    audit: StructuredEditAudit,
    planned: BTreeMap<PathBuf, Option<Vec<u8>>>,
) -> Result<StructuredJournal, StructuredTransactionError> {
    let mut entries = Vec::new();
    for (index, (path, after)) in planned.into_iter().enumerate() {
        let before = fs::read(&path).ok();
        let before_hash = before.as_deref().map(hash);
        let after_hash = after.as_deref().map(hash);
        let backup = before.as_ref().map(|bytes| {
            let name = format!("{index}.before");
            write_synced(&directory.join(&name), bytes)
                .expect("journal backup must be writable after directory creation");
            name
        });
        let staged = after.as_ref().map(|bytes| {
            let name = format!("{index}.after");
            write_synced(&directory.join(&name), bytes)
                .expect("journal staging must be writable after directory creation");
            name
        });
        entries.push(JournalEntry {
            path,
            before_exists: before.is_some(),
            before_hash,
            after_exists: after.is_some(),
            after_hash,
            backup,
            staged,
        });
    }
    Ok(StructuredJournal {
        schema: JOURNAL_SCHEMA,
        transaction_id: transaction_id.to_owned(),
        plan_id: plan.id.clone(),
        phase: StructuredTransactionPhase::Prepared,
        entries,
        applied_paths: BTreeSet::new(),
        audit,
    })
}

fn render_plan(
    repo: &Path,
    plan: &StructuredEditPlan,
    snapshots: &BTreeMap<PathBuf, FileSnapshot>,
) -> Result<BTreeMap<PathBuf, Option<Vec<u8>>>, StructuredTransactionError> {
    let mut rendered = BTreeMap::new();
    let mut grouped: BTreeMap<&Path, Vec<&StructuredTextEdit>> = BTreeMap::new();
    for edit in &plan.text_edits {
        grouped.entry(&edit.path).or_default().push(edit);
    }
    for (path, mut edits) in grouped {
        edits.sort_by_key(|edit| edit.range);
        let mut content = snapshots
            .get(path)
            .ok_or_else(|| {
                StructuredTransactionError::InvalidJournal(format!(
                    "missing validated snapshot for {}",
                    path.display()
                ))
            })?
            .content
            .clone();
        for edit in edits.into_iter().rev() {
            content.replace_range(
                edit.range.start_byte..edit.range.end_byte,
                &edit.replacement,
            );
        }
        rendered.insert(repo.join(path), Some(content.into_bytes()));
    }

    for operation in &plan.file_operations {
        match operation {
            StructuredFileOperation::Create { path, content, .. } => {
                rendered.insert(repo.join(path), Some(content.clone()));
            }
            StructuredFileOperation::Delete { path, .. } => {
                rendered.insert(repo.join(path), None);
            }
            StructuredFileOperation::Move { from, to, .. }
            | StructuredFileOperation::Rename { from, to, .. } => {
                let bytes = fs::read(repo.join(from))?;
                rendered.insert(repo.join(from), None);
                rendered.insert(repo.join(to), Some(bytes));
            }
        }
    }
    Ok(rendered)
}

fn load_snapshots(
    repo: &Path,
    plan: &StructuredEditPlan,
) -> Result<BTreeMap<PathBuf, FileSnapshot>, StructuredTransactionError> {
    let mut snapshots = BTreeMap::new();
    for path in plan.touched_paths() {
        let absolute = repo.join(&path);
        if !absolute.is_file() {
            continue;
        }
        let bytes = fs::read(&absolute)?;
        let content = String::from_utf8(bytes.clone()).map_err(|_| {
            StructuredTransactionError::InvalidJournal(format!(
                "structured edit target is not UTF-8: {}",
                path.display()
            ))
        })?;
        snapshots.insert(
            path,
            FileSnapshot {
                hash: hash(&bytes),
                version: None,
                content,
                symbols: BTreeSet::new(),
                ast_nodes: BTreeSet::new(),
            },
        );
    }
    Ok(snapshots)
}

fn verify_current(repo: &Path, entry: &JournalEntry) -> Result<(), StructuredTransactionError> {
    let current = fs::read(repo.join(&entry.path)).ok();
    let current_hash = current.as_deref().map(hash);
    if current_hash != entry.before_hash {
        return Err(StructuredTransactionError::ExternalModification {
            path: entry.path.clone(),
        });
    }
    Ok(())
}

fn apply_entry(
    repo: &Path,
    directory: &Path,
    entry: &JournalEntry,
) -> Result<(), StructuredTransactionError> {
    let destination = repo.join(&entry.path);
    match &entry.staged {
        Some(staged) => replace_file(&directory.join(staged), &destination)?,
        None if destination.exists() => fs::remove_file(destination)?,
        None => {}
    }
    Ok(())
}

fn rollback(
    repo: &Path,
    directory: &Path,
    journal: &mut StructuredJournal,
) -> Result<(), StructuredTransactionError> {
    journal.phase = StructuredTransactionPhase::RollingBack;
    persist_journal(directory, journal)?;
    for entry in journal.entries.iter().rev() {
        if !journal.applied_paths.contains(&entry.path) {
            continue;
        }
        let destination = repo.join(&entry.path);
        match &entry.backup {
            Some(backup) => replace_file(&directory.join(backup), &destination)?,
            None if destination.exists() => fs::remove_file(destination)?,
            None => {}
        }
    }
    journal.phase = StructuredTransactionPhase::RolledBack;
    persist_journal(directory, journal)
}

fn transaction_id(
    plan: &StructuredEditPlan,
    audit: &StructuredEditAudit,
) -> Result<String, StructuredTransactionError> {
    let payload = serde_json::to_vec(&(plan, audit))?;
    Ok(format!("{}-{}", plan.id, &hash(&payload)[..16]))
}

fn receipt(
    journal: &StructuredJournal,
) -> Result<StructuredTransactionReceipt, StructuredTransactionError> {
    let before_hashes = journal
        .entries
        .iter()
        .map(|entry| (entry.path.clone(), entry.before_hash.clone()))
        .collect();
    let after_hashes = journal
        .entries
        .iter()
        .map(|entry| (entry.path.clone(), entry.after_hash.clone()))
        .collect();
    Ok(StructuredTransactionReceipt {
        transaction_id: journal.transaction_id.clone(),
        plan_id: journal.plan_id.clone(),
        phase: journal.phase,
        touched_paths: journal.audit.touched_paths.clone(),
        before_hashes,
        after_hashes,
        audit: journal.audit.clone(),
    })
}

fn load_journal(directory: &Path) -> Result<StructuredJournal, StructuredTransactionError> {
    let journal: StructuredJournal =
        serde_json::from_slice(&fs::read(directory.join("journal.json"))?)?;
    if journal.schema != JOURNAL_SCHEMA {
        return Err(StructuredTransactionError::InvalidJournal(format!(
            "unsupported journal schema: {}",
            journal.schema
        )));
    }
    Ok(journal)
}

fn persist_journal(
    directory: &Path,
    journal: &StructuredJournal,
) -> Result<(), StructuredTransactionError> {
    let bytes = serde_json::to_vec_pretty(journal)?;
    let temporary = directory.join("journal.json.tmp");
    write_synced(&temporary, &bytes)?;
    fs::rename(temporary, directory.join("journal.json"))?;
    Ok(())
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), StructuredTransactionError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn replace_file(source: &Path, destination: &Path) -> Result<(), StructuredTransactionError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = destination.with_extension("medusa-structured-replace");
    write_synced(&temporary, &fs::read(source)?)?;
    fs::rename(temporary, destination)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EditMetadata, EditPreconditions, EditRange};

    fn text_plan(path: &str, expected: &str, replacement: &str) -> StructuredEditPlan {
        let mut plan = StructuredEditPlan::new("fixture");
        plan.add_text_edit(StructuredTextEdit {
            path: PathBuf::from(path),
            file_hash: None,
            file_version: None,
            range: EditRange {
                start_byte: 0,
                end_byte: expected.len(),
            },
            replacement: replacement.to_owned(),
            metadata: EditMetadata::default(),
            preconditions: EditPreconditions {
                expected_content: Some(expected.to_owned()),
                expected_symbol: None,
                expected_ast_node: None,
            },
        });
        plan
    }

    #[test]
    fn preview_matches_applied_content() {
        let repository = tempfile::tempdir().expect("repository");
        fs::write(repository.path().join("a.txt"), "old").expect("source");
        let transaction = StructuredTransaction::new(text_plan("a.txt", "old", "new"));
        let preview = transaction.preview(repository.path()).expect("preview");
        let receipt = transaction.commit(repository.path()).expect("commit");
        assert_eq!(preview, receipt.audit);
        assert_eq!(
            fs::read_to_string(repository.path().join("a.txt")).expect("result"),
            "new"
        );
    }

    #[test]
    fn injected_failure_recovers_without_partial_mutation() {
        let repository = tempfile::tempdir().expect("repository");
        fs::write(repository.path().join("a.txt"), "old").expect("source");
        let transaction = StructuredTransaction::new(text_plan("a.txt", "old", "new"))
            .with_failure_injection(FailureInjectionPoint::DuringApply(0));
        assert!(transaction.commit(repository.path()).is_err());
        let receipts = recover_structured_transactions(repository.path()).expect("recover");
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].phase, StructuredTransactionPhase::RolledBack);
        assert_eq!(
            fs::read_to_string(repository.path().join("a.txt")).expect("result"),
            "old"
        );
    }
}
