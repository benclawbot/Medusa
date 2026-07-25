use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    FileSnapshot, StructuredEditAudit, StructuredEditError, StructuredEditPlan,
    StructuredFileOperation, support::hash,
};

const JOURNAL_SCHEMA: u32 = 1;
const JOURNAL_ROOT: &str = ".medusa/structured-transactions";

/// Durable transaction lifecycle persisted before and after each mutation phase.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredTransactionState {
    Prepared,
    Staged,
    Applying,
    Applied,
    Verifying,
    Committed,
    RollingBack,
    RolledBack,
}

impl StructuredTransactionState {
    fn terminal(self) -> bool {
        matches!(self, Self::Committed | Self::RolledBack)
    }
}

/// Test-only and diagnostic failure injection points covering every transaction phase.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionFailurePoint {
    AfterPrepare,
    AfterStage,
    DuringApply,
    AfterApply,
    DuringVerification,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct JournalPath {
    path: PathBuf,
    existed_before: bool,
    before_hash: Option<String>,
    after_hash: Option<String>,
    backup_file: Option<String>,
    staged_file: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StructuredTransactionJournal {
    schema: u32,
    transaction_id: String,
    state: StructuredTransactionState,
    plan: StructuredEditPlan,
    audit: StructuredEditAudit,
    paths: Vec<JournalPath>,
    applied_paths: BTreeSet<PathBuf>,
}

/// Evidence emitted after an all-or-nothing structured edit transaction commits.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StructuredTransactionReceipt {
    pub transaction_id: String,
    pub changed_paths: Vec<PathBuf>,
    pub before_hashes: BTreeMap<PathBuf, Option<String>>,
    pub after_hashes: BTreeMap<PathBuf, Option<String>>,
    pub audit: StructuredEditAudit,
    pub state: StructuredTransactionState,
}

/// Typed transaction failures, including validation, stale workspace, I/O, and injection.
#[derive(Debug)]
pub enum StructuredTransactionError {
    Validation(Vec<StructuredEditError>),
    Io(std::io::Error),
    Serialization(serde_json::Error),
    StaleWorkspace {
        path: PathBuf,
        expected: String,
        actual: Option<String>,
    },
    VerificationMismatch {
        path: PathBuf,
        expected: Option<String>,
        actual: Option<String>,
    },
    InvalidPlan(String),
    Injected(TransactionFailurePoint),
}

impl std::fmt::Display for StructuredTransactionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Validation(errors) => {
                write!(formatter, "structured edit validation failed: {errors:?}")
            }
            Self::Io(error) => write!(formatter, "structured transaction I/O error: {error}"),
            Self::Serialization(error) => write!(
                formatter,
                "structured transaction serialization error: {error}"
            ),
            Self::StaleWorkspace {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "workspace changed before apply for {}: expected {expected}, found {actual:?}",
                path.display()
            ),
            Self::VerificationMismatch {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "post-apply verification mismatch for {}: expected {expected:?}, found {actual:?}",
                path.display()
            ),
            Self::InvalidPlan(message) => formatter.write_str(message),
            Self::Injected(point) => write!(
                formatter,
                "injected structured transaction failure at {point:?}"
            ),
        }
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

