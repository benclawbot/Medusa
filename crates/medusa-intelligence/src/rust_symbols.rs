use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser};

use crate::{
    rust_ast::{RustAstIndex, SourcePosition, SourceRange},
    support::{hash, internal},
};

/// Stable identity for a Rust lexical or semantic scope.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RustScopeId(pub String);

/// Stable identity for a Rust symbol.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RustSymbolId(pub String);

/// Semantic category of a Rust scope.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum RustScopeKind {
    File,
    Module,
    Type,
    Trait,
    Impl,
    Function,
    Block,
}

/// Semantic category of an indexed Rust symbol.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum RustSymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Variant,
    Trait,
    AssociatedFunction,
    AssociatedType,
    Constant,
    Static,
    TypeAlias,
    Module,
    Field,
    Parameter,
    Local,
}

/// One deterministic lexical or semantic scope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustScope {
    pub id: RustScopeId,
    pub parent: Option<RustScopeId>,
    pub kind: RustScopeKind,
    pub name: String,
    pub qualified_name: String,
    pub path: PathBuf,
    pub range: SourceRange,
    pub owner: Option<RustSymbolId>,
}

/// One declaration or binding indexed from Rust source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustSymbol {
    pub id: RustSymbolId,
    pub scope: RustScopeId,
    pub owner: Option<RustSymbolId>,
    pub kind: RustSymbolKind,
    pub name: String,
    pub qualified_name: String,
    pub visibility: Option<String>,
    pub path: PathBuf,
    pub declaration: SourceRange,
    pub definition: SourceRange,
}

/// Serializable deterministic symbol table built from the repository Rust AST.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustSymbolTable {
    symbols: BTreeMap<RustSymbolId, RustSymbol>,
    scopes: BTreeMap<RustScopeId, RustScope>,
    by_qualified_name: BTreeMap<String, Vec<RustSymbolId>>,
    by_simple_name: BTreeMap<String, Vec<RustSymbolId>>,
    by_file: BTreeMap<PathBuf, Vec<RustSymbolId>>,
    by_scope: BTreeMap<RustScopeId, Vec<RustSymbolId>>,
}

impl RustSymbolTable {
    /// Builds the scoped symbol table for every file present in an AST index.
    pub fn build(repo: &Path, ast: &RustAstIndex) -> MedusaResult<Self> {
        let mut table = Self::default();
        for (path, _) in ast.files() {
            let source = fs::read_to_string(repo.join(path))?;
            table.index_file(path, &source)?;
        }
        table.normalize();
        Ok(table)
    }

    /// Returns a symbol by exact stable identity.
    #[must_use]
    pub fn symbol(&self, id: &RustSymbolId) -> Option<&RustSymbol> {
        self.symbols.get(id)
    }

    /// Returns a scope by exact stable identity.
    #[must_use]
    pub fn scope(&self, id: &RustScopeId) -> Option<&RustScope> {
        self.scopes.get(id)
    }

    /// Looks up symbols by fully qualified name in deterministic order.
    #[must_use]
    pub fn qualified(&self, name: &str) -> Vec<&RustSymbol> {
        self.resolve(self.by_qualified_name.get(name))
    }

    /// Looks up symbols by simple name in deterministic shadowing order.
    #[must_use]
    pub fn named(&self, name: &str) -> Vec<&RustSymbol> {
        self.resolve(self.by_simple_name.get(name))
    }

    /// Returns all symbols declared in a repository-relative file.
    #[must_use]
    pub fn in_file(&self, path: &Path) -> Vec<&RustSymbol> {
        self.resolve(self.by_file.get(path))
    }

    /// Returns all symbols declared directly in a scope.
    #[must_use]
    pub fn in_scope(&self, scope: &RustScopeId) -> Vec<&RustSymbol> {
        self.resolve(self.by_scope.get(scope))
    }

    /// Resolves the nearest visible declaration by walking outward through parent scopes.
    #[must_use]
    pub fn resolve_visible(&self, scope: &RustScopeId, name: &str) -> Option<&RustSymbol> {
        let mut current = Some(scope);
        while let Some(scope_id) = current {
            if let Some(ids) = self.by_scope.get(scope_id) {
                if let Some(symbol) = ids
                    .iter()
                    .filter_map(|id| self.symbols.get(id))
                    .find(|symbol| symbol.name == name)
                {
                    return Some(symbol);
                }
            }
            current = self
                .scopes
                .get(scope_id)
                .and_then(|candidate| candidate.parent.as_ref());
        }
        None
    }

