use std::{collections::BTreeSet, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{ResolutionStatus, RustResolutionIndex, RustSymbolId, RustSymbolTable, TextEdit};

/// One indexed source file participating in a workspace rename plan.
pub struct RustRenameFile<'a> {
    pub source: &'a str,
    pub symbols: &'a RustSymbolTable,
    pub resolution: &'a RustResolutionIndex,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RustRenameConflictKind {
    InvalidIdentifier,
    MissingTarget,
    NameCollision,
    AmbiguousReference,
    StaleSource,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustRenameConflict {
    pub kind: RustRenameConflictKind,
    pub path: Option<PathBuf>,
    pub message: String,
}

/// A reviewable, non-mutating multi-file rename proposal.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustRenamePlan {
    pub target: RustSymbolId,
    pub old_name: String,
    pub new_name: String,
    pub edits: Vec<TextEdit>,
    pub conflicts: Vec<RustRenameConflict>,
}

impl Default for RustRenamePlan {
    fn default() -> Self {
        Self {
            target: RustSymbolId(String::new()),
            old_name: String::new(),
            new_name: String::new(),
            edits: Vec::new(),
            conflicts: Vec::new(),
        }
    }
}

impl RustRenamePlan {
    #[must_use]
    pub fn is_safe(&self) -> bool {
        self.conflicts.is_empty()
    }

    #[must_use]
    pub fn changed_paths(&self) -> Vec<PathBuf> {
        self.edits
            .iter()
            .map(|edit| edit.path.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// Build a semantic rename plan without changing the workspace.
#[must_use]
pub fn plan_rust_rename(
    files: &[RustRenameFile<'_>],
    target: &RustSymbolId,
    new_name: &str,
) -> RustRenamePlan {
    let mut plan = RustRenamePlan {
        target: target.clone(),
        new_name: new_name.to_owned(),
        ..RustRenamePlan::default()
    };

    if !valid_rust_identifier(new_name) {
        plan.conflicts.push(RustRenameConflict {
            kind: RustRenameConflictKind::InvalidIdentifier,
            path: None,
            message: format!("`{new_name}` is not a valid Rust identifier"),
        });
        return plan;
    }

    let Some((target_file, target_symbol)) = files
        .iter()
        .find_map(|file| file.symbols.symbol(target).map(|symbol| (file, symbol)))
    else {
        plan.conflicts.push(RustRenameConflict {
            kind: RustRenameConflictKind::MissingTarget,
            path: None,
            message: "rename target is not present in the workspace index".to_owned(),
        });
        return plan;
    };
    plan.old_name = target_symbol.name.clone();

    let replacement_qualified = target_symbol.qualified_name.rsplit_once("::").map_or_else(
        || new_name.to_owned(),
        |(prefix, _)| format!("{prefix}::{new_name}"),
    );
    let collisions = target_file
        .symbols
        .find_qualified(&replacement_qualified)
        .into_iter()
        .filter(|symbol| symbol.id != *target)
        .collect::<Vec<_>>();
    if !collisions.is_empty() {
        plan.conflicts.push(RustRenameConflict {
            kind: RustRenameConflictKind::NameCollision,
            path: Some(target_symbol.path.clone()),
            message: format!("`{replacement_qualified}` already exists"),
        });
    }

    push_edit(
        &mut plan,
        target_symbol.path.clone(),
        target_file.source,
        target_symbol.name_range.start_byte,
        target_symbol.name_range.end_byte,
        new_name,
    );

    for file in files {
        for reference in &file.resolution.references {
            if !reference.targets.contains(target) {
                continue;
            }
            if reference.status != ResolutionStatus::Resolved {
                plan.conflicts.push(RustRenameConflict {
                    kind: RustRenameConflictKind::AmbiguousReference,
                    path: Some(file.symbols.path.clone()),
                    message: format!("reference `{}` is not uniquely resolved", reference.name),
                });
                continue;
            }
            push_edit(
                &mut plan,
                file.symbols.path.clone(),
                file.source,
                reference.range.start_byte,
                reference.range.end_byte,
                new_name,
            );
        }
    }

    plan.edits.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.start_byte.cmp(&right.start_byte))
    });
    plan.edits.dedup_by(|left, right| {
        left.path == right.path
            && left.start_byte == right.start_byte
            && left.end_byte == right.end_byte
    });
    plan
}

fn push_edit(
    plan: &mut RustRenamePlan,
    path: PathBuf,
    source: &str,
    start_byte: usize,
    end_byte: usize,
    replacement: &str,
) {
    let Some(expected) = source.get(start_byte..end_byte) else {
        plan.conflicts.push(RustRenameConflict {
            kind: RustRenameConflictKind::StaleSource,
            path: Some(path),
            message: format!("source range {start_byte}..{end_byte} is no longer valid"),
        });
        return;
    };
    plan.edits.push(TextEdit {
        path,
        start_byte,
        end_byte,
        expected: expected.to_owned(),
        replacement: replacement.to_owned(),
    });
}

fn valid_rust_identifier(name: &str) -> bool {
    let name = name.strip_prefix("r#").unwrap_or(name);
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_alphabetic())
        && chars.all(|character| character == '_' || character.is_alphanumeric())
        && !matches!(
            name,
            "as" | "break"
                | "const"
                | "continue"
                | "crate"
                | "else"
                | "enum"
                | "extern"
                | "false"
                | "fn"
                | "for"
                | "if"
                | "impl"
                | "in"
                | "let"
                | "loop"
                | "match"
                | "mod"
                | "move"
                | "mut"
                | "pub"
                | "ref"
                | "return"
                | "self"
                | "Self"
                | "static"
                | "struct"
                | "super"
                | "trait"
                | "true"
                | "type"
                | "unsafe"
                | "use"
                | "where"
                | "while"
                | "async"
                | "await"
                | "dyn"
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RustAstDocument, RustResolutionIndex, RustSymbolTable};

