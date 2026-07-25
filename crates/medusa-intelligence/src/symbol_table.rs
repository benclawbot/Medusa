use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ast::{RustAstDocument, RustAstNode, SourceRange};

/// Stable identity for a symbol across unrelated edits.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RustSymbolId(pub String);

/// Rust declaration categories represented by the symbol table.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RustSymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Variant,
    Trait,
    Module,
    Impl,
    TypeAlias,
    Constant,
    Static,
    Macro,
    Field,
    Parameter,
    Local,
}

/// Scope categories used for deterministic lexical lookup.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RustScopeKind {
    File,
    Module,
    Type,
    Trait,
    Impl,
    Function,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustScope {
    pub id: usize,
    pub parent: Option<usize>,
    pub kind: RustScopeKind,
    pub name: String,
    pub qualified_name: String,
    pub owner: Option<RustSymbolId>,
    pub symbols: Vec<RustSymbolId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustSymbol {
    pub id: RustSymbolId,
    pub name: String,
    pub qualified_name: String,
    pub kind: RustSymbolKind,
    pub path: PathBuf,
    pub range: SourceRange,
    pub name_range: SourceRange,
    pub scope: usize,
    pub public: bool,
}

/// Deterministic scoped symbol table derived from one Rust AST document.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustSymbolTable {
    pub path: PathBuf,
    pub scopes: Vec<RustScope>,
    pub symbols: BTreeMap<RustSymbolId, RustSymbol>,
    pub by_qualified_name: BTreeMap<String, Vec<RustSymbolId>>,
    pub by_simple_name: BTreeMap<String, Vec<RustSymbolId>>,
}

impl RustSymbolTable {
    #[must_use]
    pub fn build(document: &RustAstDocument, source: &str) -> Self {
        let file_name = document
            .path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("crate")
            .to_owned();
        let mut table = Self {
            path: document.path.clone(),
            scopes: vec![RustScope {
                id: 0,
                parent: None,
                kind: RustScopeKind::File,
                name: file_name.clone(),
                qualified_name: file_name,
                owner: None,
                symbols: Vec::new(),
            }],
            symbols: BTreeMap::new(),
            by_qualified_name: BTreeMap::new(),
            by_simple_name: BTreeMap::new(),
        };
        table.walk(document, source, document.root, 0);
        table
    }

    #[must_use]
    pub fn symbol(&self, id: &RustSymbolId) -> Option<&RustSymbol> {
        self.symbols.get(id)
    }

    #[must_use]
    pub fn find_qualified(&self, name: &str) -> Vec<&RustSymbol> {
        self.by_qualified_name
            .get(name)
            .into_iter()
            .flatten()
            .filter_map(|id| self.symbols.get(id))
            .collect()
    }

    #[must_use]
    pub fn find_simple(&self, name: &str) -> Vec<&RustSymbol> {
        self.by_simple_name
            .get(name)
            .into_iter()
            .flatten()
            .filter_map(|id| self.symbols.get(id))
            .collect()
    }

    /// Resolve a simple name from a scope, preferring the nearest lexical declaration.
    #[must_use]
    pub fn resolve_in_scope(&self, mut scope: usize, name: &str) -> Vec<&RustSymbol> {
        loop {
            let matches = self.scopes[scope]
                .symbols
                .iter()
                .filter_map(|id| self.symbols.get(id))
                .filter(|symbol| symbol.name == name)
                .collect::<Vec<_>>();
            if !matches.is_empty() {
                return matches;
            }
            let Some(parent) = self.scopes[scope].parent else {
                return Vec::new();
            };
            scope = parent;
        }
    }

