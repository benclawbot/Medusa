use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{patch::TextEdit, support::hash};

/// Stable schema version for serialized structured edit plans.
pub const STRUCTURED_EDIT_SCHEMA: u32 = 1;

/// A source byte range using half-open offsets.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct EditRange {
    pub start_byte: usize,
    pub end_byte: usize,
}

/// Why an edit exists.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditIntent {
    Refactor,
    Fix,
    Generate,
    Format,
    Rename,
    CodeAction,
    Other(String),
}

/// Where an edit originated.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct EditProvenance {
    pub producer: String,
    pub operation: String,
    pub request_id: Option<String>,
}

/// Preconditions evaluated before mutation.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EditPrecondition {
    FileHash { expected: String },
    ExpectedContent { range: EditRange, expected: String },
    ExpectedSymbol { symbol_id: String },
    ExpectedAstNode { node_id: String, kind: Option<String> },
    PathAbsent,
    PathPresent,
}

/// A precise text replacement that is inert until applied by a transaction engine.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct StructuredTextEdit {
    pub path: PathBuf,
    pub range: EditRange,
    pub replacement: String,
    pub intent: EditIntent,
    pub provenance: EditProvenance,
    pub preconditions: Vec<EditPrecondition>,
}

/// Language-neutral file operations.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum FileOperation {
    Create {
        path: PathBuf,
        content: String,
        provenance: EditProvenance,
        preconditions: Vec<EditPrecondition>,
    },
    Delete {
        path: PathBuf,
        provenance: EditProvenance,
        preconditions: Vec<EditPrecondition>,
    },
    Move {
        from: PathBuf,
        to: PathBuf,
        provenance: EditProvenance,
        preconditions: Vec<EditPrecondition>,
    },
}

/// A deterministic, reviewable multi-file change plan.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StructuredEditPlan {
    pub schema: u32,
    pub plan_id: String,
    pub text_edits: Vec<StructuredTextEdit>,
    pub file_operations: Vec<FileOperation>,
    pub metadata: BTreeMap<String, String>,
}

/// Typed validation failure produced before any mutation occurs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StructuredEditError {
    EmptyPlan,
    InvalidRange {
        path: PathBuf,
        range: EditRange,
    },
    Overlap {
        path: PathBuf,
        first: EditRange,
        second: EditRange,
    },
    ConflictingFileOperation {
        path: PathBuf,
    },
    TextEditConflictsWithFileOperation {
        path: PathBuf,
    },
    StaleFileHash {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    ExpectedContentMismatch {
        path: PathBuf,
        range: EditRange,
        expected: String,
        actual: String,
    },
    MissingPath {
        path: PathBuf,
    },
    UnexpectedPath {
        path: PathBuf,
    },
}

/// Snapshot used to validate a plan without touching the working tree.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct EditSnapshot {
    pub files: BTreeMap<PathBuf, String>,
}

/// Machine-readable evidence for review and audit systems.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StructuredEditAudit {
    pub schema: u32,
    pub plan_id: String,
    pub touched_paths: Vec<PathBuf>,
    pub text_edit_count: usize,
    pub file_operation_count: usize,
    pub deterministic_digest: String,
}

impl StructuredEditPlan {
    #[must_use]
    pub fn new(text_edits: Vec<StructuredTextEdit>, file_operations: Vec<FileOperation>) -> Self {
        let mut plan = Self {
            schema: STRUCTURED_EDIT_SCHEMA,
            plan_id: String::new(),
            text_edits,
            file_operations,
            metadata: BTreeMap::new(),
        };
        plan.normalize();
        plan.plan_id = plan.digest();
        plan
    }

    /// Converts existing patch edits into the structured model.
    #[must_use]
    pub fn from_text_edits(edits: impl IntoIterator<Item = TextEdit>) -> Self {
        let provenance = EditProvenance {
            producer: "medusa-intelligence".to_owned(),
            operation: "patch_transaction".to_owned(),
            request_id: None,
        };
        let text_edits = edits
            .into_iter()
            .map(|edit| StructuredTextEdit {
                path: edit.path,
                range: EditRange {
                    start_byte: edit.start_byte,
                    end_byte: edit.end_byte,
                },
                replacement: edit.replacement,
                intent: EditIntent::Refactor,
                provenance: provenance.clone(),
                preconditions: vec![EditPrecondition::ExpectedContent {
                    range: EditRange {
                        start_byte: edit.start_byte,
                        end_byte: edit.end_byte,
                    },
                    expected: edit.expected,
                }],
            })
            .collect();
        Self::new(text_edits, Vec::new())
    }

    /// Sorts all operations into stable serialization and application order.
    pub fn normalize(&mut self) {
        self.text_edits.sort();
        self.file_operations.sort();
        for edit in &mut self.text_edits {
            edit.preconditions.sort();
            edit.preconditions.dedup();
        }
        for operation in &mut self.file_operations {
            match operation {
                FileOperation::Create { preconditions, .. }
                | FileOperation::Delete { preconditions, .. }
                | FileOperation::Move { preconditions, .. } => {
                    preconditions.sort();
                    preconditions.dedup();
                }
            }
        }
    }