    /// Iterates all symbols in exact identity order.
    pub fn symbols(&self) -> impl Iterator<Item = &RustSymbol> {
        self.symbols.values()
    }

    /// Iterates all scopes in exact identity order.
    pub fn scopes(&self) -> impl Iterator<Item = &RustScope> {
        self.scopes.values()
    }

    fn resolve(&self, ids: Option<&Vec<RustSymbolId>>) -> Vec<&RustSymbol> {
        ids.into_iter()
            .flatten()
            .filter_map(|id| self.symbols.get(id))
            .collect()
    }

    fn index_file(&mut self, path: &Path, source: &str) -> MedusaResult<()> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .map_err(|error| internal(format!("configure Rust parser: {error}")))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| internal("Rust parser returned no tree"))?;
        let root = tree.root_node();
        let module = file_module_path(path);
        let root_id = scope_id(path, RustScopeKind::File, &module, None);
        self.scopes.insert(
            root_id.clone(),
            RustScope {
                id: root_id.clone(),
                parent: None,
                kind: RustScopeKind::File,
                name: module.rsplit("::").next().unwrap_or("crate").to_owned(),
                qualified_name: module,
                path: path.to_path_buf(),
                range: range(root),
                owner: None,
            },
        );
        let mut context = IndexContext::new(path, source, root_id);
        self.walk(root, &mut context)?;
        Ok(())
    }

    fn walk(&mut self, node: Node<'_>, context: &mut IndexContext<'_>) -> MedusaResult<()> {
        let mut child_scope = None;
        let mut child_owner = context.owner.clone();

        if let Some(kind) = item_kind(node, context) {
            if let Some(name) = node_name(node, context.source) {
                let symbol = self.insert_symbol(node, name, kind, context)?;
                child_owner = Some(symbol.id.clone());
                if let Some(scope_kind) = owned_scope_kind(node.kind()) {
                    child_scope = Some(self.insert_scope(
                        node,
                        scope_kind,
                        symbol.name.clone(),
                        Some(symbol.id),
                        context,
                    ));
                }
            }
        } else if node.kind() == "let_declaration" {
            if let Some(pattern) = node.child_by_field_name("pattern") {
                for name in binding_names(pattern, context.source) {
                    self.insert_symbol(node, name, RustSymbolKind::Local, context)?;
                }
            }
        } else if node.kind() == "parameter" || node.kind() == "self_parameter" {
            let pattern = node.child_by_field_name("pattern").unwrap_or(node);
            for name in binding_names(pattern, context.source) {
                self.insert_symbol(node, name, RustSymbolKind::Parameter, context)?;
            }
        } else if node.kind() == "block" {
            let ordinal = context.next_block();
            child_scope = Some(self.insert_scope(
                node,
                RustScopeKind::Block,
                format!("block#{ordinal}"),
                context.owner.clone(),
                context,
            ));
        }

        let previous_scope = context.scope.clone();
        let previous_owner = context.owner.clone();
        if let Some(scope) = child_scope {
            context.scope = scope;
            context.owner = child_owner;
        }

        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.walk(child, context)?;
        }

        context.scope = previous_scope;
        context.owner = previous_owner;
        Ok(())
    }

    fn insert_symbol(
        &mut self,
        node: Node<'_>,
        name: String,
        kind: RustSymbolKind,
        context: &mut IndexContext<'_>,
    ) -> MedusaResult<RustSymbol> {
        let scope = self
            .scopes
            .get(&context.scope)
            .ok_or_else(|| internal("symbol scope is missing"))?;
        let ordinal = context.next_symbol(&context.scope, kind, &name);
        let qualified_name = format!("{}::{name}", scope.qualified_name);
        let identity = format!(
            "{}|{:?}|{}|{}|{}",
            context.path.display(),
            kind,
            scope.qualified_name,
            name,
            ordinal
        );
        let id = RustSymbolId(hash(identity.as_bytes()));
        let symbol = RustSymbol {
            id: id.clone(),
            scope: context.scope.clone(),
            owner: context.owner.clone(),
            kind,
            name: name.clone(),
            qualified_name: qualified_name.clone(),
            visibility: visibility(node, context.source),
            path: context.path.to_path_buf(),
            declaration: range(node),
            definition: range(node),
        };
        self.symbols.insert(id.clone(), symbol.clone());
        self.by_qualified_name
            .entry(qualified_name)
            .or_default()
            .push(id.clone());
        self.by_simple_name
            .entry(name)
            .or_default()
            .push(id.clone());
        self.by_file
            .entry(context.path.to_path_buf())
            .or_default()
            .push(id.clone());
        self.by_scope
            .entry(context.scope.clone())
            .or_default()
            .push(id);
        Ok(symbol)
    }

    fn insert_scope(
        &mut self,
        node: Node<'_>,
        kind: RustScopeKind,
        name: String,
        owner: Option<RustSymbolId>,
        context: &IndexContext<'_>,
    ) -> RustScopeId {
        let parent = self.scopes.get(&context.scope).expect("parent scope");
        let qualified_name = if kind == RustScopeKind::Impl {
            format!("{}::<{name}>", parent.qualified_name)
        } else {
            format!("{}::{name}", parent.qualified_name)
        };
        let id = scope_id(context.path, kind, &qualified_name, Some(&context.scope));
        self.scopes.insert(
            id.clone(),
            RustScope {
                id: id.clone(),
                parent: Some(context.scope.clone()),
                kind,
                name,
                qualified_name,
                path: context.path.to_path_buf(),
                range: range(node),
                owner,
            },
        );
        id
    }

    fn normalize(&mut self) {
        for ids in self
            .by_qualified_name
            .values_mut()
            .chain(self.by_simple_name.values_mut())
            .chain(self.by_file.values_mut())
            .chain(self.by_scope.values_mut())
        {
            ids.sort();
            ids.dedup();
        }
    }
}