/// Applies a validated plan atomically, rolling every path back on any failure.
pub fn apply_structured_transaction(
    repo: &Path,
    mut plan: StructuredEditPlan,
    snapshots: &BTreeMap<PathBuf, FileSnapshot>,
    failure: Option<TransactionFailurePoint>,
) -> Result<StructuredTransactionReceipt, StructuredTransactionError> {
    plan.normalize();
    if plan.id.trim().is_empty() {
        return Err(StructuredTransactionError::InvalidPlan(
            "structured transaction plan id cannot be empty".to_owned(),
        ));
    }
    let audit = plan
        .validate(snapshots)
        .map_err(StructuredTransactionError::Validation)?;
    verify_workspace_matches_snapshots(repo, snapshots, &plan.touched_paths())?;

    let directory = repo.join(JOURNAL_ROOT).join(&plan.id);
    if directory.exists() {
        return Err(StructuredTransactionError::InvalidPlan(format!(
            "structured transaction already exists: {}",
            plan.id
        )));
    }
    fs::create_dir_all(&directory)?;

    let final_files = render_final_workspace(repo, &plan)?;
    let mut journal = prepare_journal(repo, plan, audit, final_files, &directory)?;
    persist_journal(&directory, &journal)?;
    inject(failure, TransactionFailurePoint::AfterPrepare)?;

    stage_files(repo, &directory, &mut journal)?;
    journal.state = StructuredTransactionState::Staged;
    persist_journal(&directory, &journal)?;
    inject(failure, TransactionFailurePoint::AfterStage)?;

    journal.state = StructuredTransactionState::Applying;
    persist_journal(&directory, &journal)?;
    let apply_result = apply_staged(repo, &directory, &mut journal, failure);
    if let Err(error) = apply_result {
        rollback(repo, &directory, &mut journal)?;
        return Err(error);
    }

    journal.state = StructuredTransactionState::Applied;
    persist_journal(&directory, &journal)?;
    if let Err(error) = inject(failure, TransactionFailurePoint::AfterApply) {
        rollback(repo, &directory, &mut journal)?;
        return Err(error);
    }

    journal.state = StructuredTransactionState::Verifying;
    persist_journal(&directory, &journal)?;
    if let Err(error) = inject(failure, TransactionFailurePoint::DuringVerification)
        .and_then(|_| verify_applied(repo, &journal))
    {
        rollback(repo, &directory, &mut journal)?;
        return Err(error);
    }

    journal.state = StructuredTransactionState::Committed;
    persist_journal(&directory, &journal)?;
    Ok(receipt(&journal))
}

/// Recovers interrupted transactions deterministically to committed or rolled-back state.
pub fn recover_structured_transactions(
    repo: &Path,
) -> Result<Vec<StructuredTransactionReceipt>, StructuredTransactionError> {
    let root = repo.join(JOURNAL_ROOT);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut directories = fs::read_dir(&root)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    directories.sort();

    let mut recovered = Vec::new();
    for directory in directories {
        let mut journal = load_journal(&directory)?;
        if journal.state.terminal() {
            continue;
        }
        if journal.state == StructuredTransactionState::Verifying
            && verify_applied(repo, &journal).is_ok()
        {
            journal.state = StructuredTransactionState::Committed;
            persist_journal(&directory, &journal)?;
        } else {
            rollback(repo, &directory, &mut journal)?;
        }
        recovered.push(receipt(&journal));
    }
    Ok(recovered)
}

fn verify_workspace_matches_snapshots(
    repo: &Path,
    snapshots: &BTreeMap<PathBuf, FileSnapshot>,
    touched: &[PathBuf],
) -> Result<(), StructuredTransactionError> {
    for path in touched {
        let Some(snapshot) = snapshots.get(path) else {
            continue;
        };
        let actual = file_hash(&repo.join(path))?;
        if actual.as_deref() != Some(snapshot.hash.as_str()) {
            return Err(StructuredTransactionError::StaleWorkspace {
                path: path.clone(),
                expected: snapshot.hash.clone(),
                actual,
            });
        }
    }
    Ok(())
}

fn render_final_workspace(
    repo: &Path,
    plan: &StructuredEditPlan,
) -> Result<BTreeMap<PathBuf, Option<Vec<u8>>>, StructuredTransactionError> {
    let mut result = BTreeMap::new();
    for path in plan.touched_paths() {
        let absolute = repo.join(&path);
        result.insert(
            path,
            absolute.is_file().then(|| fs::read(absolute)).transpose()?,
        );
    }

    let mut grouped = BTreeMap::<PathBuf, Vec<_>>::new();
    for edit in &plan.text_edits {
        grouped.entry(edit.path.clone()).or_default().push(edit);
    }
    for (path, mut edits) in grouped {
        let bytes = result.get(&path).and_then(Option::as_ref).ok_or_else(|| {
            StructuredTransactionError::InvalidPlan(format!(
                "text edit target does not exist: {}",
                path.display()
            ))
        })?;
        let mut text = String::from_utf8(bytes.clone()).map_err(|_| {
            StructuredTransactionError::InvalidPlan(format!(
                "text edit target is not UTF-8: {}",
                path.display()
            ))
        })?;
        edits.sort_by_key(|edit| edit.range);
        for edit in edits.into_iter().rev() {
            text.replace_range(
                edit.range.start_byte..edit.range.end_byte,
                &edit.replacement,
            );
        }
        result.insert(path, Some(text.into_bytes()));
    }

    for operation in &plan.file_operations {
        match operation {
            StructuredFileOperation::Create {
                path,
                content,
                overwrite,
                ..
            } => {
                if result.get(path).is_some_and(Option::is_some) && !overwrite {
                    return Err(StructuredTransactionError::InvalidPlan(format!(
                        "create target exists: {}",
                        path.display()
                    )));
                }
                result.insert(path.clone(), Some(content.clone()));
            }
            StructuredFileOperation::Delete { path, .. } => {
                result.insert(path.clone(), None);
            }
            StructuredFileOperation::Move {
                from,
                to,
                overwrite,
                ..
            }
            | StructuredFileOperation::Rename {
                from,
                to,
                overwrite,
                ..
            } => {
                if result.get(to).is_some_and(Option::is_some) && !overwrite {
                    return Err(StructuredTransactionError::InvalidPlan(format!(
                        "move target exists: {}",
                        to.display()
                    )));
                }
                let content = result.get(from).cloned().flatten().ok_or_else(|| {
                    StructuredTransactionError::InvalidPlan(format!(
                        "move source does not exist: {}",
                        from.display()
                    ))
                })?;
                result.insert(from.clone(), None);
                result.insert(to.clone(), Some(content));
            }
        }
    }
    Ok(result)
}