    fn indexed(path: &str, source: &str) -> (RustSymbolTable, RustResolutionIndex) {
        let ast = RustAstDocument::parse(path, source).expect("ast");
        let table = RustSymbolTable::build(&ast, source);
        let resolution = RustResolutionIndex::build(&ast, source, &table);
        (table, resolution)
    }

    #[test]
    fn plans_definition_and_resolved_references() {
        let source = "fn old_name() {} fn caller() { old_name(); }";
        let (table, resolution) = indexed("src/lib.rs", source);
        let target = table.find_simple("old_name")[0].id.clone();
        let files = [RustRenameFile {
            source,
            symbols: &table,
            resolution: &resolution,
        }];
        let plan = plan_rust_rename(&files, &target, "new_name");
        assert!(plan.is_safe());
        assert_eq!(plan.edits.len(), 2);
        assert_eq!(plan.changed_paths(), vec![PathBuf::from("src/lib.rs")]);
    }

    #[test]
    fn reports_same_scope_collisions() {
        let source = "fn first() {} fn second() {}";
        let (table, resolution) = indexed("src/lib.rs", source);
        let target = table.find_simple("first")[0].id.clone();
        let files = [RustRenameFile {
            source,
            symbols: &table,
            resolution: &resolution,
        }];
        let plan = plan_rust_rename(&files, &target, "second");
        assert!(!plan.is_safe());
        assert_eq!(
            plan.conflicts[0].kind,
            RustRenameConflictKind::NameCollision
        );
    }

    #[test]
    fn rejects_keywords_before_creating_edits() {
        let source = "fn answer() {}";
        let (table, resolution) = indexed("src/lib.rs", source);
        let target = table.find_simple("answer")[0].id.clone();
        let files = [RustRenameFile {
            source,
            symbols: &table,
            resolution: &resolution,
        }];
        let plan = plan_rust_rename(&files, &target, "match");
        assert!(plan.edits.is_empty());
        assert_eq!(
            plan.conflicts[0].kind,
            RustRenameConflictKind::InvalidIdentifier
        );
    }

    #[test]
    fn serialization_preserves_reviewable_plan() {
        let source = "fn old() {} fn call() { old(); }";
        let (table, resolution) = indexed("src/lib.rs", source);
        let target = table.find_simple("old")[0].id.clone();
        let files = [RustRenameFile {
            source,
            symbols: &table,
            resolution: &resolution,
        }];
        let plan = plan_rust_rename(&files, &target, "new");
        let encoded = serde_json::to_string(&plan).expect("serialize");
        let decoded: RustRenamePlan = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, plan);
    }
}
