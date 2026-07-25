use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{RustAstDocument, RustSymbol, RustSymbolId, RustSymbolTable, SourceRange};

/// Confidence/status for one reference resolution.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStatus {
    Resolved,
    Ambiguous,
    Unresolved,
}

/// One syntax reference and its candidate semantic targets.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustResolvedReference {
    pub name: String,
    pub range: SourceRange,
    pub scope: usize,
    pub status: ResolutionStatus,
    pub targets: Vec<RustSymbolId>,
    pub reason: String,
}

/// Definition/reference index derived from a Rust AST and scoped symbol table.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustResolutionIndex {
    pub references: Vec<RustResolvedReference>,
    pub by_definition: BTreeMap<RustSymbolId, Vec<usize>>,
}

impl RustResolutionIndex {
    #[must_use]
    pub fn build(document: &RustAstDocument, source: &str, table: &RustSymbolTable) -> Self {
        let definition_ranges = table
            .symbols
            .values()
            .map(|symbol| (symbol.name_range.start_byte, symbol.name_range.end_byte))
            .collect::<Vec<_>>();
        let mut index = Self::default();

        for node in &document.nodes {
            if !matches!(
                node.kind.as_str(),
                "identifier" | "type_identifier" | "field_identifier"
            ) || definition_ranges.contains(&(node.range.start_byte, node.range.end_byte))
            {
                continue;
            }
            let Some(raw) = source.get(node.range.start_byte..node.range.end_byte) else {
                continue;
            };
            let name = raw.trim_start_matches("r#").to_owned();
            let scope = containing_scope(table, node.range.start_byte);
            let (targets, reason) = resolve_name(document, source, table, node.id, scope, &name);
            let status = match targets.len() {
                0 => ResolutionStatus::Unresolved,
                1 => ResolutionStatus::Resolved,
                _ => ResolutionStatus::Ambiguous,
            };
            let reference_id = index.references.len();
            for target in &targets {
                index
                    .by_definition
                    .entry(target.clone())
                    .or_default()
                    .push(reference_id);
            }
            index.references.push(RustResolvedReference {
                name,
                range: node.range,
                scope,
                status,
                targets,
                reason,
            });
        }
        index
    }

    #[must_use]
    pub fn references_to(&self, id: &RustSymbolId) -> Vec<&RustResolvedReference> {
        self.by_definition
            .get(id)
            .into_iter()
            .flatten()
            .filter_map(|reference| self.references.get(*reference))
            .collect()
    }

    #[must_use]
    pub fn definition_for(&self, reference: usize) -> Option<&RustSymbolId> {
        let reference = self.references.get(reference)?;
        (reference.status == ResolutionStatus::Resolved).then(|| &reference.targets[0])
    }
}

fn resolve_name(
    document: &RustAstDocument,
    source: &str,
    table: &RustSymbolTable,
    node_id: usize,
    scope: usize,
    name: &str,
) -> (Vec<RustSymbolId>, String) {
    if let Some(path) = qualified_path(document, source, node_id) {
        let exact = qualified_candidates(table, scope, &path);
        if !exact.is_empty() {
            return (exact, format!("qualified path `{path}`"));
        }
    }

    let lexical = table
        .resolve_in_scope(scope, name)
        .into_iter()
        .map(|symbol| symbol.id.clone())
        .collect::<Vec<_>>();
    if !lexical.is_empty() {
        return (lexical, "nearest lexical scope".to_owned());
    }

    let global = table
        .find_simple(name)
        .into_iter()
        .filter(|symbol| visible_from(table, symbol, scope))
        .map(|symbol| symbol.id.clone())
        .collect::<Vec<_>>();
    if global.is_empty() {
        (global, "no visible declaration found".to_owned())
    } else {
        (global, "visible workspace candidates".to_owned())
    }
}

