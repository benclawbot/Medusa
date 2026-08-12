use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::TextEdit;

/// Stable byte range within one UTF-8 file revision.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct EditRange {
    pub start_byte: usize,
    pub end_byte: usize,
}

impl EditRange {
    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        self.start_byte < other.end_byte && other.start_byte < self.end_byte
    }
}

/// Human and machine readable reason for an edit.
#[derive(Clone, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct EditMetadata {
    pub intent: String,
    pub provenance: String,
    pub annotation: Option<String>,
}

/// Optional semantic guards attached to a text edit.
#[derive(Clone, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct EditPreconditions {
    pub expected_content: Option<String>,
    pub expected_symbol: Option<String>,
    pub expected_ast_node: Option<String>,
}

/// One source replacement that does not mutate the workspace by itself.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct StructuredTextEdit {
    pub path: PathBuf,
    pub file_hash: Option<String>,
    pub file_version: Option<u64>,
    pub range: EditRange,
    pub replacement: String,
    pub metadata: EditMetadata,
    pub preconditions: EditPreconditions,
}

/// Language-neutral file-system operations represented in a reviewable plan.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredFileOperation {
    Create {
        path: PathBuf,
        content: Vec<u8>,
        overwrite: bool,
        metadata: EditMetadata,
    },
    Delete {
        path: PathBuf,
        expected_hash: Option<String>,
        metadata: EditMetadata,
    },
    Move {
        from: PathBuf,
        to: PathBuf,
        expected_hash: Option<String>,
        overwrite: bool,
        metadata: EditMetadata,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
        expected_hash: Option<String>,
        overwrite: bool,
        metadata: EditMetadata,
    },
}

/// Complete non-mutating edit proposal spanning any number of files.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct StructuredEditPlan {
    pub schema_version: u32,
    pub id: String,
    pub text_edits: Vec<StructuredTextEdit>,
    pub file_operations: Vec<StructuredFileOperation>,
}

/// Current workspace evidence used to validate preconditions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileSnapshot {
    pub hash: String,
    pub version: Option<u64>,
    pub content: String,
    pub symbols: BTreeSet<String>,
    pub ast_nodes: BTreeSet<String>,
}

/// Typed validation failures produced before any application step.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredEditError {
    InvalidRange {
        path: PathBuf,
        range: EditRange,
    },
    OverlappingEdits {
        path: PathBuf,
        first: EditRange,
        second: EditRange,
    },
    MissingSnapshot {
        path: PathBuf,
    },
    StaleHash {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    StaleVersion {
        path: PathBuf,
        expected: u64,
        actual: Option<u64>,
    },
    ExpectedContentMismatch {
        path: PathBuf,
        range: EditRange,
        expected: String,
        actual: String,
    },
    ExpectedSymbolMissing {
        path: PathBuf,
        symbol: String,
    },
    ExpectedAstNodeMissing {
        path: PathBuf,
        node: String,
    },
    ConflictingFileOperation {
        path: PathBuf,
    },
    UnsafePath {
        path: PathBuf,
    },
}

impl fmt::Display for StructuredEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for StructuredEditError {}

/// Deterministic validation output suitable for logs and approval UIs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StructuredEditAudit {
    pub plan_id: String,
    pub touched_paths: Vec<PathBuf>,
    pub text_edit_count: usize,
    pub file_operation_count: usize,
    pub previews: Vec<EditPreview>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EditPreview {
    pub path: PathBuf,
    pub before: String,
    pub after: String,
    pub intent: String,
    pub provenance: String,
}

