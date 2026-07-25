use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use medusa_core::MedusaResult;

use crate::{
    EditMetadata, EditPreconditions, EditRange, RustAstDocument, RustAstNode, StructuredEditPlan,
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
        let node = self.node(node_id)?;
        self.build_plan(vec![self.text_edit(
            node,
            EditRange {
                start_byte: node.range.start_byte,
                end_byte: node.range.start_byte,
            },
            text.into(),
            intent.into(),
            None,
        )?])
    }

    pub fn insert_after(
        &self,
        node_id: usize,
        text: impl Into<String>,
        intent: impl Into<String>,
    ) -> MedusaResult<StructuredEditPlan> {
        let node = self.node(node_id)?;
        self.build_plan(vec![self.text_edit(
            node,
            EditRange {
                start_byte: node.range.end_byte,
                end_byte: node.range.end_byte,
            },
            text.into(),
            intent.into(),
            None,
        )?])
    }

    pub fn replace_function_body(
        &self,
        function: &str,
        body: &str,
    ) -> MedusaResult<StructuredEditPlan> {
        let function_node = self.resolve_unique("function_item", function)?;
        let body_node = self
            .child_of_kind(function_node, "block")
            .ok_or_else(|| invalid(format!("function body not found: {function}")))?;
        let replacement = normalize_block(body);
        self.build_plan(vec![self.text_edit(
            body_node,
            range(body_node),
            replacement,
            format!("replace body of {function}"),
            Some(function.to_owned()),
        )?])
    }

    pub fn replace_function_signature(
        &self,
        function: &str,
        signature: &str,
    ) -> MedusaResult<StructuredEditPlan> {
        let function_node = self.resolve_unique("function_item", function)?;
        let body_node = self
            .child_of_kind(function_node, "block")
            .ok_or_else(|| invalid(format!("function body not found: {function}")))?;
        self.build_plan(vec![self.text_edit(
            function_node,
            EditRange {
                start_byte: function_node.range.start_byte,
                end_byte: body_node.range.start_byte,
            },
            format!("{} ", signature.trim()),
            format!("replace signature of {function}"),
            Some(function.to_owned()),
        )?])
    }

    pub fn set_visibility(
        &self,
        kind: &str,
        name: &str,
        visibility: &str,
    ) -> MedusaResult<StructuredEditPlan> {
        if !matches!(
            visibility,
            "" | "pub" | "pub(crate)" | "pub(super)" | "pub(self)"
        ) {
            return Err(invalid(format!(
                "unsupported Rust visibility: {visibility}"
            )));
        }
        let node = self.resolve_unique(kind, name)?;
        let text = self.node_text(node)?;
        self.build_plan(vec![self.text_edit(
            node,
            range(node),
            replace_visibility(text, visibility),
            format!("set visibility of {name}"),
            Some(name.to_owned()),
        )?])
    }

    pub fn move_node_before(
        &self,
        node_id: usize,
        anchor_id: usize,
        intent: impl Into<String>,
    ) -> MedusaResult<StructuredEditPlan> {
        let node = self.node(node_id)?;
        let anchor = self.node(anchor_id)?;
        if node.id == anchor.id {
            return Err(invalid("cannot move a Rust node relative to itself"));
        }
        let text = self.node_text(node)?.to_owned();
        let intent = intent.into();
        let delete = self.text_edit(
            node,
            range(node),
            String::new(),
            intent.clone(),
            self.node_name(node).map(str::to_owned),
        )?;
        let insert = self.text_edit(
            anchor,
            EditRange {
                start_byte: anchor.range.start_byte,
                end_byte: anchor.range.start_byte,
            },
            format!("{text}\n"),
            intent,
            self.node_name(node).map(str::to_owned),
        )?;
        self.build_plan(vec![delete, insert])
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
        let insertion = imports.last().map_or(0, |node| node.range.end_byte);
        let root = self.node(self.document.root)?;
        let prefix = if insertion == 0 || self.source[..insertion].ends_with('\n') {
            ""
        } else {
            "\n"
        };
        let suffix = if insertion == 0 { "\n" } else { "" };
        self.build_plan(vec![self.text_edit(
            root,
            EditRange {
                start_byte: insertion,
                end_byte: insertion,
            },
            format!("{prefix}{statement}\n{suffix}"),
            format!("add import {import_path}"),
            None,
        )?])
    }

    /// Remove one exact import. Missing imports are an idempotent no-op.
    pub fn remove_import(&self, import_path: &str) -> MedusaResult<StructuredEditPlan> {
        let statement = normalize_import(import_path)?;
        let matches = self
            .document
            .nodes_of_kind("use_declaration")
            .filter(|node| {
                self.node_text(node)
                    .is_ok_and(|text| text.trim() == statement.trim())
            })
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            return Err(invalid(format!("ambiguous Rust import: {import_path}")));
        }
        let Some(node) = matches.first() else {
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
        self.build_plan(vec![self.text_edit(
            node,
            EditRange {
                start_byte: node.range.start_byte,
                end_byte: end,
            },
            String::new(),
            format!("remove import {import_path}"),
            None,
        )?])
    }

    pub fn add_module(&self, name: &str, public: bool) -> MedusaResult<StructuredEditPlan> {
        if !valid_identifier(name) {
            return Err(invalid(format!("invalid Rust module name: {name}")));
        }
        if self
            .document
            .nodes_of_kind("mod_item")
            .any(|node| self.node_name(node) == Some(name))
        {
            return Ok(StructuredEditPlan::new(plan_id(
                &self.path,
                "add_module_noop",
                name.as_bytes(),
            )));
        }
        let root = self.node(self.document.root)?;
        let visibility = if public { "pub " } else { "" };
        self.build_plan(vec![self.text_edit(
            root,
            EditRange {
                start_byte: root.range.end_byte,
                end_byte: root.range.end_byte,
            },
            format!("\n{visibility}mod {name};\n"),
            format!("add module {name}"),
            Some(name.to_owned()),
        )?])
    }

    fn edit_node(
        &self,
        node_id: usize,
        replacement: String,
        intent: String,
        expected_symbol: Option<String>,
    ) -> MedusaResult<StructuredEditPlan> {
        let node = self.node(node_id)?;
        self.build_plan(vec![self.text_edit(
            node,
            range(node),
            replacement,
            intent,
            expected_symbol,
        )?])
    }

    fn build_plan(&self, mut edits: Vec<StructuredTextEdit>) -> MedusaResult<StructuredEditPlan> {
        edits.sort_by_key(|edit| edit.range);
        for pair in edits.windows(2) {
            if pair[0].range.overlaps(pair[1].range) {
                return Err(invalid("generated Rust AST edits overlap"));
            }
        }
        let mut staged = self.source.to_owned();
        for edit in edits.iter().rev() {
            staged.replace_range(
                edit.range.start_byte..edit.range.end_byte,
                &edit.replacement,
            );
        }
        let reparsed = RustAstDocument::parse(self.path.clone(), &staged)?;
        if reparsed.has_errors() {
            return Err(invalid(format!(
                "Rust structured edit produced malformed output: {}",
                self.path.display()
            )));
        }
        let intent = edits
            .iter()
            .map(|edit| edit.metadata.intent.as_str())
            .collect::<Vec<_>>()
            .join(";");
        let mut plan = StructuredEditPlan::new(plan_id(&self.path, &intent, staged.as_bytes()));
        for edit in edits {
            plan.add_text_edit(edit);
        }
        Ok(plan)
    }

    fn text_edit(
        &self,
        node: &RustAstNode,
        edit_range: EditRange,
        replacement: String,
        intent: String,
        expected_symbol: Option<String>,
    ) -> MedusaResult<StructuredTextEdit> {
        let expected = self
            .source
            .get(edit_range.start_byte..edit_range.end_byte)
            .ok_or_else(|| invalid("Rust AST edit range is outside source"))?;
        Ok(StructuredTextEdit {
            path: self.path.clone(),
            file_hash: Some(hash(self.source.as_bytes())),
            file_version: None,
            range: edit_range,
            replacement,
            metadata: EditMetadata {
                intent,
                provenance: "rust_ast".to_owned(),
                annotation: Some(format!("{}#{}", node.kind, node.id)),
            },
            preconditions: EditPreconditions {
                expected_content: Some(expected.to_owned()),
                expected_symbol,
                expected_ast_node: Some(ast_identity(node)),
            },
        })
    }

    fn node(&self, id: usize) -> MedusaResult<&RustAstNode> {
        self.document
            .node(id)
            .ok_or_else(|| invalid(format!("Rust AST node not found: {id}")))
    }
    fn node_text(&self, node: &RustAstNode) -> MedusaResult<&str> {
        self.source
            .get(node.range.start_byte..node.range.end_byte)
            .ok_or_else(|| invalid("Rust AST node range is outside source"))
    }
    fn child_of_kind<'b>(&'b self, node: &'b RustAstNode, kind: &str) -> Option<&'b RustAstNode> {
        node.children
            .iter()
            .filter_map(|id| self.document.node(*id))
            .find(|child| child.kind == kind)
    }
    fn node_name<'b>(&'b self, node: &'b RustAstNode) -> Option<&'b str> {
        node.children
            .iter()
            .filter_map(|id| self.document.node(*id))
            .find_map(|child| {
                matches!(
                    child.kind.as_str(),
                    "identifier" | "type_identifier" | "field_identifier"
                )
                .then(|| {
                    self.source
                        .get(child.range.start_byte..child.range.end_byte)
                })
                .flatten()
            })
    }

    fn resolve_unique<'b>(&'b self, kind: &str, name: &str) -> MedusaResult<&'b RustAstNode> {
        let matches = self
            .document
            .nodes_of_kind(kind)
            .filter(|node| self.node_name(node) == Some(name))
            .collect::<Vec<_>>();
        match matches.len() {
            0 => Err(invalid(format!("Rust AST target not found: {kind} {name}"))),
            1 => Ok(matches[0]),
            count => Err(invalid(format!(
                "ambiguous Rust AST target {kind} {name}: {count} matches"
            ))),
        }
    }
}