fn prepare_journal(
    repo: &Path,
    plan: StructuredEditPlan,
    audit: StructuredEditAudit,
    final_files: BTreeMap<PathBuf, Option<Vec<u8>>>,
    directory: &Path,
) -> Result<StructuredTransactionJournal, StructuredTransactionError> {
    let mut paths = Vec::new();
    for (index, (path, final_content)) in final_files.into_iter().enumerate() {
        let absolute = repo.join(&path);
        let before = absolute
            .is_file()
            .then(|| fs::read(&absolute))
            .transpose()?;
        let backup_file = before.as_ref().map(|_| format!("{index}.before"));
        let staged_file = final_content.as_ref().map(|_| format!("{index}.after"));
        if let (Some(name), Some(bytes)) = (&backup_file, &before) {
            write_synced(&directory.join(name), bytes)?;
        }
        if let (Some(name), Some(bytes)) = (&staged_file, &final_content) {
            write_synced(&directory.join(name), bytes)?;
        }
        paths.push(JournalPath {
            path,
            existed_before: before.is_some(),
            before_hash: before.as_ref().map(|bytes| hash(bytes)),
            after_hash: final_content.as_ref().map(|bytes| hash(bytes)),
            backup_file,
            staged_file,
        });
    }
    Ok(StructuredTransactionJournal {
        schema: JOURNAL_SCHEMA,
        transaction_id: plan.id.clone(),
        state: StructuredTransactionState::Prepared,
        plan,
        audit,
        paths,
        applied_paths: BTreeSet::new(),
    })
}

fn stage_files(
    repo: &Path,
    directory: &Path,
    journal: &mut StructuredTransactionJournal,
) -> Result<(), StructuredTransactionError> {
    for entry in &journal.paths {
        let actual = file_hash(&repo.join(&entry.path))?;
        if actual != entry.before_hash {
            return Err(StructuredTransactionError::StaleWorkspace {
                path: entry.path.clone(),
                expected: entry
                    .before_hash
                    .clone()
                    .unwrap_or_else(|| "<missing>".to_owned()),
                actual,
            });
        }
        if let Some(staged) = &entry.staged_file {
            let bytes = fs::read(directory.join(staged))?;
            if Some(hash(&bytes)) != entry.after_hash {
                return Err(StructuredTransactionError::InvalidPlan(format!(
                    "staged content hash mismatch for {}",
                    entry.path.display()
                )));
            }
        }
    }
    Ok(())
}

fn apply_staged(
    repo: &Path,
    directory: &Path,
    journal: &mut StructuredTransactionJournal,
    failure: Option<TransactionFailurePoint>,
) -> Result<(), StructuredTransactionError> {
    for (index, entry) in journal.paths.clone().into_iter().enumerate() {
        let destination = repo.join(&entry.path);
        if let Some(staged) = &entry.staged_file {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            replace_file(&directory.join(staged), &destination)?;
        } else if destination.exists() {
            fs::remove_file(&destination)?;
            if let Some(parent) = destination.parent() {
                sync_directory(parent)?;
            }
        }
        journal.applied_paths.insert(entry.path.clone());
        persist_journal(directory, journal)?;
        if index == 0 {
            inject(failure, TransactionFailurePoint::DuringApply)?;
        }
    }
    Ok(())
}