impl StructuredEditPlan {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            schema_version: 1,
            id: id.into(),
            text_edits: Vec::new(),
            file_operations: Vec::new(),
        }
    }

    pub fn add_text_edit(&mut self, edit: StructuredTextEdit) {
        self.text_edits.push(edit);
        self.normalize();
    }

    pub fn add_file_operation(&mut self, operation: StructuredFileOperation) {
        self.file_operations.push(operation);
        self.normalize();
    }

    pub fn normalize(&mut self) {
        self.text_edits.sort();
        self.text_edits.dedup();
        self.file_operations.sort();
        self.file_operations.dedup();
    }

    #[must_use]
    pub fn touched_paths(&self) -> Vec<PathBuf> {
        let mut paths = BTreeSet::new();
        for edit in &self.text_edits {
            paths.insert(edit.path.clone());
        }
        for operation in &self.file_operations {
            match operation {
                StructuredFileOperation::Create { path, .. }
                | StructuredFileOperation::Delete { path, .. } => {
                    paths.insert(path.clone());
                }
                StructuredFileOperation::Move { from, to, .. }
                | StructuredFileOperation::Rename { from, to, .. } => {
                    paths.insert(from.clone());
                    paths.insert(to.clone());
                }
            }
        }
        paths.into_iter().collect()
    }

    pub fn validate(
        &self,
        snapshots: &BTreeMap<PathBuf, FileSnapshot>,
    ) -> Result<StructuredEditAudit, Vec<StructuredEditError>> {
        let mut errors = Vec::new();
        let mut grouped: BTreeMap<&Path, Vec<&StructuredTextEdit>> = BTreeMap::new();

        for edit in &self.text_edits {
            if !safe_relative(&edit.path) {
                errors.push(StructuredEditError::UnsafePath {
                    path: edit.path.clone(),
                });
                continue;
            }
            if edit.range.start_byte > edit.range.end_byte {
                errors.push(StructuredEditError::InvalidRange {
                    path: edit.path.clone(),
                    range: edit.range,
                });
                continue;
            }
            grouped.entry(&edit.path).or_default().push(edit);
        }

        let mut previews = Vec::new();
        for (path, mut edits) in grouped {
            edits.sort_by_key(|edit| edit.range);
            for pair in edits.windows(2) {
                if pair[0].range.overlaps(pair[1].range) {
                    errors.push(StructuredEditError::OverlappingEdits {
                        path: path.to_path_buf(),
                        first: pair[0].range,
                        second: pair[1].range,
                    });
                }
            }
            let Some(snapshot) = snapshots.get(path) else {
                errors.push(StructuredEditError::MissingSnapshot {
                    path: path.to_path_buf(),
                });
                continue;
            };
            for edit in &edits {
                validate_edit(edit, snapshot, &mut errors);
            }
            if errors.is_empty() {
                let mut after = snapshot.content.clone();
                for edit in edits.iter().rev() {
                    after.replace_range(
                        edit.range.start_byte..edit.range.end_byte,
                        &edit.replacement,
                    );
                }
                for edit in edits {
                    previews.push(EditPreview {
                        path: path.to_path_buf(),
                        before: snapshot.content.clone(),
                        after: after.clone(),
                        intent: edit.metadata.intent.clone(),
                        provenance: edit.metadata.provenance.clone(),
                    });
                }
            }
        }

        validate_file_operations(&self.file_operations, snapshots, &mut errors);
        if !errors.is_empty() {
            return Err(errors);
        }

        Ok(StructuredEditAudit {
            plan_id: self.id.clone(),
            touched_paths: self.touched_paths(),
            text_edit_count: self.text_edits.len(),
            file_operation_count: self.file_operations.len(),
            previews,
        })
    }
}

fn validate_edit(
    edit: &StructuredTextEdit,
    snapshot: &FileSnapshot,
    errors: &mut Vec<StructuredEditError>,
) {
    if let Some(expected) = &edit.file_hash {
        if expected != &snapshot.hash {
            errors.push(StructuredEditError::StaleHash {
                path: edit.path.clone(),
                expected: expected.clone(),
                actual: snapshot.hash.clone(),
            });
        }
    }
    if let Some(expected) = edit.file_version {
        if snapshot.version != Some(expected) {
            errors.push(StructuredEditError::StaleVersion {
                path: edit.path.clone(),
                expected,
                actual: snapshot.version,
            });
        }
    }
    let Some(actual) = snapshot
        .content
        .get(edit.range.start_byte..edit.range.end_byte)
    else {
        errors.push(StructuredEditError::InvalidRange {
            path: edit.path.clone(),
            range: edit.range,
        });
        return;
    };
    if let Some(expected) = &edit.preconditions.expected_content {
        if expected != actual {
            errors.push(StructuredEditError::ExpectedContentMismatch {
                path: edit.path.clone(),
                range: edit.range,
                expected: expected.clone(),
                actual: actual.to_owned(),
            });
        }
    }
    if let Some(symbol) = &edit.preconditions.expected_symbol {
        if !snapshot.symbols.contains(symbol) {
            errors.push(StructuredEditError::ExpectedSymbolMissing {
                path: edit.path.clone(),
                symbol: symbol.clone(),
            });
        }
    }
    if let Some(node) = &edit.preconditions.expected_ast_node {
        if !snapshot.ast_nodes.contains(node) {
            errors.push(StructuredEditError::ExpectedAstNodeMissing {
                path: edit.path.clone(),
                node: node.clone(),
            });
        }
    }
}

fn validate_file_operations(
    operations: &[StructuredFileOperation],
    snapshots: &BTreeMap<PathBuf, FileSnapshot>,
    errors: &mut Vec<StructuredEditError>,
) {
    let mut destinations = BTreeSet::new();
    for operation in operations {
        let (source, destination, expected_hash) = match operation {
            StructuredFileOperation::Create { path, .. } => (None, Some(path), None),
            StructuredFileOperation::Delete {
                path,
                expected_hash,
                ..
            } => (Some(path), None, expected_hash.as_ref()),
            StructuredFileOperation::Move {
                from,
                to,
                expected_hash,
                ..
            }
            | StructuredFileOperation::Rename {
                from,
                to,
                expected_hash,
                ..
            } => (Some(from), Some(to), expected_hash.as_ref()),
        };
        for path in source.into_iter().chain(destination) {
            if !safe_relative(path) {
                errors.push(StructuredEditError::UnsafePath { path: path.clone() });
            }
        }
        if let Some(path) = destination {
            if !destinations.insert(path.clone()) {
                errors.push(StructuredEditError::ConflictingFileOperation { path: path.clone() });
            }
        }
        if let (Some(path), Some(expected)) = (source, expected_hash) {
            match snapshots.get(path) {
                Some(snapshot) if &snapshot.hash != expected => {
                    errors.push(StructuredEditError::StaleHash {
                        path: path.clone(),
                        expected: expected.clone(),
                        actual: snapshot.hash.clone(),
                    })
                }
                None => errors.push(StructuredEditError::MissingSnapshot { path: path.clone() }),
                _ => {}
            }
        }
    }
}

