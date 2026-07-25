use std::path::{Path, PathBuf};

use medusa_core::MedusaResult;

use crate::{
    EditMetadata, EditPreconditions, EditRange, RustAstDocument, StructuredEditPlan,
    StructuredTextEdit,
    support::{hash, invalid},
};

/// AST-aware Rust edit planner that produces guarded, non-mutating structured edits.
pub struct RustStructuredEditPlanner<'a> {
    path: PathBuf,
    source: &'a str,
    document: RustAstDocument,
}

impl<'a> RustStructuredEditPlanner<'a> {
    pub fn parse(path: impl Into<PathBuf>, source: &'a str) -> MedusaResult<Self> {
        let path = path.into();
        let document = RustAstDocument::parse(path.clone(), source)?;
        if document.has_errors() {
            return Err(invalid(format!(
                "cannot plan AST edits for malformed Rust source: {}",
                path.display()
            )));
        }
        Ok(Self {
            path,
            source,
            document,
        })
    }

    #[must_use]
    pub fn document(&self) -> &RustAstDocument {
        &self.document
    }

    pub fn replace_node(
        &self,
        node_id: usize,
        replacement: impl Into<String>,
        intent: impl Into<String>,
        expected_symbol: Option<String>,
    ) -> MedusaResult<StructuredEditPlan> {
        self.edit_node(node_id, replacement.into(), intent.into(), expected_symbol)
    }

    pub fn delete_node(
        &self,
        node_id: usize,
        intent: impl Into<String>,
        expected_symbol: Option<String>,
    ) -> MedusaResult<StructuredEditPlan> {
        self.edit_node(node_id, String::new(), intent.into(), expected_symbol)
    }

    pub fn insert_before(
        &self,
        node_id: usize,
        text: impl Into<String>,
        intent: impl Into<String>,
    ) -> MedusaResult<StructuredEditPlan> {
        let node = self
            .document
            .node(node_id)
            .ok_or_else(|| invalid(format!("Rust AST node not found: {node_id}")))?;
        self.build_plan(
            EditRange {
                start_byte: node.range.start_byte,
                end_byte: node.range.start_byte,
            },
            text.into(),
            intent.into(),
            Some(node.kind.clone()),
            None,
        )
    }

    pub fn insert_after(
        &self,
        node_id: usize,
        text: impl Into<String>,
        intent: impl Into<String>,
    ) -> MedusaResult<StructuredEditPlan> {
        let node = self
            .document
            .node(node_id)
            .ok_or_else(|| invalid(format!("Rust AST node not found: {node_id}")))?;
        self.build_plan(
            EditRange {
                start_byte: node.range.end_byte,
                end_byte: node.range.end_byte,
            },
            text.into(),
            intent.into(),
            Some(node.kind.clone()),
            None,
        )
    }

    /// Add a Rust import exactly once, preserving the existing import block.
    pub fn add_import(&self, import_path: &str) -> MedusaResult<StructuredEditPlan> {
        let statement = normalize_import(import_path)?;
        if self
            .source
            .lines()
            .any(|line| line.trim() == statement.trim())
        {
            return Ok(StructuredEditPlan::new(plan_id(
                &self.path,
                "add_import_noop",
                statement.as_bytes(),
            )));
        }

        let imports = self
            .document
            .nodes_of_kind("use_declaration")
            .collect::<Vec<_>>();
        let insertion = imports
            .last()
            .map_or(0, |node| node.range.end_byte);
        let prefix = if insertion == 0 || self.source[..insertion].ends_with('\n') {
            String::new()
        } else {
            "\n".to_owned()
        };
        let suffix = if insertion == 0 { "\n" } else { "" };
        self.build_plan(
            EditRange {
                start_byte: insertion,
                end_byte: insertion,
            },
            format!("{prefix}{statement}\n{suffix}"),
            format!("add import {import_path}"),
            Some("use_declaration".to_owned()),
            None,
        )
    }

    /// Remove one exact import. Missing imports are treated as an idempotent no-op.
    pub fn remove_import(&self, import_path: &str) -> MedusaResult<StructuredEditPlan> {
        let statement = normalize_import(import_path)?;
        let Some(node) = self.document.nodes_of_kind("use_declaration").find(|node| {
            self.source
                .get(node.range.start_byte..node.range.end_byte)
                .is_some_and(|text| text.trim() == statement.trim())
        }) else {
            return Ok(StructuredEditPlan::new(plan_id(
                &self.path,
                "remove_import_noop",
                statement.as_bytes(),
            )));
        };

        let mut end = node.range.end_byte;
        if self.source.as_bytes().get(end) == Some(&b'\n') {
            end += 1;
        }
        self.build_plan(
            EditRange {
                start_byte: node.range.start_byte,
                end_byte: end,
            },
            String::new(),
            format!("remove import {import_path}"),
            Some("use_declaration".to_owned()),
            None,
        )
    }