fn verify_applied(
    repo: &Path,
    journal: &StructuredTransactionJournal,
) -> Result<(), StructuredTransactionError> {
    for entry in &journal.paths {
        let actual = file_hash(&repo.join(&entry.path))?;
        if actual != entry.after_hash {
            return Err(StructuredTransactionError::VerificationMismatch {
                path: entry.path.clone(),
                expected: entry.after_hash.clone(),
                actual,
            });
        }
    }
    Ok(())
}

fn rollback(
    repo: &Path,
    directory: &Path,
    journal: &mut StructuredTransactionJournal,
) -> Result<(), StructuredTransactionError> {
    journal.state = StructuredTransactionState::RollingBack;
    persist_journal(directory, journal)?;
    for entry in journal.paths.iter().rev() {
        if !journal.applied_paths.contains(&entry.path) {
            continue;
        }
        let destination = repo.join(&entry.path);
        if let Some(backup) = &entry.backup_file {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            replace_file(&directory.join(backup), &destination)?;
        } else if destination.exists() {
            fs::remove_file(&destination)?;
        }
        let actual = file_hash(&destination)?;
        if actual != entry.before_hash {
            return Err(StructuredTransactionError::VerificationMismatch {
                path: entry.path.clone(),
                expected: entry.before_hash.clone(),
                actual,
            });
        }
    }
    journal.state = StructuredTransactionState::RolledBack;
    persist_journal(directory, journal)
}

fn receipt(journal: &StructuredTransactionJournal) -> StructuredTransactionReceipt {
    StructuredTransactionReceipt {
        transaction_id: journal.transaction_id.clone(),
        changed_paths: journal
            .paths
            .iter()
            .map(|entry| entry.path.clone())
            .collect(),
        before_hashes: journal
            .paths
            .iter()
            .map(|entry| (entry.path.clone(), entry.before_hash.clone()))
            .collect(),
        after_hashes: journal
            .paths
            .iter()
            .map(|entry| (entry.path.clone(), entry.after_hash.clone()))
            .collect(),
        audit: journal.audit.clone(),
        state: journal.state,
    }
}

fn inject(
    configured: Option<TransactionFailurePoint>,
    current: TransactionFailurePoint,
) -> Result<(), StructuredTransactionError> {
    if configured == Some(current) {
        Err(StructuredTransactionError::Injected(current))
    } else {
        Ok(())
    }
}

fn load_journal(
    directory: &Path,
) -> Result<StructuredTransactionJournal, StructuredTransactionError> {
    let journal: StructuredTransactionJournal =
        serde_json::from_slice(&fs::read(directory.join("journal.json"))?)?;
    if journal.schema != JOURNAL_SCHEMA {
        return Err(StructuredTransactionError::InvalidPlan(format!(
            "unsupported structured transaction journal schema: {}",
            journal.schema
        )));
    }
    Ok(journal)
}

fn persist_journal(
    directory: &Path,
    journal: &StructuredTransactionJournal,
) -> Result<(), StructuredTransactionError> {
    let temporary = directory.join("journal.json.tmp");
    write_synced(&temporary, &serde_json::to_vec_pretty(journal)?)?;
    fs::rename(&temporary, directory.join("journal.json"))?;
    sync_directory(directory)
}

