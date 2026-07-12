use std::{fs, path::Path};

use medusa_core::MedusaResult;
use tree_sitter::{Node, Parser, Tree};

use crate::{
    language::{CodeIndex, Reference, Symbol, SymbolKind},
    support::{internal, relative, source_files},
};

impl CodeIndex {
    /// Builds a deterministic Rust syntax index from repository source files.
    pub fn build(repo: &Path) -> MedusaResult<Self> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .map_err(|error| internal(format!("configure Rust parser: {error}")))?;
        let mut index = Self::default();
        for path in source_files(repo) {
            let source = fs::read_to_string(&path)?;
            let Some(tree) = parser.parse(&source, None) else {
                index.parse_errors.push(relative(repo, &path));
                continue;
            };
            if tree.root_node().has_error() {
                index.parse_errors.push(relative(repo, &path));
            }
            index_tree(repo, &path, &source, &tree, &mut index)?;
        }
        index.symbols.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then(left.start_byte.cmp(&right.start_byte))
                .then(left.name.cmp(&right.name))
        });
        for references in index.references.values_mut() {
            references.sort_by(|left, right| {
                left.path
                    .cmp(&right.path)
                    .then(left.start_byte.cmp(&right.start_byte))
            });
        }
        Ok(index)
    }
}

fn index_tree(
    repo: &Path,
    path: &Path,
    source: &str,
    tree: &Tree,
    index: &mut CodeIndex,
) -> MedusaResult<()> {
    let relative_path = relative(repo, path);
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if let Some(kind) = symbol_kind(node.kind())
            && let Some(name_node) = node.child_by_field_name("name")
        {
            let name = text(source, name_node)?.to_owned();
            index.symbols.push(Symbol {
                name: name.clone(),
                kind,
                path: relative_path.clone(),
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
                start_line: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
            });
            index
                .references
                .entry(name.clone())
                .or_default()
                .push(Reference {
                    name,
                    path: relative_path.clone(),
                    start_byte: name_node.start_byte(),
                    end_byte: name_node.end_byte(),
                    line: name_node.start_position().row + 1,
                    is_definition: true,
                });
        }
        if is_identifier(node.kind()) && !is_definition_name(node) {
            let name = text(source, node)?.to_owned();
            index
                .references
                .entry(name.clone())
                .or_default()
                .push(Reference {
                    name,
                    path: relative_path.clone(),
                    start_byte: node.start_byte(),
                    end_byte: node.end_byte(),
                    line: node.start_position().row + 1,
                    is_definition: false,
                });
        }
        let mut cursor = node.walk();
        let mut children = node.children(&mut cursor).collect::<Vec<_>>();
        children.reverse();
        stack.extend(children);
    }
    Ok(())
}

fn symbol_kind(kind: &str) -> Option<SymbolKind> {
    match kind {
        "function_item" => Some(SymbolKind::Function),
        "struct_item" => Some(SymbolKind::Struct),
        "enum_item" => Some(SymbolKind::Enum),
        "trait_item" => Some(SymbolKind::Trait),
        "mod_item" => Some(SymbolKind::Module),
        "type_item" => Some(SymbolKind::TypeAlias),
        "const_item" => Some(SymbolKind::Constant),
        "static_item" => Some(SymbolKind::Static),
        "macro_definition" => Some(SymbolKind::Macro),
        _ => None,
    }
}

fn is_identifier(kind: &str) -> bool {
    matches!(kind, "identifier" | "type_identifier" | "field_identifier")
}

fn is_definition_name(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        symbol_kind(parent.kind()).is_some()
            && parent
                .child_by_field_name("name")
                .is_some_and(|name| name.id() == node.id())
    })
}

fn text<'a>(source: &'a str, node: Node<'_>) -> MedusaResult<&'a str> {
    source
        .get(node.byte_range())
        .ok_or_else(|| internal("syntax node byte range is invalid"))
}