    fn edit_node(
        &self,
        node_id: usize,
        replacement: String,
        intent: String,
        expected_symbol: Option<String>,
    ) -> MedusaResult<StructuredEditPlan> {
        let node = self
            .document
            .node(node_id)
            .ok_or_else(|| invalid(format!("Rust AST node not found: {node_id}")))?;
        self.build_plan(
            EditRange {
                start_byte: node.range.start_byte,
                end_byte: node.range.end_byte,
            },
            replacement,
            intent,
            Some(node.kind.clone()),
            expected_symbol,
        )
    }

    fn build_plan(
        &self,
        range: EditRange,
        replacement: String,
        intent: String,
        expected_ast_node: Option<String>,
        expected_symbol: Option<String>,
    ) -> MedusaResult<StructuredEditPlan> {
        let expected = self
            .source
            .get(range.start_byte..range.end_byte)
            .ok_or_else(|| invalid("Rust AST edit range is outside source"))?;
        let mut staged = self.source.to_owned();
        staged.replace_range(range.start_byte..range.end_byte, &replacement);
        let reparsed = RustAstDocument::parse(self.path.clone(), &staged)?;
        if reparsed.has_errors() {
            return Err(invalid(format!(
                "Rust structured edit produced malformed output: {}",
                self.path.display()
            )));
        }

        let mut plan = StructuredEditPlan::new(plan_id(
            &self.path,
            &intent,
            staged.as_bytes(),
        ));
        plan.add_text_edit(StructuredTextEdit {
            path: self.path.clone(),
            file_hash: Some(hash(self.source.as_bytes())),
            file_version: None,
            range,
            replacement,
            metadata: EditMetadata {
                intent,
                provenance: "rust_ast".to_owned(),
                annotation: Some("reparsed before application".to_owned()),
            },
            preconditions: EditPreconditions {
                expected_content: Some(expected.to_owned()),
                expected_symbol,
                expected_ast_node,
            },
        });
        Ok(plan)
    }
}

fn normalize_import(import_path: &str) -> MedusaResult<String> {
    let path = import_path.trim().trim_end_matches(';').trim();
    if path.is_empty() || path.contains('\n') {
        return Err(invalid("invalid Rust import path"));
    }
    Ok(format!("use {path};"))
}

fn plan_id(path: &Path, intent: &str, payload: &[u8]) -> String {
    let identity = format!("{}|{}|{}", path.display(), intent, hash(payload));
    format!("rust-edit-{}", &hash(identity.as_bytes())[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_function_body_with_node_preconditions() {
        let source = "pub fn answer() -> u8 { 1 }\n";
        let planner = RustStructuredEditPlanner::parse("src/lib.rs", source).expect("planner");
        let body = planner
            .document()
            .nodes_of_kind("block")
            .next()
            .expect("block");
        let plan = planner
            .replace_node(body.id, "{ 42 }", "replace function body", Some("answer".to_owned()))
            .expect("plan");
        assert_eq!(plan.text_edits.len(), 1);
        assert_eq!(
            plan.text_edits[0].preconditions.expected_ast_node.as_deref(),
            Some("block")
        );
        assert_eq!(
            plan.text_edits[0].preconditions.expected_symbol.as_deref(),
            Some("answer")
        );
    }

    #[test]
    fn import_edits_are_idempotent() {
        let source = "use std::fmt::Debug;\nfn main() {}\n";
        let planner = RustStructuredEditPlanner::parse("src/main.rs", source).expect("planner");
        assert!(planner.add_import("std::fmt::Debug").expect("add").text_edits.is_empty());
        assert!(planner.remove_import("std::io::Read").expect("remove").text_edits.is_empty());
    }

    #[test]
    fn malformed_replacement_is_rejected_before_application() {
        let source = "pub fn answer() -> u8 { 1 }\n";
        let planner = RustStructuredEditPlanner::parse("src/lib.rs", source).expect("planner");
        let function = planner
            .document()
            .nodes_of_kind("function_item")
            .next()
            .expect("function");
        assert!(
            planner
                .replace_node(function.id, "fn broken(", "break function", None)
                .is_err()
        );
    }
}