fn file_hash(path: &Path) -> Result<Option<String>, StructuredTransactionError> {
    Ok(path
        .is_file()
        .then(|| fs::read(path))
        .transpose()?
        .map(|bytes| hash(&bytes)))
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), StructuredTransactionError> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn replace_file(source: &Path, destination: &Path) -> Result<(), StructuredTransactionError> {
    let temporary = destination.with_extension("medusa-structured-replace");
    write_synced(&temporary, &fs::read(source)?)?;
    fs::rename(&temporary, destination)?;
    if let Some(parent) = destination.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), StructuredTransactionError> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EditMetadata, EditPreconditions, EditRange, StructuredTextEdit};

    fn snapshot(content: &str) -> FileSnapshot {
        FileSnapshot {
            hash: hash(content.as_bytes()),
            version: Some(1),
            content: content.to_owned(),
            symbols: BTreeSet::new(),
            ast_nodes: BTreeSet::new(),
        }
    }

    fn two_file_plan() -> (StructuredEditPlan, BTreeMap<PathBuf, FileSnapshot>) {
        let mut plan = StructuredEditPlan::new("atomic-two-file");
        for path in ["a.txt", "b.txt"] {
            plan.add_text_edit(StructuredTextEdit {
                path: path.into(),
                file_hash: Some(hash(b"old")),
                file_version: Some(1),
                range: EditRange {
                    start_byte: 0,
                    end_byte: 3,
                },
                replacement: "new".to_owned(),
                metadata: EditMetadata {
                    intent: "test".to_owned(),
                    provenance: "unit".to_owned(),
                    annotation: None,
                },
                preconditions: EditPreconditions {
                    expected_content: Some("old".to_owned()),
                    ..EditPreconditions::default()
                },
            });
        }
        let snapshots = [
            (PathBuf::from("a.txt"), snapshot("old")),
            (PathBuf::from("b.txt"), snapshot("old")),
        ]
        .into_iter()
        .collect();
        (plan, snapshots)
    }

    #[test]
    fn commits_multi_file_plan_and_matches_preview() {
        let repo = tempfile::tempdir().expect("repo");
        fs::write(repo.path().join("a.txt"), "old").expect("a");
        fs::write(repo.path().join("b.txt"), "old").expect("b");
        let (plan, snapshots) = two_file_plan();
        let receipt =
            apply_structured_transaction(repo.path(), plan, &snapshots, None).expect("commit");
        assert_eq!(receipt.state, StructuredTransactionState::Committed);
        assert_eq!(
            fs::read_to_string(repo.path().join("a.txt")).expect("a"),
            "new"
        );
        assert!(
            receipt
                .audit
                .previews
                .iter()
                .all(|preview| preview.after == "new")
        );
    }

    #[test]
    fn injected_failure_leaves_no_partial_mutation() {
        let repo = tempfile::tempdir().expect("repo");
        fs::write(repo.path().join("a.txt"), "old").expect("a");
        fs::write(repo.path().join("b.txt"), "old").expect("b");
        let (plan, snapshots) = two_file_plan();
        let result = apply_structured_transaction(
            repo.path(),
            plan,
            &snapshots,
            Some(TransactionFailurePoint::DuringApply),
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(repo.path().join("a.txt")).expect("a"),
            "old"
        );
        assert_eq!(
            fs::read_to_string(repo.path().join("b.txt")).expect("b"),
            "old"
        );
    }

    #[test]
    fn stale_external_change_aborts_before_mutation() {
        let repo = tempfile::tempdir().expect("repo");
        fs::write(repo.path().join("a.txt"), "changed").expect("a");
        fs::write(repo.path().join("b.txt"), "old").expect("b");
        let (plan, snapshots) = two_file_plan();
        let result = apply_structured_transaction(repo.path(), plan, &snapshots, None);
        assert!(matches!(
            result,
            Err(StructuredTransactionError::StaleWorkspace { .. })
        ));
        assert_eq!(
            fs::read_to_string(repo.path().join("b.txt")).expect("b"),
            "old"
        );
    }

    #[test]
    fn failure_points_roll_back_or_abort_safely() {
        for point in [
            TransactionFailurePoint::AfterPrepare,
            TransactionFailurePoint::AfterStage,
            TransactionFailurePoint::DuringApply,
            TransactionFailurePoint::AfterApply,
            TransactionFailurePoint::DuringVerification,
        ] {
            let repo = tempfile::tempdir().expect("repo");
            fs::write(repo.path().join("a.txt"), "old").expect("a");
            fs::write(repo.path().join("b.txt"), "old").expect("b");
            let (mut plan, snapshots) = two_file_plan();
            plan.id = format!("failure-{point:?}");
            assert!(
                apply_structured_transaction(repo.path(), plan, &snapshots, Some(point)).is_err()
            );
            assert_eq!(
                fs::read_to_string(repo.path().join("a.txt")).expect("a"),
                "old"
            );
            assert_eq!(
                fs::read_to_string(repo.path().join("b.txt")).expect("b"),
                "old"
            );
        }
    }
}