#[must_use]
pub fn rust_snapshot_ast_nodes(document: &RustAstDocument) -> BTreeSet<String> {
    document.nodes.iter().map(ast_identity).collect()
}

fn range(node: &RustAstNode) -> EditRange {
    EditRange {
        start_byte: node.range.start_byte,
        end_byte: node.range.end_byte,
    }
}
fn ast_identity(node: &RustAstNode) -> String {
    format!(
        "{}@{}:{}",
        node.kind, node.range.start_byte, node.range.end_byte
    )
}
fn normalize_block(body: &str) -> String {
    let body = body.trim();
    if body.starts_with('{') && body.ends_with('}') {
        body.to_owned()
    } else {
        format!("{{ {body} }}")
    }
}
fn replace_visibility(text: &str, visibility: &str) -> String {
    let trimmed = text.trim_start();
    let indent = &text[..text.len() - trimmed.len()];
    let rest = ["pub(crate) ", "pub(super) ", "pub(self) ", "pub "]
        .into_iter()
        .find_map(|prefix| trimmed.strip_prefix(prefix))
        .unwrap_or(trimmed);
    if visibility.is_empty() {
        format!("{indent}{rest}")
    } else {
        format!("{indent}{visibility} {rest}")
    }
}
fn normalize_import(import_path: &str) -> MedusaResult<String> {
    let path = import_path.trim().trim_end_matches(';').trim();
    if path.is_empty() || path.contains('\n') {
        return Err(invalid("invalid Rust import path"));
    }
    Ok(format!("use {path};"))
}
fn valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(first) if first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}
fn plan_id(path: &Path, intent: &str, payload: &[u8]) -> String {
    let identity = format!("{}|{}|{}", path.display(), intent, hash(payload));
    format!("rust-edit-{}", &hash(identity.as_bytes())[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_body_visibility_and_module_edits_are_guarded() {
        let source = "use std::fmt::Debug;\npub struct Item;\npub fn answer(value: u8) -> u8 { value + 1 }\n";
        let planner = RustStructuredEditPlanner::parse("src/lib.rs", source).expect("planner");
        for plan in [
            planner
                .replace_function_signature("answer", "pub fn answer(value: u16) -> u16")
                .expect("signature"),
            planner
                .replace_function_body("answer", "value + 2")
                .expect("body"),
            planner
                .set_visibility("struct_item", "Item", "pub(crate)")
                .expect("visibility"),
            planner.add_module("domain", false).expect("module"),
        ] {
            assert!(plan.text_edits.iter().all(|edit| {
                edit.preconditions
                    .expected_ast_node
                    .as_ref()
                    .is_some_and(|value| value.contains('@'))
            }));
        }
    }

    #[test]
    fn import_and_module_edits_are_idempotent() {
        let source = "use std::fmt::Debug;\npub mod api;\nfn main() {}\n";
        let planner = RustStructuredEditPlanner::parse("src/main.rs", source).expect("planner");
        assert!(
            planner
                .add_import("std::fmt::Debug")
                .expect("add")
                .text_edits
                .is_empty()
        );
        assert!(
            planner
                .remove_import("std::io::Read")
                .expect("remove")
                .text_edits
                .is_empty()
        );
        assert!(
            planner
                .add_module("api", true)
                .expect("module")
                .text_edits
                .is_empty()
        );
    }

    #[test]
    fn missing_ambiguous_and_malformed_edits_fail_before_application() {
        let planner =
            RustStructuredEditPlanner::parse("src/lib.rs", "fn same() {}\nfn same() {}\n")
                .expect("planner");
        assert!(planner.replace_function_body("missing", "{}").is_err());
        assert!(planner.replace_function_body("same", "{}").is_err());
        let planner =
            RustStructuredEditPlanner::parse("src/lib.rs", "fn answer() {}\n").expect("planner");
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