fn safe_relative(path: &Path) -> bool {
    !path.is_absolute()
        && path.components().all(|component| {
            matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
}

impl From<TextEdit> for StructuredTextEdit {
    fn from(edit: TextEdit) -> Self {
        Self {
            path: edit.path,
            file_hash: None,
            file_version: None,
            range: EditRange {
                start_byte: edit.start_byte,
                end_byte: edit.end_byte,
            },
            replacement: edit.replacement,
            metadata: EditMetadata {
                intent: "text_patch".to_owned(),
                provenance: "medusa_patch_transaction".to_owned(),
                annotation: None,
            },
            preconditions: EditPreconditions {
                expected_content: Some(edit.expected),
                expected_symbol: None,
                expected_ast_node: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(content: &str, hash: &str, version: u64) -> FileSnapshot {
        FileSnapshot {
            hash: hash.to_owned(),
            version: Some(version),
            content: content.to_owned(),
            symbols: BTreeSet::from(["answer".to_owned()]),
            ast_nodes: BTreeSet::from(["function_item:answer".to_owned()]),
        }
    }

    #[test]
    fn validates_multi_file_plan_without_mutation() {
        let mut plan = StructuredEditPlan::new("rename-answer");
        for path in ["src/lib.rs", "tests/use.rs"] {
            plan.add_text_edit(StructuredTextEdit {
                path: path.into(),
                file_hash: Some("h1".into()),
                file_version: Some(1),
                range: EditRange {
                    start_byte: 0,
                    end_byte: 6,
                },
                replacement: "result".into(),
                metadata: EditMetadata {
                    intent: "rename".into(),
                    provenance: "test".into(),
                    annotation: None,
                },
                preconditions: EditPreconditions {
                    expected_content: Some("answer".into()),
                    expected_symbol: Some("answer".into()),
                    expected_ast_node: None,
                },
            });
        }
        let snapshots = BTreeMap::from([
            (PathBuf::from("src/lib.rs"), snapshot("answer()", "h1", 1)),
            (PathBuf::from("tests/use.rs"), snapshot("answer()", "h1", 1)),
        ]);
        let audit = plan.validate(&snapshots).expect("valid plan");
        assert_eq!(audit.touched_paths.len(), 2);
        assert_eq!(snapshots[Path::new("src/lib.rs")].content, "answer()");
    }

    #[test]
    fn reports_overlap_and_stale_hash_as_typed_failures() {
        let mut plan = StructuredEditPlan::new("bad");
        for range in [
            EditRange {
                start_byte: 0,
                end_byte: 4,
            },
            EditRange {
                start_byte: 3,
                end_byte: 6,
            },
        ] {
            plan.add_text_edit(StructuredTextEdit {
                path: "src/lib.rs".into(),
                file_hash: Some("old".into()),
                file_version: None,
                range,
                replacement: "x".into(),
                metadata: EditMetadata::default(),
                preconditions: EditPreconditions::default(),
            });
        }
        let errors = plan
            .validate(&BTreeMap::from([(
                PathBuf::from("src/lib.rs"),
                snapshot("answer()", "new", 1),
            )]))
            .expect_err("invalid");
        assert!(
            errors
                .iter()
                .any(|error| matches!(error, StructuredEditError::OverlappingEdits { .. }))
        );
        assert!(
            errors
                .iter()
                .any(|error| matches!(error, StructuredEditError::StaleHash { .. }))
        );
    }

    #[test]
    fn serialization_is_deterministic_after_normalization() {
        let mut plan = StructuredEditPlan::new("stable");
        plan.add_text_edit(
            TextEdit {
                path: "b.rs".into(),
                start_byte: 0,
                end_byte: 1,
                expected: "b".into(),
                replacement: "B".into(),
            }
            .into(),
        );
        plan.add_text_edit(
            TextEdit {
                path: "a.rs".into(),
                start_byte: 0,
                end_byte: 1,
                expected: "a".into(),
                replacement: "A".into(),
            }
            .into(),
        );
        let first = serde_json::to_string(&plan).expect("serialize");
        let decoded: StructuredEditPlan = serde_json::from_str(&first).expect("deserialize");
        let second = serde_json::to_string(&decoded).expect("serialize");
        assert_eq!(first, second);
    }
}
