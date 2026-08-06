use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::lsp_actions::compare_rename_paths;
use crate::patch::{PatchTransaction, TextEdit};
use crate::support::{hash, validate_relative};
use crate::{LspPosition, LspRange, LspWorkspaceEdit, LspWorkspaceOperation};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GuardedRenamePlan {
    pub edit: LspWorkspaceEdit,
    pub paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RevisionBoundRenamePlan {
    pub plan: GuardedRenamePlan,
    pub before_hashes: BTreeMap<PathBuf, String>,
}

/// Validate a language-server rename before it can enter a mutation transaction.
///
/// The plan fails closed when the server requests resource operations, confirmation,
/// edits outside the accepted scope, overlapping edits, an empty edit, or when the
/// independently discovered static path set disagrees with the language server.
pub fn validate_guarded_rename(
    edit: LspWorkspaceEdit,
    accepted_paths: &[PathBuf],
    static_paths: &[PathBuf],
) -> Result<GuardedRenamePlan, String> {
    if edit.operations.is_empty() {
        return Err("rename refused because the language server returned no edits".into());
    }
    if edit
        .annotations
        .values()
        .any(|annotation| annotation.needs_confirmation)
    {
        return Err("rename refused because the workspace edit requires confirmation".into());
    }

    let mut by_path: BTreeMap<PathBuf, Vec<LspRange>> = BTreeMap::new();
    for operation in &edit.operations {
        let LspWorkspaceOperation::Text(text) = operation else {
            return Err("rename refused because resource operations are not supported".into());
        };
        if !accepted_paths.contains(&text.path) {
            return Err(format!(
                "rename refused because `{}` is outside the accepted write scope",
                text.path.display()
            ));
        }
        if text.new_text.is_empty() {
            return Err("rename refused because an edit has empty replacement text".into());
        }
        by_path
            .entry(text.path.clone())
            .or_default()
            .push(text.range.clone());
    }

    for (path, ranges) in &mut by_path {
        ranges.sort();
        if ranges.windows(2).any(|pair| pair[0].end > pair[1].start) {
            return Err(format!(
                "rename refused because edits overlap in `{}`",
                path.display()
            ));
        }
    }

    let comparison = compare_rename_paths(&edit, static_paths);
    if !comparison.agrees {
        return Err(format!(
            "rename refused because semantic and static path evidence disagree: only_lsp={:?}, only_static={:?}",
            comparison.only_lsp, comparison.only_static
        ));
    }

    Ok(GuardedRenamePlan {
        paths: comparison.lsp_paths,
        edit,
    })
}

/// Capture exact content hashes for every file touched by a guarded rename plan.
pub fn bind_guarded_rename_snapshot(
    repo: &Path,
    plan: GuardedRenamePlan,
) -> Result<RevisionBoundRenamePlan, String> {
    let before_hashes = plan
        .paths
        .iter()
        .map(|path| read_hash(repo, path).map(|digest| (path.clone(), digest)))
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    Ok(RevisionBoundRenamePlan {
        plan,
        before_hashes,
    })
}

/// Refuse a rename plan when any touched file changed after semantic analysis.
pub fn validate_guarded_rename_snapshot(
    repo: &Path,
    bound: &RevisionBoundRenamePlan,
) -> Result<(), String> {
    validate_snapshot_paths(bound)?;
    for path in &bound.plan.paths {
        let expected = bound
            .before_hashes
            .get(path)
            .ok_or_else(|| missing_snapshot(path))?;
        let actual = read_hash(repo, path)?;
        if &actual != expected {
            return Err(stale_snapshot(path, expected, &actual));
        }
    }
    Ok(())
}

/// Convert one LSP UTF-16 position into a UTF-8 byte offset.
///
/// Line endings may be LF, CRLF, or CR. Positions that split a UTF-16 surrogate
/// pair or exceed the selected line fail closed.
pub fn lsp_position_to_byte_offset(text: &str, position: &LspPosition) -> Result<usize, String> {
    let target_line = usize::try_from(position.line)
        .map_err(|_| "rename refused because the LSP line does not fit this platform".to_owned())?;
    let target_character = usize::try_from(position.character).map_err(|_| {
        "rename refused because the LSP character does not fit this platform".to_owned()
    })?;
    let (line_start, line_end) = line_bounds(text, target_line)?;
    let line = &text[line_start..line_end];

    let mut utf16_units = 0usize;
    for (byte_offset, character) in line.char_indices() {
        if utf16_units == target_character {
            return Ok(line_start + byte_offset);
        }
        let next = utf16_units + character.len_utf16();
        if target_character < next {
            return Err(format!(
                "rename refused because LSP position {}:{} splits a UTF-16 surrogate pair",
                position.line, position.character
            ));
        }
        utf16_units = next;
    }
    if utf16_units == target_character {
        Ok(line_end)
    } else {
        Err(format!(
            "rename refused because LSP position {}:{} exceeds the line",
            position.line, position.character
        ))
    }
}

/// Translate a revision-bound LSP workspace edit into the existing guarded patch transaction.
///
/// The returned transaction remains non-mutating until `PatchTransaction::commit` is invoked.
/// Prepared edits collapse every touched source file into one whole-file expected-content
/// edit, so drift anywhere in a file fails before journaling or replacement.
pub fn prepare_guarded_rename_transaction(
    repo: &Path,
    bound: &RevisionBoundRenamePlan,
) -> Result<PatchTransaction, String> {
    let validated = validate_guarded_rename(
        bound.plan.edit.clone(),
        &bound.plan.paths,
        &bound.plan.paths,
    )?;
    if validated.paths != bound.plan.paths {
        return Err("rename refused because the guarded plan is not canonical".into());
    }
    validate_snapshot_paths(bound)?;

    let mut sources = BTreeMap::new();
    for path in &validated.paths {
        let expected_hash = bound
            .before_hashes
            .get(path)
            .ok_or_else(|| missing_snapshot(path))?;
        let bytes = read_guarded_bytes(repo, path)?;
        let actual_hash = hash(&bytes);
        if &actual_hash != expected_hash {
            return Err(stale_snapshot(path, expected_hash, &actual_hash));
        }
        let source = String::from_utf8(bytes).map_err(|_| {
            format!(
                "rename refused because `{}` is not valid UTF-8",
                path.display()
            )
        })?;
        sources.insert(path.clone(), source);
    }

    let mut replacements: BTreeMap<PathBuf, Vec<TextEdit>> = BTreeMap::new();
    for operation in validated.edit.operations {
        let LspWorkspaceOperation::Text(edit) = operation else {
            return Err("rename refused because resource operations are not supported".into());
        };
        let source = sources.get(&edit.path).ok_or_else(|| {
            format!(
                "rename refused because `{}` has no revision-bound source",
                edit.path.display()
            )
        })?;
        let start_byte = lsp_position_to_byte_offset(source, &edit.range.start)?;
        let end_byte = lsp_position_to_byte_offset(source, &edit.range.end)?;
        if start_byte >= end_byte {
            return Err(format!(
                "rename refused because `{}` contains an empty or reversed edit range",
                edit.path.display()
            ));
        }
        let expected = source.get(start_byte..end_byte).ok_or_else(|| {
            format!(
                "rename refused because an edit is outside `{}`",
                edit.path.display()
            )
        })?;
        replacements
            .entry(edit.path.clone())
            .or_default()
            .push(TextEdit {
                path: edit.path,
                start_byte,
                end_byte,
                expected: expected.to_owned(),
                replacement: edit.new_text,
            });
    }

    let mut transaction = PatchTransaction::new();
    for path in &validated.paths {
        let source = sources.get(path).ok_or_else(|| {
            format!(
                "rename refused because `{}` has no revision-bound source",
                path.display()
            )
        })?;
        let edits = replacements.remove(path).ok_or_else(|| {
            format!(
                "rename refused because `{}` has no rename replacements",
                path.display()
            )
        })?;
        add_revision_guarded_file_edit(&mut transaction, path, source, edits)?;
    }
    if !replacements.is_empty() {
        return Err("rename refused because replacements escaped the guarded path set".into());
    }

    Ok(transaction)
}

fn add_revision_guarded_file_edit(
    transaction: &mut PatchTransaction,
    path: &Path,
    source: &str,
    mut replacements: Vec<TextEdit>,
) -> Result<(), String> {
    if source.is_empty() {
        return Err(format!(
            "rename refused because `{}` is empty",
            path.display()
        ));
    }
    replacements.sort_by_key(|edit| edit.start_byte);
    let mut previous_end = 0usize;
    for replacement in &replacements {
        if replacement.path.as_path() != path {
            return Err(format!(
                "rename refused because a replacement escaped `{}`",
                path.display()
            ));
        }
        if replacement.start_byte < previous_end || replacement.end_byte > source.len() {
            return Err(format!(
                "rename refused because byte ranges overlap or escape `{}`",
                path.display()
            ));
        }
        let expected = source
            .get(replacement.start_byte..replacement.end_byte)
            .ok_or_else(|| {
                format!(
                    "rename refused because a byte range splits UTF-8 in `{}`",
                    path.display()
                )
            })?;
        if expected != replacement.expected {
            return Err(format!(
                "rename refused because expected content differs in `{}`",
                path.display()
            ));
        }
        previous_end = replacement.end_byte;
    }

    let mut updated = source.to_owned();
    for replacement in replacements.into_iter().rev() {
        updated.replace_range(
            replacement.start_byte..replacement.end_byte,
            &replacement.replacement,
        );
    }
    if updated == source {
        return Err(format!(
            "rename refused because `{}` would not change",
            path.display()
        ));
    }

    transaction
        .add_edit(TextEdit {
            path: path.to_path_buf(),
            start_byte: 0,
            end_byte: source.len(),
            expected: source.to_owned(),
            replacement: updated,
        })
        .map_err(|error| error.to_string())
}

fn validate_snapshot_paths(bound: &RevisionBoundRenamePlan) -> Result<(), String> {
    let planned_paths = bound.plan.paths.iter().cloned().collect::<BTreeSet<_>>();
    let snapshot_paths = bound.before_hashes.keys().cloned().collect::<BTreeSet<_>>();
    if planned_paths != snapshot_paths {
        return Err(format!(
            "rename refused because snapshot paths differ from the guarded plan: planned={planned_paths:?}, snapshot={snapshot_paths:?}"
        ));
    }
    Ok(())
}

fn line_bounds(text: &str, target_line: usize) -> Result<(usize, usize), String> {
    let bytes = text.as_bytes();
    let mut current_line = 0usize;
    let mut line_start = 0usize;
    let mut index = 0usize;

    while index < bytes.len() {
        match bytes[index] {
            b'\n' => {
                if current_line == target_line {
                    return Ok((line_start, index));
                }
                current_line += 1;
                index += 1;
                line_start = index;
            }
            b'\r' => {
                if current_line == target_line {
                    return Ok((line_start, index));
                }
                current_line += 1;
                index += 1;
                if index < bytes.len() && bytes[index] == b'\n' {
                    index += 1;
                }
                line_start = index;
            }
            _ => index += 1,
        }
    }

    if current_line == target_line {
        Ok((line_start, bytes.len()))
    } else {
        Err(format!(
            "rename refused because LSP line {target_line} is outside the document"
        ))
    }
}

fn missing_snapshot(path: &Path) -> String {
    format!(
        "rename refused because `{}` has no snapshot",
        path.display()
    )
}

fn stale_snapshot(path: &Path, expected: &str, actual: &str) -> String {
    format!(
        "rename refused because `{}` changed after semantic analysis: expected {expected}, found {actual}",
        path.display()
    )
}

fn read_hash(repo: &Path, path: &Path) -> Result<String, String> {
    read_guarded_bytes(repo, path).map(|bytes| hash(&bytes))
}

fn read_guarded_bytes(repo: &Path, path: &Path) -> Result<Vec<u8>, String> {
    validate_relative(path).map_err(|error| error.to_string())?;
    let absolute = repo.join(path);
    let metadata = fs::symlink_metadata(&absolute).map_err(|error| {
        format!(
            "rename refused because `{}` cannot be inspected: {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "rename refused because `{}` is a symbolic link",
            path.display()
        ));
    }
    if !metadata.is_file() {
        return Err(format!(
            "rename refused because `{}` is not a regular file",
            path.display()
        ));
    }
    fs::read(&absolute).map_err(|error| {
        format!(
            "rename refused because `{}` cannot be read: {error}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        LspAnnotatedTextEdit, LspResourceOperation, LspWorkspaceOperation,
        finalize_patch_transactions,
    };

    fn text_edit(path: &str, start: u32, end: u32) -> LspWorkspaceOperation {
        ranged_text_edit(path, 0, start, 0, end)
    }

    fn ranged_text_edit(
        path: &str,
        start_line: u32,
        start_character: u32,
        end_line: u32,
        end_character: u32,
    ) -> LspWorkspaceOperation {
        LspWorkspaceOperation::Text(LspAnnotatedTextEdit {
            path: path.into(),
            range: LspRange {
                start: LspPosition {
                    line: start_line,
                    character: start_character,
                },
                end: LspPosition {
                    line: end_line,
                    character: end_character,
                },
            },
            new_text: "answer".into(),
            annotation_id: None,
        })
    }

    fn plan(path: &str) -> GuardedRenamePlan {
        GuardedRenamePlan {
            edit: LspWorkspaceEdit {
                operations: vec![text_edit(path, 0, 3)],
                annotations: BTreeMap::new(),
            },
            paths: vec![path.into()],
        }
    }

    #[test]
    fn accepts_scoped_non_overlapping_text_edits() {
        let edit = LspWorkspaceEdit {
            operations: vec![text_edit("src/lib.ts", 0, 3)],
            annotations: BTreeMap::new(),
        };
        let plan = validate_guarded_rename(
            edit,
            &[PathBuf::from("src/lib.ts")],
            &[PathBuf::from("src/lib.ts")],
        )
        .expect("guarded rename");
        assert_eq!(plan.paths, vec![PathBuf::from("src/lib.ts")]);
    }

    #[test]
    fn binds_and_revalidates_exact_file_content() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(directory.path().join("src")).expect("source directory");
        fs::write(directory.path().join("src/lib.ts"), "let answer = 1;").expect("source");

        let bound =
            bind_guarded_rename_snapshot(directory.path(), plan("src/lib.ts")).expect("snapshot");
        assert_eq!(bound.before_hashes[Path::new("src/lib.ts")].len(), 64);
        validate_guarded_rename_snapshot(directory.path(), &bound).expect("fresh snapshot");
    }

    #[test]
    fn maps_utf16_positions_across_unicode_and_line_endings() {
        let source = "a😀b\r\nβeta\n";
        assert_eq!(
            lsp_position_to_byte_offset(
                source,
                &LspPosition {
                    line: 0,
                    character: 1,
                },
            )
            .expect("before emoji"),
            1
        );
        assert_eq!(
            lsp_position_to_byte_offset(
                source,
                &LspPosition {
                    line: 0,
                    character: 3,
                },
            )
            .expect("after emoji"),
            5
        );
        assert_eq!(
            lsp_position_to_byte_offset(
                source,
                &LspPosition {
                    line: 1,
                    character: 1,
                },
            )
            .expect("after beta"),
            10
        );
        assert_eq!(
            lsp_position_to_byte_offset(
                source,
                &LspPosition {
                    line: 2,
                    character: 0,
                },
            )
            .expect("empty final line"),
            source.len()
        );
        let error = lsp_position_to_byte_offset(
            source,
            &LspPosition {
                line: 0,
                character: 2,
            },
        )
        .expect_err("surrogate split must fail");
        assert!(error.contains("surrogate pair"));
    }

    #[test]
    fn prepares_and_commits_unicode_cross_file_rename() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(directory.path().join("src")).expect("source directory");
        fs::write(directory.path().join("src/a.ts"), "const 😀target = 1;\n")
            .expect("first source");
        fs::write(directory.path().join("src/b.ts"), "target();\n").expect("second source");

        let edit = LspWorkspaceEdit {
            operations: vec![
                ranged_text_edit("src/a.ts", 0, 8, 0, 14),
                ranged_text_edit("src/b.ts", 0, 0, 0, 6),
            ],
            annotations: BTreeMap::new(),
        };
        let paths = vec![PathBuf::from("src/a.ts"), PathBuf::from("src/b.ts")];
        let plan = validate_guarded_rename(edit, &paths, &paths).expect("plan");
        let bound = bind_guarded_rename_snapshot(directory.path(), plan).expect("snapshot");
        let transaction =
            prepare_guarded_rename_transaction(directory.path(), &bound).expect("transaction");
        let receipt = transaction.commit(directory.path()).expect("commit");

        assert_eq!(receipt.changed_paths, paths);
        assert_eq!(
            fs::read_to_string(directory.path().join("src/a.ts")).expect("first"),
            "const 😀answer = 1;\n"
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("src/b.ts")).expect("second"),
            "answer();\n"
        );
        finalize_patch_transactions(directory.path(), true).expect("finalize");
    }

    #[test]
    fn transaction_refuses_drift_outside_rename_ranges() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(directory.path().join("src")).expect("source directory");
        let path = directory.path().join("src/lib.ts");
        fs::write(&path, "target + untouched\n").expect("source");

        let edit = LspWorkspaceEdit {
            operations: vec![ranged_text_edit("src/lib.ts", 0, 0, 0, 6)],
            annotations: BTreeMap::new(),
        };
        let paths = vec![PathBuf::from("src/lib.ts")];
        let plan = validate_guarded_rename(edit, &paths, &paths).expect("plan");
        let bound = bind_guarded_rename_snapshot(directory.path(), plan).expect("snapshot");
        let transaction =
            prepare_guarded_rename_transaction(directory.path(), &bound).expect("transaction");

        fs::write(&path, "target + changed\n").expect("drift");
        let error = transaction
            .commit(directory.path())
            .expect_err("unrenamed drift must fail");
        assert!(error.to_string().contains("stale edit"));
        assert_eq!(
            fs::read_to_string(path).expect("source"),
            "target + changed\n"
        );
    }

    #[test]
    fn refuses_changed_or_incomplete_snapshot() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(directory.path().join("src")).expect("source directory");
        fs::write(directory.path().join("src/lib.ts"), "let answer = 1;").expect("source");

        let mut bound =
            bind_guarded_rename_snapshot(directory.path(), plan("src/lib.ts")).expect("snapshot");
        fs::write(directory.path().join("src/lib.ts"), "let answer = 2;").expect("changed");
        let error = validate_guarded_rename_snapshot(directory.path(), &bound)
            .expect_err("changed content must fail");
        assert!(error.contains("changed after semantic analysis"));

        bound.before_hashes.clear();
        let error = validate_guarded_rename_snapshot(directory.path(), &bound)
            .expect_err("incomplete snapshot must fail");
        assert!(error.contains("snapshot paths differ"));
    }

    #[test]
    fn refuses_scope_drift_and_path_disagreement() {
        let edit = LspWorkspaceEdit {
            operations: vec![text_edit("src/other.ts", 0, 3)],
            annotations: BTreeMap::new(),
        };
        let error = validate_guarded_rename(
            edit.clone(),
            &[PathBuf::from("src/lib.ts")],
            &[PathBuf::from("src/other.ts")],
        )
        .expect_err("scope drift must fail");
        assert!(error.contains("outside the accepted write scope"));

        let error = validate_guarded_rename(
            edit,
            &[PathBuf::from("src/other.ts")],
            &[PathBuf::from("src/lib.ts")],
        )
        .expect_err("evidence disagreement must fail");
        assert!(error.contains("path evidence disagree"));
    }

    #[test]
    fn refuses_overlapping_and_resource_edits() {
        let overlap = LspWorkspaceEdit {
            operations: vec![text_edit("src/lib.ts", 0, 4), text_edit("src/lib.ts", 3, 7)],
            annotations: BTreeMap::new(),
        };
        let error = validate_guarded_rename(
            overlap,
            &[PathBuf::from("src/lib.ts")],
            &[PathBuf::from("src/lib.ts")],
        )
        .expect_err("overlap must fail");
        assert!(error.contains("edits overlap"));

        let resource = LspWorkspaceEdit {
            operations: vec![LspWorkspaceOperation::Resource(
                crate::LspResourceOperation::Create {
                    path: "src/new.ts".into(),
                    overwrite: false,
                    ignore_if_exists: false,
                    annotation_id: None,
                },
            )],
            annotations: BTreeMap::new(),
        };
        let error =
            validate_guarded_rename(resource, &[], &[]).expect_err("resource operation must fail");
        assert!(error.contains("resource operations"));
    }
}