fn qualified_candidates(table: &RustSymbolTable, scope: usize, path: &str) -> Vec<RustSymbolId> {
    let mut prefixes = Vec::new();
    let mut cursor = Some(scope);
    while let Some(id) = cursor {
        prefixes.push(table.scopes[id].qualified_name.clone());
        cursor = table.scopes[id].parent;
    }
    prefixes.push(String::new());

    for prefix in prefixes {
        let candidate = if prefix.is_empty() {
            path.to_owned()
        } else {
            format!("{prefix}::{path}")
        };
        let matches = table
            .find_qualified(&candidate)
            .into_iter()
            .map(|symbol| symbol.id.clone())
            .collect::<Vec<_>>();
        if !matches.is_empty() {
            return matches;
        }
    }

    table
        .symbols
        .values()
        .filter(|symbol| symbol.qualified_name.ends_with(&format!("::{path}")))
        .map(|symbol| symbol.id.clone())
        .collect()
}

fn qualified_path(document: &RustAstDocument, source: &str, node_id: usize) -> Option<String> {
    let node = document.node(node_id)?;
    let parent = node.parent.and_then(|id| document.node(id))?;
    if !matches!(
        parent.kind.as_str(),
        "scoped_identifier" | "scoped_type_identifier"
    ) || node.range.end_byte != parent.range.end_byte
    {
        return None;
    }
    source
        .get(parent.range.start_byte..parent.range.end_byte)
        .map(|value| value.replace(char::is_whitespace, ""))
}

fn containing_scope(table: &RustSymbolTable, byte: usize) -> usize {
    table
        .scopes
        .iter()
        .filter_map(|scope| {
            let owner = scope.owner.as_ref().and_then(|id| table.symbol(id))?;
            (owner.range.start_byte <= byte && byte <= owner.range.end_byte)
                .then_some((scope.id, owner.range.end_byte - owner.range.start_byte))
        })
        .min_by_key(|(_, width)| *width)
        .map_or(0, |(scope, _)| scope)
}

fn visible_from(table: &RustSymbolTable, symbol: &RustSymbol, scope: usize) -> bool {
    if symbol.public || symbol.scope == scope {
        return true;
    }
    let mut cursor = Some(scope);
    while let Some(id) = cursor {
        if id == symbol.scope {
            return true;
        }
        cursor = table.scopes[id].parent;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(source: &str) -> (RustSymbolTable, RustResolutionIndex) {
        let ast = RustAstDocument::parse("src/lib.rs", source).expect("ast");
        let table = RustSymbolTable::build(&ast, source);
        let resolution = RustResolutionIndex::build(&ast, source, &table);
        (table, resolution)
    }

    #[test]
    fn duplicate_names_resolve_to_the_nearest_module() {
        let source = r#"
mod alpha { pub fn run() {} pub fn call() { run(); } }
mod beta { pub fn run() {} pub fn call() { run(); } }
"#;
        let (table, resolution) = resolve(source);
        let alpha = table.find_qualified("lib::alpha::run")[0];
        let beta = table.find_qualified("lib::beta::run")[0];
        assert_eq!(resolution.references_to(&alpha.id).len(), 1);
        assert_eq!(resolution.references_to(&beta.id).len(), 1);
        assert!(resolution.references.iter().all(|reference| {
            reference.name != "run" || reference.status == ResolutionStatus::Resolved
        }));
    }

    #[test]
    fn qualified_paths_select_exact_symbols() {
        let source = r#"
mod alpha { pub fn run() {} }
mod beta { pub fn run() {} }
fn call() { alpha::run(); beta::run(); }
"#;
        let (table, resolution) = resolve(source);
        for qualified in ["lib::alpha::run", "lib::beta::run"] {
            let symbol = table.find_qualified(qualified)[0];
            assert_eq!(resolution.references_to(&symbol.id).len(), 1);
        }
    }

    #[test]
    fn unresolved_and_ambiguous_references_are_explicit() {
        let source = r#"
mod alpha { pub fn run() {} }
mod beta { pub fn run() {} }
fn call() { run(); missing(); }
"#;
        let (_, resolution) = resolve(source);
        assert!(resolution.references.iter().any(|reference| {
            reference.name == "run" && reference.status == ResolutionStatus::Ambiguous
        }));
        assert!(resolution.references.iter().any(|reference| {
            reference.name == "missing" && reference.status == ResolutionStatus::Unresolved
        }));
    }

    #[test]
    fn serialization_round_trip_preserves_targets() {
        let (_, resolution) = resolve("fn answer() {} fn call() { answer(); }");
        let encoded = serde_json::to_string(&resolution).expect("serialize");
        let decoded: RustResolutionIndex = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, resolution);
    }
}