    fn walk(&mut self, document: &RustAstDocument, source: &str, node_id: usize, scope: usize) {
        let node = &document.nodes[node_id];
        let declaration = declaration(document, source, node);
        let mut child_scope = scope;

        if let Some((name, name_range, kind)) = declaration {
            let qualified_name = qualify(&self.scopes[scope].qualified_name, &name);
            let id = stable_id(&document.path, kind, &qualified_name);
            let public = source
                .get(node.range.start_byte..name_range.start_byte)
                .is_some_and(|prefix| prefix.split_whitespace().any(|part| part == "pub"));
            let symbol = RustSymbol {
                id: id.clone(),
                name: name.clone(),
                qualified_name: qualified_name.clone(),
                kind,
                path: document.path.clone(),
                range: node.range,
                name_range,
                scope,
                public,
            };
            self.scopes[scope].symbols.push(id.clone());
            self.by_qualified_name
                .entry(qualified_name.clone())
                .or_default()
                .push(id.clone());
            self.by_simple_name
                .entry(name.clone())
                .or_default()
                .push(id.clone());
            self.symbols.insert(id.clone(), symbol);

            if let Some(scope_kind) = scope_kind(kind) {
                child_scope = self.scopes.len();
                self.scopes.push(RustScope {
                    id: child_scope,
                    parent: Some(scope),
                    kind: scope_kind,
                    name,
                    qualified_name,
                    owner: Some(id),
                    symbols: Vec::new(),
                });
            }
        }

        for child in &node.children {
            self.walk(document, source, *child, child_scope);
        }
    }
}

fn declaration(
    document: &RustAstDocument,
    source: &str,
    node: &RustAstNode,
) -> Option<(String, SourceRange, RustSymbolKind)> {
    let mut kind = match node.kind.as_str() {
        "function_item" => RustSymbolKind::Function,
        "struct_item" => RustSymbolKind::Struct,
        "enum_item" => RustSymbolKind::Enum,
        "enum_variant" => RustSymbolKind::Variant,
        "trait_item" => RustSymbolKind::Trait,
        "mod_item" => RustSymbolKind::Module,
        "impl_item" => RustSymbolKind::Impl,
        "type_item" => RustSymbolKind::TypeAlias,
        "const_item" => RustSymbolKind::Constant,
        "static_item" => RustSymbolKind::Static,
        "macro_definition" => RustSymbolKind::Macro,
        "field_declaration" => RustSymbolKind::Field,
        "parameter" | "self_parameter" => RustSymbolKind::Parameter,
        "let_declaration" => RustSymbolKind::Local,
        _ => return None,
    };

    let name_node = first_named_identifier(document, node);
    let (name, range) = if let Some(name_node) = name_node {
        let text = source.get(name_node.range.start_byte..name_node.range.end_byte)?;
        (text.trim_start_matches("r#").to_owned(), name_node.range)
    } else if kind == RustSymbolKind::Impl {
        (
            format!("<impl@{}>", node.range.start_byte),
            SourceRange {
                start_byte: node.range.start_byte,
                end_byte: node.range.start_byte,
                start: node.range.start,
                end: node.range.start,
            },
        )
    } else {
        return None;
    };

    if kind == RustSymbolKind::Function
        && ancestors(document, node.id)
            .any(|ancestor| matches!(ancestor.kind.as_str(), "impl_item" | "trait_item"))
    {
        kind = RustSymbolKind::Method;
    }
    Some((name, range, kind))
}

fn first_named_identifier<'a>(
    document: &'a RustAstDocument,
    node: &RustAstNode,
) -> Option<&'a RustAstNode> {
    node.children
        .iter()
        .filter_map(|id| document.node(*id))
        .find(|child| {
            matches!(
                child.kind.as_str(),
                "identifier" | "type_identifier" | "field_identifier" | "self"
            )
        })
        .or_else(|| {
            node.children
                .iter()
                .filter_map(|id| document.node(*id))
                .flat_map(|child| child.children.iter())
                .filter_map(|id| document.node(*id))
                .find(|child| {
                    matches!(
                        child.kind.as_str(),
                        "identifier" | "type_identifier" | "field_identifier" | "self"
                    )
                })
        })
}