struct IndexContext<'a> {
    path: &'a Path,
    source: &'a str,
    scope: RustScopeId,
    owner: Option<RustSymbolId>,
    symbol_ordinals: BTreeMap<(RustScopeId, RustSymbolKind, String), usize>,
    block_ordinal: usize,
}

impl<'a> IndexContext<'a> {
    fn new(path: &'a Path, source: &'a str, scope: RustScopeId) -> Self {
        Self {
            path,
            source,
            scope,
            owner: None,
            symbol_ordinals: BTreeMap::new(),
            block_ordinal: 0,
        }
    }

    fn next_symbol(&mut self, scope: &RustScopeId, kind: RustSymbolKind, name: &str) -> usize {
        let value = self
            .symbol_ordinals
            .entry((scope.clone(), kind, name.to_owned()))
            .or_default();
        let ordinal = *value;
        *value += 1;
        ordinal
    }

    fn next_block(&mut self) -> usize {
        let ordinal = self.block_ordinal;
        self.block_ordinal += 1;
        ordinal
    }
}

fn item_kind(node: Node<'_>, context: &IndexContext<'_>) -> Option<RustSymbolKind> {
    let inside_impl = context
        .owner
        .as_ref()
        .and_then(|owner| owner_kind_hint(owner, context))
        .is_some_and(|kind| kind == RustScopeKind::Impl);
    Some(match node.kind() {
        "function_item" if inside_impl => RustSymbolKind::Method,
        "function_item" => RustSymbolKind::Function,
        "struct_item" => RustSymbolKind::Struct,
        "enum_item" => RustSymbolKind::Enum,
        "enum_variant" => RustSymbolKind::Variant,
        "trait_item" => RustSymbolKind::Trait,
        "associated_type" => RustSymbolKind::AssociatedType,
        "const_item" => RustSymbolKind::Constant,
        "static_item" => RustSymbolKind::Static,
        "type_item" => RustSymbolKind::TypeAlias,
        "mod_item" => RustSymbolKind::Module,
        "field_declaration" => RustSymbolKind::Field,
        _ => return None,
    })
}

fn owner_kind_hint(_owner: &RustSymbolId, _context: &IndexContext<'_>) -> Option<RustScopeKind> {
    None
}

fn owned_scope_kind(kind: &str) -> Option<RustScopeKind> {
    Some(match kind {
        "function_item" => RustScopeKind::Function,
        "struct_item" | "enum_item" => RustScopeKind::Type,
        "trait_item" => RustScopeKind::Trait,
        "mod_item" => RustScopeKind::Module,
        _ => return None,
    })
}

fn node_name(node: Node<'_>, source: &str) -> Option<String> {
    node.child_by_field_name("name")
        .and_then(|name| source.get(name.byte_range()))
        .map(str::to_owned)
}

fn binding_names(node: Node<'_>, source: &str) -> Vec<String> {
    let mut names = Vec::new();
    collect_binding_names(node, source, &mut names);
    names.sort();
    names.dedup();
    names
}