    /// Detects structural conflicts before any file-system access.
    pub fn validate_structure(&self) -> Result<(), StructuredEditError> {
        if self.text_edits.is_empty() && self.file_operations.is_empty() {
            return Err(StructuredEditError::EmptyPlan);
        }

        let mut grouped: BTreeMap<&Path, Vec<EditRange>> = BTreeMap::new();
        for edit in &self.text_edits {
            if edit.range.start_byte > edit.range.end_byte {
                return Err(StructuredEditError::InvalidRange {
                    path: edit.path.clone(),
                    range: edit.range,
                });
            }
            grouped.entry(&edit.path).or_default().push(edit.range);
        }
        for (path, ranges) in &mut grouped {
            ranges.sort();
            for pair in ranges.windows(2) {
                if pair[0].end_byte > pair[1].start_byte {
                    return Err(StructuredEditError::Overlap {
                        path: (*path).to_path_buf(),
                        first: pair[0],
                        second: pair[1],
                    });
                }
            }
        }

        let mut operation_paths = BTreeSet::new();
        for operation in &self.file_operations {
            for path in operation.paths() {
                if !operation_paths.insert(path.to_path_buf()) {
                    return Err(StructuredEditError::ConflictingFileOperation {
                        path: path.to_path_buf(),
                    });
                }
            }
        }
        for path in grouped.keys() {
            if operation_paths.contains(*path) {
                return Err(StructuredEditError::TextEditConflictsWithFileOperation {
                    path: (*path).to_path_buf(),
                });
            }
        }
        Ok(())
    }

    /// Evaluates file hash/content/path preconditions against an immutable snapshot.
    pub fn validate_snapshot(&self, snapshot: &EditSnapshot) -> Result<(), StructuredEditError> {
        self.validate_structure()?;
        for edit in &self.text_edits {
            let content = snapshot
                .files
                .get(&edit.path)
                .ok_or_else(|| StructuredEditError::MissingPath {
                    path: edit.path.clone(),
                })?;
            validate_preconditions(&edit.path, content, &edit.preconditions, snapshot)?;
        }
        for operation in &self.file_operations {
            operation.validate(snapshot)?;
        }
        Ok(())
    }

    /// Human-readable review preview. It intentionally does not mutate files.
    #[must_use]
    pub fn preview(&self) -> String {
        let mut lines = vec![format!("Structured edit plan {}", self.plan_id)];
        for edit in &self.text_edits {
            lines.push(format!(
                "EDIT {} [{}..{}] {:?}: {} bytes",
                edit.path.display(),
                edit.range.start_byte,
                edit.range.end_byte,
                edit.intent,
                edit.replacement.len()
            ));
        }
        for operation in &self.file_operations {
            lines.push(operation.preview());
        }
        lines.join("\n")
    }

    #[must_use]
    pub fn touched_paths(&self) -> Vec<PathBuf> {
        let mut paths = BTreeSet::new();
        paths.extend(self.text_edits.iter().map(|edit| edit.path.clone()));
        for operation in &self.file_operations {
            paths.extend(operation.paths().into_iter().map(Path::to_path_buf));
        }
        paths.into_iter().collect()
    }

    #[must_use]
    pub fn audit(&self) -> StructuredEditAudit {
        StructuredEditAudit {
            schema: self.schema,
            plan_id: self.plan_id.clone(),
            touched_paths: self.touched_paths(),
            text_edit_count: self.text_edits.len(),
            file_operation_count: self.file_operations.len(),
            deterministic_digest: self.digest(),
        }
    }

    fn digest(&self) -> String {
        let payload = serde_json::to_vec(&(
            self.schema,
            &self.text_edits,
            &self.file_operations,
            &self.metadata,
        ))
        .expect("structured edit serialization is infallible");
        hash(&payload)
    }
}

impl FileOperation {
    fn paths(&self) -> Vec<&Path> {
        match self {
            Self::Create { path, .. } | Self::Delete { path, .. } => vec![path],
            Self::Move { from, to, .. } => vec![from, to],
        }
    }

    fn preview(&self) -> String {
        match self {
            Self::Create { path, content, .. } => {
                format!("CREATE {}: {} bytes", path.display(), content.len())
            }
            Self::Delete { path, .. } => format!("DELETE {}", path.display()),
            Self::Move { from, to, .. } => {
                format!("MOVE {} -> {}", from.display(), to.display())
            }
        }
    }

    fn validate(&self, snapshot: &EditSnapshot) -> Result<(), StructuredEditError> {
        match self {
            Self::Create {
                path,
                preconditions,
                ..
            } => {
                if snapshot.files.contains_key(path) {
                    return Err(StructuredEditError::UnexpectedPath { path: path.clone() });
                }
                validate_preconditions(path, "", preconditions, snapshot)
            }
            Self::Delete {
                path,
                preconditions,
                ..
            } => {
                let content = snapshot
                    .files
                    .get(path)
                    .ok_or_else(|| StructuredEditError::MissingPath { path: path.clone() })?;
                validate_preconditions(path, content, preconditions, snapshot)
            }
            Self::Move {
                from,
                to,
                preconditions,
                ..
            } => {
                let content = snapshot
                    .files
                    .get(from)
                    .ok_or_else(|| StructuredEditError::MissingPath { path: from.clone() })?;
                if snapshot.files.contains_key(to) {
                    return Err(StructuredEditError::UnexpectedPath { path: to.clone() });
                }
                validate_preconditions(from, content, preconditions, snapshot)
            }
        }
    }
}

