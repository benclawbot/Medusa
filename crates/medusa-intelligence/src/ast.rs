use std::path::PathBuf;

use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser, Point};

use crate::support::internal;

/// A stable source position using zero-based rows and columns.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SourcePosition {
    pub row: usize,
    pub column: usize,
}

impl From<Point> for SourcePosition {
    fn from(value: Point) -> Self {
        Self {
            row: value.row,
            column: value.column,
        }
    }
}

/// Byte and line/column location for one syntax node.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SourceRange {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start: SourcePosition,
    pub end: SourcePosition,
}

/// One parser diagnostic retained alongside the usable AST.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParseDiagnostic {
    pub kind: String,
    pub range: SourceRange,
    pub missing: bool,
    pub error: bool,
}

/// A language-aware Rust syntax node with explicit parent and child relationships.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustAstNode {
    pub id: usize,
    pub parent: Option<usize>,
    pub kind: String,
    pub named: bool,
    /// Tree-sitter field role assigned by the parent node.
    pub field_name: Option<String>,
    /// Semantic identifier extracted from the grammar's `name` field.
    pub name: Option<String>,
    pub range: SourceRange,
    pub children: Vec<usize>,
}

/// Parsed Rust syntax for one file.
///
/// The document keeps valid nodes even when Tree-sitter reports malformed or
/// missing syntax, allowing callers to perform partial indexing and diagnostics.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustAstDocument {
    pub path: PathBuf,
    pub root: usize,
    pub nodes: Vec<RustAstNode>,
    pub diagnostics: Vec<ParseDiagnostic>,
}

impl RustAstDocument {
    /// Parse Rust source into a serializable AST document.
    pub fn parse(path: impl Into<PathBuf>, source: &str) -> MedusaResult<Self> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .map_err(|error| internal(format!("configure Rust parser: {error}")))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| internal("Rust parser did not produce a syntax tree"))?;

        let mut document = Self {
            path: path.into(),
            root: 0,
            nodes: Vec::new(),
            diagnostics: Vec::new(),
        };
        document.root = document.collect(tree.root_node(), None, None, source);
        Ok(document)
    }

    /// Return a syntax node by its stable document-local identifier.
    #[must_use]
    pub fn node(&self, id: usize) -> Option<&RustAstNode> {
        self.nodes.get(id)
    }

    /// Iterate over nodes matching a Tree-sitter grammar kind.
    pub fn nodes_of_kind<'a>(&'a self, kind: &str) -> impl Iterator<Item = &'a RustAstNode> + 'a {
        let kind = kind.to_owned();
        self.nodes.iter().filter(move |node| node.kind == kind)
    }

    /// Whether parsing produced any error or missing-node diagnostics.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        !self.diagnostics.is_empty()
    }

    fn collect(
        &mut self,
        node: Node<'_>,
        parent: Option<usize>,
        field_name: Option<String>,
        source: &str,
    ) -> usize {
        let id = self.nodes.len();
        let range = source_range(node);
        self.nodes.push(RustAstNode {
            id,
            parent,
            kind: node.kind().to_owned(),
            named: node.is_named(),
            field_name,
            name: semantic_name(node, source),
            range,
            children: Vec::new(),
        });

        if node.is_error() || node.is_missing() {
            self.diagnostics.push(ParseDiagnostic {
                kind: node.kind().to_owned(),
                range,
                missing: node.is_missing(),
                error: node.is_error(),
            });
        }

        let mut cursor = node.walk();
        let child_ids = node
            .children(&mut cursor)
            .enumerate()
            .map(|(index, child)| {
                let child_field = node.field_name_for_child(index as u32).map(str::to_owned);
                self.collect(child, Some(id), child_field, source)
            })
            .collect();
        self.nodes[id].children = child_ids;
        id
    }
}

fn semantic_name(node: Node<'_>, source: &str) -> Option<String> {
    let name = node.child_by_field_name("name")?;
    source.get(name.byte_range()).map(str::to_owned)
}

fn source_range(node: Node<'_>) -> SourceRange {
    SourceRange {
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        start: node.start_position().into(),
        end: node.end_position().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_rich_rust_syntax_and_parent_relationships() {
        let source = r#"
#[derive(Clone)]
pub mod domain {
    use std::fmt::Debug;

    pub trait Store<T: Debug> { fn save(&self, value: T); }
    pub struct Memory<T> { values: Vec<T> }
    pub enum State { Empty, Ready }
    impl<T: Debug> Store<T> for Memory<T> {
        fn save(&self, value: T) { let _captured = value; }
    }
    pub type Id = u64;
}
"#;
        let document = RustAstDocument::parse("src/lib.rs", source).expect("parse");

        for kind in [
            "attribute_item",
            "mod_item",
            "use_declaration",
            "trait_item",
            "struct_item",
            "enum_item",
            "impl_item",
            "function_item",
            "type_item",
            "let_declaration",
        ] {
            assert!(
                document.nodes_of_kind(kind).next().is_some(),
                "missing {kind}"
            );
        }
        assert!(
            document
                .nodes
                .iter()
                .skip(1)
                .all(|node| node.parent.is_some())
        );
        assert!(!document.has_errors());
    }

    #[test]
    fn captures_semantic_names_and_field_roles() {
        let document = RustAstDocument::parse(
            "src/lib.rs",
            "pub struct User { pub name: String }\nimpl User { pub fn new() -> Self { todo!() } }\n",
        )
        .expect("parse");

        let structure = document
            .nodes_of_kind("struct_item")
            .next()
            .expect("struct item");
        assert_eq!(structure.name.as_deref(), Some("User"));

        let name_node = structure
            .children
            .iter()
            .filter_map(|id| document.node(*id))
            .find(|node| node.field_name.as_deref() == Some("name"))
            .expect("name field");
        assert_eq!(name_node.kind, "type_identifier");
    }

    #[test]
    fn malformed_source_retains_partial_ast_and_diagnostics() {
        let document = RustAstDocument::parse(
            "src/broken.rs",
            "pub struct Good;\nfn broken( {\npub enum StillVisible { A }\n",
        )
        .expect("partial parse");

        assert!(document.has_errors());
        assert!(document.nodes_of_kind("struct_item").next().is_some());
        assert!(document.nodes_of_kind("enum_item").next().is_some());
        assert!(document.nodes.len() > document.diagnostics.len());
    }

    #[test]
    fn serialization_round_trip_preserves_locations_and_identity() {
        let document =
            RustAstDocument::parse("src/lib.rs", "pub fn answer() -> u8 { 42 }\n").expect("parse");
        let encoded = serde_json::to_string(&document).expect("serialize");
        let decoded: RustAstDocument = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, document);
    }
}