fn ancestors(document: &RustAstDocument, node_id: usize) -> impl Iterator<Item = &RustAstNode> {
    std::iter::successors(
        document.node(node_id).and_then(|node| node.parent),
        move |id| document.node(*id).and_then(|node| node.parent),
    )
    .filter_map(|id| document.node(id))
}

fn scope_kind(kind: RustSymbolKind) -> Option<RustScopeKind> {
    match kind {
        RustSymbolKind::Module => Some(RustScopeKind::Module),
        RustSymbolKind::Struct | RustSymbolKind::Enum => Some(RustScopeKind::Type),
        RustSymbolKind::Trait => Some(RustScopeKind::Trait),
        RustSymbolKind::Impl => Some(RustScopeKind::Impl),
        RustSymbolKind::Function | RustSymbolKind::Method => Some(RustScopeKind::Function),
        _ => None,
    }
}

fn qualify(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}::{name}")
    }
}

fn stable_id(path: &Path, kind: RustSymbolKind, qualified_name: &str) -> RustSymbolId {
    let mut digest = Sha256::new();
    digest.update(path.to_string_lossy().as_bytes());
    digest.update([0]);
    digest.update(format!("{kind:?}").as_bytes());
    digest.update([0]);
    digest.update(qualified_name.as_bytes());
    RustSymbolId(hex::encode(digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_deterministic_scopes_and_distinguishes_duplicate_names() {
        let source = r#"
mod alpha { pub fn run(value: u8) { let local = value; } }
mod beta { pub fn run(value: u16) { let local = value; } }
trait Store { fn save(&self); }
struct Memory { count: usize }
impl Store for Memory { fn save(&self) {} }
enum State { Ready, Failed }
"#;
        let document = RustAstDocument::parse("src/lib.rs", source).expect("ast");
        let first = RustSymbolTable::build(&document, source);
        let second = RustSymbolTable::build(&document, source);

        assert_eq!(first, second);
        assert_eq!(first.find_simple("run").len(), 2);
        assert_eq!(first.find_qualified("lib::alpha::run").len(), 1);
        assert_eq!(first.find_qualified("lib::beta::run").len(), 1);
        assert!(
            first
                .find_simple("Ready")
                .iter()
                .any(|symbol| symbol.kind == RustSymbolKind::Variant)
        );
        assert!(
            first
                .find_simple("count")
                .iter()
                .any(|symbol| symbol.kind == RustSymbolKind::Field)
        );
        assert!(
            first
                .find_simple("save")
                .iter()
                .all(|symbol| symbol.kind == RustSymbolKind::Method)
        );
    }

    #[test]
    fn nearest_scope_wins_and_identity_survives_unrelated_edits() {
        let before = "fn outer() { let value = 1; { let value = 2; } }\n";
        let after = "const UNRELATED: u8 = 0;\nfn outer() { let value = 1; { let value = 2; } }\n";
        let before_doc = RustAstDocument::parse("src/lib.rs", before).expect("before");
        let after_doc = RustAstDocument::parse("src/lib.rs", after).expect("after");
        let before_table = RustSymbolTable::build(&before_doc, before);
        let after_table = RustSymbolTable::build(&after_doc, after);

        let before_outer = &before_table.find_qualified("lib::outer")[0].id;
        let after_outer = &after_table.find_qualified("lib::outer")[0].id;
        assert_eq!(before_outer, after_outer);

        let function_scope = after_table
            .scopes
            .iter()
            .find(|scope| scope.qualified_name == "lib::outer")
            .expect("function scope");
        assert!(
            !after_table
                .resolve_in_scope(function_scope.id, "value")
                .is_empty()
        );
    }

    #[test]
    fn serialization_round_trip_preserves_symbol_identity() {
        let source = "pub struct Item { pub id: u64 }\n";
        let document = RustAstDocument::parse("src/lib.rs", source).expect("ast");
        let table = RustSymbolTable::build(&document, source);
        let encoded = serde_json::to_string(&table).expect("serialize");
        let decoded: RustSymbolTable = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, table);
    }
}