fn validate_preconditions(
    path: &Path,
    content: &str,
    preconditions: &[EditPrecondition],
    snapshot: &EditSnapshot,
) -> Result<(), StructuredEditError> {
    for precondition in preconditions {
        match precondition {
            EditPrecondition::FileHash { expected } => {
                let actual = hash(content.as_bytes());
                if &actual != expected {
                    return Err(StructuredEditError::StaleFileHash {
                        path: path.to_path_buf(),
                        expected: expected.clone(),
                        actual,
                    });
                }
            }
            EditPrecondition::ExpectedContent { range, expected } => {
                let actual = content
                    .get(range.start_byte..range.end_byte)
                    .unwrap_or_default()
                    .to_owned();
                if &actual != expected {
                    return Err(StructuredEditError::ExpectedContentMismatch {
                        path: path.to_path_buf(),
                        range: *range,
                        expected: expected.clone(),
                        actual,
                    });
                }
            }
            EditPrecondition::PathAbsent => {
                if snapshot.files.contains_key(path) {
                    return Err(StructuredEditError::UnexpectedPath {
                        path: path.to_path_buf(),
                    });
                }
            }
            EditPrecondition::PathPresent => {
                if !snapshot.files.contains_key(path) {
                    return Err(StructuredEditError::MissingPath {
                        path: path.to_path_buf(),
                    });
                }
            }
            EditPrecondition::ExpectedSymbol { .. }
            | EditPrecondition::ExpectedAstNode { .. } => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance() -> EditProvenance {
        EditProvenance {
            producer: "test".to_owned(),
            operation: "fixture".to_owned(),
            request_id: Some("request-1".to_owned()),
        }
    }

    #[test]
    fn detects_overlapping_edits_before_application() {
        let plan = StructuredEditPlan::new(
            vec![
                StructuredTextEdit {
                    path: "src/lib.rs".into(),
                    range: EditRange { start_byte: 0, end_byte: 4 },
                    replacement: "one".to_owned(),
                    intent: EditIntent::Refactor,
                    provenance: provenance(),
                    preconditions: Vec::new(),
                },
                StructuredTextEdit {
                    path: "src/lib.rs".into(),
                    range: EditRange { start_byte: 3, end_byte: 7 },
                    replacement: "two".to_owned(),
                    intent: EditIntent::Refactor,
                    provenance: provenance(),
                    preconditions: Vec::new(),
                },
            ],
            Vec::new(),
        );
        assert!(matches!(
            plan.validate_structure(),
            Err(StructuredEditError::Overlap { .. })
        ));
    }

    #[test]
    fn reports_stale_hash_as_typed_failure() {
        let plan = StructuredEditPlan::new(
            vec![StructuredTextEdit {
                path: "src/lib.rs".into(),
                range: EditRange { start_byte: 0, end_byte: 2 },
                replacement: "ok".to_owned(),
                intent: EditIntent::Fix,
                provenance: provenance(),
                preconditions: vec![EditPrecondition::FileHash {
                    expected: "stale".to_owned(),
                }],
            }],
            Vec::new(),
        );
        let snapshot = EditSnapshot {
            files: BTreeMap::from([("src/lib.rs".into(), "fn main() {}".to_owned())]),
        };
        assert!(matches!(
            plan.validate_snapshot(&snapshot),
            Err(StructuredEditError::StaleFileHash { .. })
        ));
    }

    #[test]
    fn serialization_and_audit_are_deterministic() {
        let plan = StructuredEditPlan::new(
            vec![StructuredTextEdit {
                path: "src/lib.rs".into(),
                range: EditRange { start_byte: 0, end_byte: 2 },
                replacement: "pub".to_owned(),
                intent: EditIntent::Refactor,
                provenance: provenance(),
                preconditions: Vec::new(),
            }],
            Vec::new(),
        );
        let first = serde_json::to_vec(&plan).expect("serialize");
        let decoded: StructuredEditPlan = serde_json::from_slice(&first).expect("deserialize");
        let second = serde_json::to_vec(&decoded).expect("serialize");
        assert_eq!(first, second);
        assert_eq!(plan.audit().deterministic_digest, decoded.audit().deterministic_digest);
    }

    #[test]
    fn adapts_existing_text_patch_workflow() {
        let plan = StructuredEditPlan::from_text_edits([TextEdit {
            path: "src/lib.rs".into(),
            start_byte: 0,
            end_byte: 3,
            expected: "old".to_owned(),
            replacement: "new".to_owned(),
        }]);
        assert_eq!(plan.text_edits.len(), 1);
        assert!(plan.validate_structure().is_ok());
        assert!(plan.preview().contains("EDIT src/lib.rs [0..3]"));
    }
}
