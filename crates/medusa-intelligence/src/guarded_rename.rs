use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::lsp_actions::compare_rename_paths;
use crate::support::{hash, validate_relative};
use crate::{LspRange, LspWorkspaceEdit, LspWorkspaceOperation};

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
    let planned_paths = bound.plan.paths.iter().cloned().collect::<BTreeSet<_>>();
    let snapshot_paths = bound.before_hashes.keys().cloned().collect::<BTreeSet<_>>();
    if planned_paths != snapshot_paths {
        return Err(format!(
            "rename refused because snapshot paths differ from the guarded plan: planned={planned_paths:?}, snapshot={snapshot_paths:?}"
        ));
    }

    for path in &bound.plan.paths {
        let expected = bound
            .before_hashes
            .get(path)
            .ok_or_else(|| format!("rename refused because `{}` has no snapshot", path.display()))?;
        let actual = read_hash(repo, path)?;
        if &actual != expected {
            return Err(format!(
                "rename refused because `{}` changed after semantic analysis: expected {expected}, found {actual}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn read_hash(repo: &Path, path: &Path) -> Result<String, String> {
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
    let bytes = fs::read(&absolute).map_err(|error| {
        format!(
            "rename refused because `{}` cannot be read: {error}",
            path.display()
        )
    })?;
    Ok(hash(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LspAnnotatedTextEdit, LspPosition, LspResourceOperation, LspWorkspaceOperation};

    fn text_edit(path: &str, start: u32, end: u32) -> LspWorkspaceOperation {
        LspWorkspaceOperation::Text(LspAnnotatedTextEdit {
            path: path.into(),
            range: LspRange {
                start: LspPosition {
                    line: 0,
                    character: start,
                },
                end: LspPosition {
                    line: 0,
                    character: end,
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

        let bound = bind_guarded_rename_snapshot(directory.path(), plan("src/lib.ts"))
            .expect("snapshot");
        assert_eq!(bound.before_hashes[Path::new("src/lib.ts")].len(), 64);
        validate_guarded_rename_snapshot(directory.path(), &bound).expect("fresh snapshot");
    }

    #[test]
    fn refuses_changed_or_incomplete_snapshot() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(directory.path().join("src")).expect("source directory");
        fs::write(directory.path().join("src/lib.ts"), "let answer = 1;").expect("source");

        let mut bound = bind_guarded_rename_snapshot(directory.path(), plan("src/lib.ts"))
            .expect("snapshot");
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
                LspResourceOperation::Create {
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