fn collect_binding_names(node: Node<'_>, source: &str, names: &mut Vec<String>) {
    if node.kind() == "identifier" {
        if let Some(name) = source.get(node.byte_range()) {
            if name != "self" && name != "Self" {
                names.push(name.to_owned());
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_binding_names(child, source, names);
    }
}

fn visibility(node: Node<'_>, source: &str) -> Option<String> {
    node.child_by_field_name("visibility")
        .and_then(|visibility| source.get(visibility.byte_range()))
        .map(str::to_owned)
}

fn range(node: Node<'_>) -> SourceRange {
    let start = node.start_position();
    let end = node.end_position();
    SourceRange {
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        start: SourcePosition {
            row: start.row,
            column: start.column,
        },
        end: SourcePosition {
            row: end.row,
            column: end.column,
        },
    }
}

fn scope_id(
    path: &Path,
    kind: RustScopeKind,
    qualified_name: &str,
    parent: Option<&RustScopeId>,
) -> RustScopeId {
    let identity = format!(
        "{}|{:?}|{}|{}",
        path.display(),
        kind,
        qualified_name,
        parent.map_or("root", |value| value.0.as_str())
    );
    RustScopeId(hash(identity.as_bytes()))
}

fn file_module_path(path: &Path) -> String {
    let mut components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if components.first().is_some_and(|value| value == "src") {
        components.remove(0);
    }
    if let Some(last) = components.last_mut() {
        *last = last.trim_end_matches(".rs").to_owned();
    }
    if components.last().is_some_and(|value| value == "lib" || value == "main") {
        components.pop();
    } else if components.last().is_some_and(|value| value == "mod") {
        components.pop();
    }
    let mut qualified = vec!["crate".to_owned()];
    qualified.extend(components);
    qualified.join("::")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn fixture(source: &str) -> (tempfile::TempDir, RustSymbolTable) {
        let repository = tempfile::tempdir().expect("repository");
        fs::create_dir_all(repository.path().join("src")).expect("src");
        fs::write(repository.path().join("src/lib.rs"), source).expect("source");
        let ast = RustAstIndex::build(repository.path()).expect("ast");
        let table = RustSymbolTable::build(repository.path(), &ast).expect("symbols");
        (repository, table)
    }

    #[test]
    fn indexes_nested_modules_traits_impls_locals_and_shadowing() {
        let (_repository, table) = fixture(
            r#"
            mod nested {
                pub struct User { pub name: String }
                pub trait Read { fn read(&self); }
                impl User {
                    pub fn read(&self, value: usize) {
                        let item = value;
                        { let item = item + 1; let _copy = item; }
                    }
                }
            }
            "#,
        );
        assert_eq!(table.named("User").len(), 1);
        assert_eq!(table.named("read").len(), 2);
        assert_eq!(table.named("item").len(), 2);
        assert_eq!(table.named("value").len(), 1);
        assert!(table
            .symbols()
            .any(|symbol| symbol.kind == RustSymbolKind::Trait));
        assert!(table
            .symbols()
            .any(|symbol| symbol.kind == RustSymbolKind::Field));
    }

    #[test]
    fn lookups_cover_identity_qualified_name_file_and_scope() {
        let (_repository, table) = fixture("pub mod api { pub fn answer(value: u8) -> u8 { value } }");
        let answer = table.named("answer")[0];
        assert_eq!(table.symbol(&answer.id), Some(answer));
        assert_eq!(table.qualified(&answer.qualified_name), vec![answer]);
        assert!(table.in_file(Path::new("src/lib.rs")).contains(&answer));
        assert!(table.in_scope(&answer.scope).contains(&answer));
    }

    #[test]
    fn serialization_round_trip_preserves_exact_identity() {
        let (_repository, table) = fixture("pub struct Value; pub fn make() -> Value { Value }");
        let encoded = serde_json::to_vec(&table).expect("serialize");
        let decoded: RustSymbolTable = serde_json::from_slice(&encoded).expect("deserialize");
        assert_eq!(decoded, table);
    }

    #[test]
    fn unrelated_file_edits_preserve_existing_symbol_ids() {
        let (repository, before) = fixture("pub fn stable() {}\n");
        fs::write(repository.path().join("src/other.rs"), "pub fn unrelated() {}\n")
            .expect("other");
        let ast = RustAstIndex::build(repository.path()).expect("ast");
        let after = RustSymbolTable::build(repository.path(), &ast).expect("symbols");
        assert_eq!(before.named("stable")[0].id, after.named("stable")[0].id);
    }
}
