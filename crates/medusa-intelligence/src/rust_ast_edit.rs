use std::{collections::BTreeSet, fmt, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    EditMetadata, EditPreconditions, EditRange, RustAstDocument, RustAstNode, StructuredEditPlan,
    StructuredTextEdit,
};
use crate::support::hash;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustEditTarget {
    pub kind: String,
    pub name: Option<String>,
    pub node_id: Option<usize>,
}

impl RustEditTarget {
    #[must_use]
    pub fn named(kind: impl Into<String>, name: impl Into<String>) -> Self {
        Self { kind: kind.into(), name: Some(name.into()), node_id: None }
    }

    #[must_use]
    pub fn node(node_id: usize) -> Self {
        Self { kind: String::new(), name: None, node_id: Some(node_id) }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RustAstEdit {
    ReplaceNode { target: RustEditTarget, replacement: String },
    DeleteNode { target: RustEditTarget },
    InsertBefore { target: RustEditTarget, content: String },
    InsertAfter { target: RustEditTarget, content: String },
    ReplaceFunctionBody { function: String, body: String },
    ReplaceFunctionSignature { function: String, signature: String },
    SetVisibility { target: RustEditTarget, visibility: String },
    AddImport { path: String },
    RemoveImport { path: String },
    AddModule { name: String, public: bool },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RustStructuredEditError {
    MissingTarget { kind: String, name: Option<String> },
    AmbiguousTarget { kind: String, name: Option<String>, matches: usize },
    InvalidNodeId { node_id: usize },
    InvalidSourceRange { node_id: usize },
    MalformedOutput { diagnostics: usize },
    InvalidVisibility { visibility: String },
    InvalidModuleName { name: String },
    OverlappingGeneratedEdits,
}

impl fmt::Display for RustStructuredEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for RustStructuredEditError {}

#[derive(Clone, Debug)]
pub struct RustStructuredEditPlanner<'a> {
    path: PathBuf,
    source: &'a str,
    document: RustAstDocument,
    plan: StructuredEditPlan,
}

impl<'a> RustStructuredEditPlanner<'a> {
    pub fn new(id: impl Into<String>, path: impl Into<PathBuf>, source: &'a str) -> Result<Self, RustStructuredEditError> {
        let path = path.into();
        let document = RustAstDocument::parse(path.clone(), source)
            .map_err(|_| RustStructuredEditError::MalformedOutput { diagnostics: 1 })?;
        if document.has_errors() {
            return Err(RustStructuredEditError::MalformedOutput { diagnostics: document.diagnostics.len() });
        }
        Ok(Self { path, source, document, plan: StructuredEditPlan::new(id) })
    }

    pub fn push(&mut self, edit: RustAstEdit) -> Result<(), RustStructuredEditError> {
        match edit {
            RustAstEdit::ReplaceNode { target, replacement } => {
                let node = self.resolve(&target)?;
                self.add_node_edit(&node, replacement, "replace_rust_node");
            }
            RustAstEdit::DeleteNode { target } => {
                let node = self.resolve(&target)?;
                self.add_node_edit(&node, String::new(), "delete_rust_node");
            }
            RustAstEdit::InsertBefore { target, content } => {
                let node = self.resolve(&target)?;
                self.add_range_edit(&node, node.range.start_byte, node.range.start_byte, content, "insert_before_rust_node");
            }
            RustAstEdit::InsertAfter { target, content } => {
                let node = self.resolve(&target)?;
                self.add_range_edit(&node, node.range.end_byte, node.range.end_byte, content, "insert_after_rust_node");
            }
            RustAstEdit::ReplaceFunctionBody { function, body } => {
                let function_node = self.resolve(&RustEditTarget::named("function_item", &function))?;
                let body_node = self.child_of_kind(&function_node, "block")
                    .ok_or_else(|| RustStructuredEditError::MissingTarget { kind: "block".to_owned(), name: Some(function) })?;
                self.add_node_edit(&body_node, normalize_block(&body), "replace_function_body");
            }
            RustAstEdit::ReplaceFunctionSignature { function, signature } => {
                let function_node = self.resolve(&RustEditTarget::named("function_item", &function))?;
                let body_node = self.child_of_kind(&function_node, "block")
                    .ok_or_else(|| RustStructuredEditError::MissingTarget { kind: "block".to_owned(), name: Some(function) })?;
                self.add_range_edit(&function_node, function_node.range.start_byte, body_node.range.start_byte, format!("{} ", signature.trim()), "replace_function_signature");
            }
            RustAstEdit::SetVisibility { target, visibility } => {
                if !matches!(visibility.as_str(), "" | "pub" | "pub(crate)" | "pub(super)" | "pub(self)") {
                    return Err(RustStructuredEditError::InvalidVisibility { visibility });
                }
                let node = self.resolve(&target)?;
                let text = self.node_text(&node)?.to_owned();
                self.add_node_edit(&node, replace_visibility(&text, &visibility), "set_rust_visibility");
            }
            RustAstEdit::AddImport { path } => self.add_import(&path)?,
            RustAstEdit::RemoveImport { path } => self.remove_import(&path)?,
            RustAstEdit::AddModule { name, public } => self.add_module(&name, public)?,
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<StructuredEditPlan, RustStructuredEditError> {
        self.plan.normalize();
        let staged = apply_text_edits(self.source, &self.plan.text_edits)?;
        let reparsed = RustAstDocument::parse(self.path.clone(), &staged)
            .map_err(|_| RustStructuredEditError::MalformedOutput { diagnostics: 1 })?;
        if reparsed.has_errors() {
            return Err(RustStructuredEditError::MalformedOutput { diagnostics: reparsed.diagnostics.len() });
        }
        Ok(self.plan)
    }

    fn resolve(&self, target: &RustEditTarget) -> Result<RustAstNode, RustStructuredEditError> {
        if let Some(node_id) = target.node_id {
            return self.document.node(node_id).cloned().ok_or(RustStructuredEditError::InvalidNodeId { node_id });
        }
        let matches = self.document.nodes.iter()
            .filter(|node| target.kind.is_empty() || node.kind == target.kind)
            .filter(|node| target.name.as_ref().map_or(true, |name| self.node_name(node) == Some(name.as_str())))
            .cloned()
            .collect::<Vec<_>>();
        match matches.len() {
            0 => Err(RustStructuredEditError::MissingTarget { kind: target.kind.clone(), name: target.name.clone() }),
            1 => Ok(matches[0].clone()),
            count => Err(RustStructuredEditError::AmbiguousTarget { kind: target.kind.clone(), name: target.name.clone(), matches: count }),
        }
    }

    fn node_name<'b>(&'b self, node: &'b RustAstNode) -> Option<&'b str> {
        node.children.iter().filter_map(|id| self.document.node(*id)).find_map(|child| {
            if matches!(child.kind.as_str(), "identifier" | "type_identifier" | "field_identifier") {
                self.source.get(child.range.start_byte..child.range.end_byte)
            } else {
                None
            }
        })
    }

    fn child_of_kind(&self, node: &RustAstNode, kind: &str) -> Option<RustAstNode> {
        node.children.iter().filter_map(|id| self.document.node(*id)).find(|child| child.kind == kind).cloned()
    }

    fn node_text(&self, node: &RustAstNode) -> Result<&str, RustStructuredEditError> {
        self.source.get(node.range.start_byte..node.range.end_byte)
            .ok_or(RustStructuredEditError::InvalidSourceRange { node_id: node.id })
    }

    fn add_node_edit(&mut self, node: &RustAstNode, replacement: String, intent: &str) {
        self.add_range_edit(node, node.range.start_byte, node.range.end_byte, replacement, intent);
    }

    fn add_range_edit(&mut self, node: &RustAstNode, start: usize, end: usize, replacement: String, intent: &str) {
        self.plan.add_text_edit(StructuredTextEdit {
            path: self.path.clone(),
            file_hash: Some(hash(self.source.as_bytes())),
            file_version: None,
            range: EditRange { start_byte: start, end_byte: end },
            replacement,
            metadata: EditMetadata {
                intent: intent.to_owned(),
                provenance: "rust_ast_structured_edit".to_owned(),
                annotation: Some(format!("{}#{}", node.kind, node.id)),
            },
            preconditions: EditPreconditions {
                expected_content: Some(self.source.get(start..end).unwrap_or_default().to_owned()),
                expected_symbol: self.node_name(node).map(str::to_owned),
                expected_ast_node: Some(ast_identity(node)),
            },
        });
    }

    fn add_import(&mut self, path: &str) -> Result<(), RustStructuredEditError> {
        let normalized = path.trim().trim_end_matches(';');
        if self.document.nodes_of_kind("use_declaration")
            .filter_map(|node| self.node_text(node).ok())
            .any(|text| text.trim().trim_end_matches(';').trim_start_matches("use ") == normalized)
        {
            return Ok(());
        }
        let mut imports = self.document.nodes_of_kind("use_declaration").cloned().collect::<Vec<_>>();
        imports.sort_by_key(|node| node.range.start_byte);
        let insertion = imports.last().map_or(0, |node| node.range.end_byte);
        let root = self.document.node(self.document.root).cloned().expect("root node");
        let prefix = if insertion == 0 { "" } else { "\n" };
        self.add_range_edit(&root, insertion, insertion, format!("{prefix}use {normalized};\n"), "add_rust_import");
        Ok(())
    }

    fn remove_import(&mut self, path: &str) -> Result<(), RustStructuredEditError> {
        let normalized = path.trim().trim_end_matches(';');
        let matches = self.document.nodes_of_kind("use_declaration")
            .filter(|node| self.node_text(node).map_or(false, |text| text.trim().trim_end_matches(';').trim_start_matches("use ") == normalized))
            .cloned()
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            return Err(RustStructuredEditError::AmbiguousTarget { kind: "use_declaration".to_owned(), name: Some(normalized.to_owned()), matches: matches.len() });
        }
        if let Some(node) = matches.first() {
            let mut end = node.range.end_byte;
            if self.source.as_bytes().get(end) == Some(&b'\n') { end += 1; }
            self.add_range_edit(node, node.range.start_byte, end, String::new(), "remove_rust_import");
        }
        Ok(())
    }

    fn add_module(&mut self, name: &str, public: bool) -> Result<(), RustStructuredEditError> {
        if !valid_identifier(name) {
            return Err(RustStructuredEditError::InvalidModuleName { name: name.to_owned() });
        }
        if self.document.nodes_of_kind("mod_item").any(|node| self.node_name(node) == Some(name)) {
            return Ok(());
        }
        let root = self.document.node(self.document.root).cloned().expect("root node");
        let visibility = if public { "pub " } else { "" };
        self.add_range_edit(&root, root.range.end_byte, root.range.end_byte, format!("\n{visibility}mod {name};\n"), "add_rust_module");
        Ok(())
    }
}

#[must_use]
pub fn rust_snapshot_ast_nodes(document: &RustAstDocument) -> BTreeSet<String> {
    document.nodes.iter().map(ast_identity).collect()
}

fn ast_identity(node: &RustAstNode) -> String {
    format!("{}@{}:{}", node.kind, node.range.start_byte, node.range.end_byte)
}

fn normalize_block(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') { trimmed.to_owned() } else { format!("{{ {trimmed} }}") }
}

fn replace_visibility(text: &str, visibility: &str) -> String {
    let trimmed = text.trim_start();
    let indent = &text[..text.len() - trimmed.len()];
    let rest = ["pub(crate) ", "pub(super) ", "pub(self) ", "pub "]
        .into_iter().find_map(|prefix| trimmed.strip_prefix(prefix)).unwrap_or(trimmed);
    if visibility.is_empty() { format!("{indent}{rest}") } else { format!("{indent}{visibility} {rest}") }
}

fn apply_text_edits(source: &str, edits: &[StructuredTextEdit]) -> Result<String, RustStructuredEditError> {
    let mut output = source.to_owned();
    let mut edits = edits.to_vec();
    edits.sort_by_key(|edit| edit.range);
    for pair in edits.windows(2) {
        if pair[0].range.overlaps(pair[1].range) {
            return Err(RustStructuredEditError::OverlappingGeneratedEdits);
        }
    }
    for edit in edits.into_iter().rev() {
        if output.get(edit.range.start_byte..edit.range.end_byte).is_none() {
            return Err(RustStructuredEditError::InvalidSourceRange { node_id: usize::MAX });
        }
        output.replace_range(edit.range.start_byte..edit.range.end_byte, &edit.replacement);
    }
    Ok(output)
}

fn valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(first) if first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "use std::fmt::Debug;\n\npub mod api;\n\npub fn answer(value: u8) -> u8 { value + 1 }\n";

    #[test]
    fn edits_signature_and_body_with_ast_preconditions() {
        let mut planner = RustStructuredEditPlanner::new("rust-edit", "src/lib.rs", SOURCE).expect("planner");
        planner.push(RustAstEdit::ReplaceFunctionSignature { function: "answer".to_owned(), signature: "pub fn answer(value: u16) -> u16".to_owned() }).expect("signature");
        planner.push(RustAstEdit::ReplaceFunctionBody { function: "answer".to_owned(), body: "{ value + 2 }".to_owned() }).expect("body");
        let plan = planner.finish().expect("plan");
        assert_eq!(plan.text_edits.len(), 2);
        assert!(plan.text_edits.iter().all(|edit| edit.preconditions.expected_ast_node.is_some()));
    }

    #[test]
    fn imports_and_modules_are_idempotent() {
        let mut planner = RustStructuredEditPlanner::new("imports", "src/lib.rs", SOURCE).expect("planner");
        planner.push(RustAstEdit::AddImport { path: "std::fmt::Debug".to_owned() }).expect("import");
        planner.push(RustAstEdit::AddModule { name: "api".to_owned(), public: true }).expect("module");
        assert!(planner.finish().expect("plan").text_edits.is_empty());
    }

    #[test]
    fn missing_and_ambiguous_targets_fail_safely() {
        let mut planner = RustStructuredEditPlanner::new("missing", "src/lib.rs", SOURCE).expect("planner");
        assert!(matches!(planner.push(RustAstEdit::DeleteNode { target: RustEditTarget::named("function_item", "missing") }), Err(RustStructuredEditError::MissingTarget { .. })));
        let mut planner = RustStructuredEditPlanner::new("ambiguous", "src/lib.rs", "fn same() {}\nfn same() {}\n").expect("planner");
        assert!(matches!(planner.push(RustAstEdit::DeleteNode { target: RustEditTarget::named("function_item", "same") }), Err(RustStructuredEditError::AmbiguousTarget { .. })));
    }

    #[test]
    fn malformed_staged_output_is_rejected() {
        let mut planner = RustStructuredEditPlanner::new("broken", "src/lib.rs", SOURCE).expect("planner");
        planner.push(RustAstEdit::ReplaceNode { target: RustEditTarget::named("function_item", "answer"), replacement: "fn answer( {".to_owned() }).expect("edit");
        assert!(matches!(planner.finish(), Err(RustStructuredEditError::MalformedOutput { .. })));
    }

    #[test]
    fn representative_item_visibility_import_and_module_edits_reparse() {
        let mut planner = RustStructuredEditPlanner::new("representative", "src/lib.rs", SOURCE).expect("planner");
        planner.push(RustAstEdit::SetVisibility { target: RustEditTarget::named("function_item", "answer"), visibility: "pub(crate)".to_owned() }).expect("visibility");
        planner.push(RustAstEdit::AddImport { path: "std::collections::BTreeMap".to_owned() }).expect("import");
        planner.push(RustAstEdit::AddModule { name: "domain".to_owned(), public: false }).expect("module");
        assert_eq!(planner.finish().expect("plan").text_edits.len(), 3);
    }

    #[test]
    fn remove_import_reparses_cleanly() {
        let mut planner = RustStructuredEditPlanner::new("remove", "src/lib.rs", SOURCE).expect("planner");
        planner.push(RustAstEdit::RemoveImport { path: "std::fmt::Debug".to_owned() }).expect("remove");
        assert_eq!(planner.finish().expect("plan").text_edits.len(), 1);
    }
}
