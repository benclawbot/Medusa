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

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RustScopeId(pub String);

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RustSymbolId(pub String);

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

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum RustSymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Variant,
    Trait,
    AssociatedType,
    Constant,
    Static,
    TypeAlias,
    Module,
    Field,
    Parameter,
    Local,
}

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
    pub fn build(repo: &Path, ast: &RustAstIndex) -> MedusaResult<Self> {
        let mut table = Self::default();
        for (path, _) in ast.files() {
            let source = fs::read_to_string(repo.join(path))?;
            table.index_file(path, &source)?;
        }
        table.normalize();
        Ok(table)
    }

    #[must_use]
    pub fn symbol(&self, id: &RustSymbolId) -> Option<&RustSymbol> {
        self.symbols.get(id)
    }

    #[must_use]
    pub fn scope(&self, id: &RustScopeId) -> Option<&RustScope> {
        self.scopes.get(id)
    }

    #[must_use]
    pub fn qualified(&self, name: &str) -> Vec<&RustSymbol> {
        self.resolve(self.by_qualified_name.get(name))
    }

    #[must_use]
    pub fn named(&self, name: &str) -> Vec<&RustSymbol> {
        self.resolve(self.by_simple_name.get(name))
    }

    #[must_use]
    pub fn in_file(&self, path: &Path) -> Vec<&RustSymbol> {
        self.resolve(self.by_file.get(path))
    }

    #[must_use]
    pub fn in_scope(&self, scope: &RustScopeId) -> Vec<&RustSymbol> {
        self.resolve(self.by_scope.get(scope))
    }

    pub fn symbols(&self) -> impl Iterator<Item = &RustSymbol> {
        self.symbols.values()
    }

    pub fn scopes(&self) -> impl Iterator<Item = &RustScope> {
        self.scopes.values()
    }

    #[must_use]
    pub fn resolve_visible(&self, scope: &RustScopeId, name: &str) -> Option<&RustSymbol> {
        let mut current = Some(scope);
        while let Some(scope_id) = current {
            if let Some(found) = self
                .by_scope
                .get(scope_id)
                .into_iter()
                .flatten()
                .filter_map(|id| self.symbols.get(id))
                .find(|symbol| symbol.name == name)
            {
                return Some(found);
            }
            current = self
                .scopes
                .get(scope_id)
                .and_then(|candidate| candidate.parent.as_ref());
        }
        None
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
        let qualified = file_module_path(path);
        let root_id = make_scope_id(path, RustScopeKind::File, &qualified, None);
        self.scopes.insert(
            root_id.clone(),
            RustScope {
                id: root_id.clone(),
                parent: None,
                kind: RustScopeKind::File,
                name: qualified.rsplit("::").next().unwrap_or("crate").to_owned(),
                qualified_name: qualified,
                path: path.to_path_buf(),
                range: source_range(root),
                owner: None,
            },
        );
        let mut context = Context::new(path, source, root_id, RustScopeKind::File);
        self.walk(root, &mut context)?;
        Ok(())
    }

    fn walk(&mut self, node: Node<'_>, context: &mut Context<'_>) -> MedusaResult<()> {
        let saved_scope = context.scope.clone();
        let saved_kind = context.scope_kind;
        let saved_owner = context.owner.clone();

        if node.kind() == "impl_item" {
            let name = impl_name(node, context.source);
            let scope = self.insert_scope(
                node,
                RustScopeKind::Impl,
                name,
                context.owner.clone(),
                context,
            )?;
            context.scope = scope;
            context.scope_kind = RustScopeKind::Impl;
        } else if let Some(kind) = symbol_kind(node.kind(), context.scope_kind) {
            if let Some(name) = node_name(node, context.source) {
                let symbol = self.insert_symbol(node, name, kind, context)?;
                if let Some(scope_kind) = owned_scope_kind(node.kind()) {
                    let scope = self.insert_scope(
                        node,
                        scope_kind,
                        symbol.name.clone(),
                        Some(symbol.id.clone()),
                        context,
                    )?;
                    context.scope = scope;
                    context.scope_kind = scope_kind;
                    context.owner = Some(symbol.id);
                }
            }
        } else if node.kind() == "let_declaration" {
            if let Some(pattern) = node.child_by_field_name("pattern") {
                for name in binding_names(pattern, context.source) {
                    self.insert_symbol(node, name, RustSymbolKind::Local, context)?;
                }
            }
        } else if matches!(node.kind(), "parameter" | "self_parameter") {
            let pattern = node.child_by_field_name("pattern").unwrap_or(node);
            for name in binding_names(pattern, context.source) {
                self.insert_symbol(node, name, RustSymbolKind::Parameter, context)?;
            }
        } else if node.kind() == "block" {
            let name = format!("block#{}", context.next_block());
            let scope = self.insert_scope(
                node,
                RustScopeKind::Block,
                name,
                context.owner.clone(),
                context,
            )?;
            context.scope = scope;
            context.scope_kind = RustScopeKind::Block;
        }

        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.walk(child, context)?;
        }

        context.scope = saved_scope;
        context.scope_kind = saved_kind;
        context.owner = saved_owner;
        Ok(())
    }

    fn insert_symbol(
        &mut self,
        node: Node<'_>,
        name: String,
        kind: RustSymbolKind,
        context: &mut Context<'_>,
    ) -> MedusaResult<RustSymbol> {
        let scope_id = context.scope.clone();
        let scope = self
            .scopes
            .get(&scope_id)
            .ok_or_else(|| internal("symbol scope is missing"))?;
        let scope_qualified = scope.qualified_name.clone();
        let ordinal = context.next_symbol(scope_id.clone(), kind, &name);
        let qualified_name = format!("{scope_qualified}::{name}");
        let identity = format!(
            "{}|{:?}|{}|{}|{}",
            context.path.display(),
            kind,
            scope_qualified,
            name,
            ordinal
        );
        let id = RustSymbolId(hash(identity.as_bytes()));
        let symbol = RustSymbol {
            id: id.clone(),
            scope: scope_id.clone(),
            owner: context.owner.clone(),
            kind,
            name: name.clone(),
            qualified_name: qualified_name.clone(),
            visibility: visibility(node, context.source),
            path: context.path.to_path_buf(),
            declaration: source_range(node),
            definition: source_range(node),
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
        self.by_scope.entry(scope_id).or_default().push(id);
        Ok(symbol)
    }

    fn insert_scope(
        &mut self,
        node: Node<'_>,
        kind: RustScopeKind,
        name: String,
        owner: Option<RustSymbolId>,
        context: &Context<'_>,
    ) -> MedusaResult<RustScopeId> {
        let parent = self
            .scopes
            .get(&context.scope)
            .ok_or_else(|| internal("parent scope is missing"))?;
        let qualified_name = if kind == RustScopeKind::Impl {
            format!("{}::<{}>", parent.qualified_name, name)
        } else {
            format!("{}::{}", parent.qualified_name, name)
        };
        let id = make_scope_id(context.path, kind, &qualified_name, Some(&context.scope));
        self.scopes.insert(
            id.clone(),
            RustScope {
                id: id.clone(),
                parent: Some(context.scope.clone()),
                kind,
                name,
                qualified_name,
                path: context.path.to_path_buf(),
                range: source_range(node),
                owner,
            },
        );
        Ok(id)
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

struct Context<'a> {
    path: &'a Path,
    source: &'a str,
    scope: RustScopeId,
    scope_kind: RustScopeKind,
    owner: Option<RustSymbolId>,
    ordinals: BTreeMap<(RustScopeId, RustSymbolKind, String), usize>,
    block_ordinal: usize,
}

impl<'a> Context<'a> {
    fn new(path: &'a Path, source: &'a str, scope: RustScopeId, scope_kind: RustScopeKind) -> Self {
        Self {
            path,
            source,
            scope,
            scope_kind,
            owner: None,
            ordinals: BTreeMap::new(),
            block_ordinal: 0,
        }
    }

    fn next_symbol(&mut self, scope: RustScopeId, kind: RustSymbolKind, name: &str) -> usize {
        let value = self
            .ordinals
            .entry((scope, kind, name.to_owned()))
            .or_default();
        let ordinal = *value;
        *value += 1;
        ordinal
    }

    fn next_block(&mut self) -> usize {
        let value = self.block_ordinal;
        self.block_ordinal += 1;
        value
    }
}

fn symbol_kind(kind: &str, scope: RustScopeKind) -> Option<RustSymbolKind> {
    Some(match kind {
        "function_item" if matches!(scope, RustScopeKind::Impl | RustScopeKind::Trait) => {
            RustSymbolKind::Method
        }
        "function_signature_item" if scope == RustScopeKind::Trait => RustSymbolKind::Method,
        "function_item" | "function_signature_item" => RustSymbolKind::Function,
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

fn owned_scope_kind(kind: &str) -> Option<RustScopeKind> {
    Some(match kind {
        "function_item" | "function_signature_item" => RustScopeKind::Function,
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

fn impl_name(node: Node<'_>, source: &str) -> String {
    node.child_by_field_name("type")
        .and_then(|value| source.get(value.byte_range()))
        .unwrap_or("anonymous")
        .trim()
        .to_owned()
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
            if !matches!(name, "self" | "Self") {
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
        .and_then(|value| source.get(value.byte_range()))
        .map(str::to_owned)
}

fn source_range(node: Node<'_>) -> SourceRange {
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

fn make_scope_id(
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
    let mut parts = path
        .components()
        .filter_map(|part| part.as_os_str().to_str())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if parts.first().is_some_and(|value| value == "src") {
        parts.remove(0);
    }
    if let Some(last) = parts.last_mut() {
        *last = last.trim_end_matches(".rs").to_owned();
    }
    if parts
        .last()
        .is_some_and(|value| matches!(value.as_str(), "lib" | "main" | "mod"))
    {
        parts.pop();
    }
    let mut qualified = vec!["crate".to_owned()];
    qualified.extend(parts);
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
    fn indexes_modules_impls_trait_methods_locals_and_shadowing() {
        let (_, table) = fixture(
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
        assert!(
            table
                .symbols()
                .any(|symbol| symbol.kind == RustSymbolKind::Method)
        );
        assert!(
            table
                .scopes()
                .any(|scope| scope.kind == RustScopeKind::Impl)
        );
    }

    #[test]
    fn supports_all_lookup_dimensions() {
        let (_, table) = fixture("pub mod api { pub fn answer(value: u8) -> u8 { value } }");
        let answer = table.named("answer")[0];
        assert_eq!(table.symbol(&answer.id), Some(answer));
        assert_eq!(table.qualified(&answer.qualified_name), vec![answer]);
        assert!(table.in_file(Path::new("src/lib.rs")).contains(&answer));
        assert!(table.in_scope(&answer.scope).contains(&answer));
        assert!(table.scope(&answer.scope).is_some());
        assert_eq!(table.resolve_visible(&answer.scope, "answer"), Some(answer));
    }

    #[test]
    fn serialization_preserves_identity() {
        let (_, table) = fixture("pub struct Value; pub fn make() -> Value { Value }");
        let encoded = serde_json::to_vec(&table).expect("serialize");
        let decoded: RustSymbolTable = serde_json::from_slice(&encoded).expect("deserialize");
        assert_eq!(decoded, table);
    }

    #[test]
    fn unrelated_file_edits_preserve_symbol_ids() {
        let (repository, before) = fixture("pub fn stable() {}\n");
        fs::write(
            repository.path().join("src/other.rs"),
            "pub fn unrelated() {}\n",
        )
        .expect("other");
        let ast = RustAstIndex::build(repository.path()).expect("ast");
        let after = RustSymbolTable::build(repository.path(), &ast).expect("symbols");
        assert_eq!(before.named("stable")[0].id, after.named("stable")[0].id);
    }
}
