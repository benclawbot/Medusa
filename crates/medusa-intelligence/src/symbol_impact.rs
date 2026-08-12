use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

use crate::{RustCallGraph, RustSymbolId, RustSymbolKind, RustSymbolTable};

/// One indexed Rust source file participating in workspace impact analysis.
pub struct RustImpactFile<'a> {
    pub symbols: &'a RustSymbolTable,
    pub call_graph: &'a RustCallGraph,
}

/// Deterministic verification scope derived from changed symbols rather than files alone.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustSymbolImpact {
    pub changed_symbols: Vec<RustSymbolId>,
    pub affected_symbols: Vec<RustSymbolId>,
    pub affected_paths: Vec<PathBuf>,
    pub test_symbols: Vec<RustSymbolId>,
    pub commands: Vec<String>,
    pub reasons: Vec<String>,
}

/// Compute reverse-call impact and the narrowest known Cargo verification commands.
#[must_use]
pub fn analyze_rust_symbol_impact(
    files: &[RustImpactFile<'_>],
    changed_symbols: &[RustSymbolId],
) -> RustSymbolImpact {
    let mut symbols = BTreeMap::new();
    let mut callers = BTreeMap::<RustSymbolId, BTreeSet<RustSymbolId>>::new();

    for file in files {
        for (id, symbol) in &file.symbols.symbols {
            symbols.insert(id.clone(), symbol);
        }
        for edge in &file.call_graph.edges {
            callers
                .entry(edge.callee.clone())
                .or_default()
                .insert(edge.caller.clone());
        }
    }

    let changed = changed_symbols.iter().cloned().collect::<BTreeSet<_>>();
    let mut affected = changed.clone();
    let mut queue = changed.iter().cloned().collect::<VecDeque<_>>();
    while let Some(current) = queue.pop_front() {
        for caller in callers.get(&current).into_iter().flatten() {
            if affected.insert(caller.clone()) {
                queue.push_back(caller.clone());
            }
        }
    }

    let affected_paths = affected
        .iter()
        .filter_map(|id| symbols.get(id).map(|symbol| symbol.path.clone()))
        .collect::<BTreeSet<_>>();
    let test_symbols = affected
        .iter()
        .filter(|id| {
            symbols.get(*id).is_some_and(|symbol| {
                matches!(
                    symbol.kind,
                    RustSymbolKind::Function | RustSymbolKind::Method
                ) && (symbol.name.starts_with("test_")
                    || symbol
                        .path
                        .components()
                        .any(|part| part.as_os_str() == "tests"))
            })
        })
        .cloned()
        .collect::<BTreeSet<_>>();

    let mut commands = BTreeSet::new();
    let mut reasons = BTreeSet::new();
    for test in &test_symbols {
        let Some(symbol) = symbols.get(test) else {
            continue;
        };
        if let Some(package) = crate_name(&symbol.path) {
            if symbol
                .path
                .components()
                .any(|part| part.as_os_str() == "tests")
            {
                if let Some(name) = symbol.path.file_stem().and_then(|name| name.to_str()) {
                    commands.insert(format!("cargo test -p {package} --test {name}"));
                }
            } else {
                commands.insert(format!("cargo test -p {package} {}", symbol.name));
            }
        } else {
            commands.insert(format!("cargo test {}", symbol.name));
        }
        reasons.insert(format!(
            "Changed symbol reaches test symbol {} in {}",
            symbol.qualified_name,
            symbol.path.display()
        ));
    }

    if commands.is_empty() {
        for path in &affected_paths {
            if let Some(package) = crate_name(path) {
                commands.insert(format!("cargo test -p {package} --all-features"));
                reasons.insert(format!(
                    "Affected symbol belongs to package {package}: {}",
                    path.display()
                ));
            }
        }
    }

    RustSymbolImpact {
        changed_symbols: changed.into_iter().collect(),
        affected_symbols: affected.into_iter().collect(),
        affected_paths: affected_paths.into_iter().collect(),
        test_symbols: test_symbols.into_iter().collect(),
        commands: commands.into_iter().collect(),
        reasons: reasons.into_iter().collect(),
    }
}

fn crate_name(path: &std::path::Path) -> Option<&str> {
    let mut components = path.components();
    while let Some(component) = components.next() {
        if component.as_os_str() == "crates" {
            return components.next()?.as_os_str().to_str();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RustAstDocument, RustResolutionIndex};

    fn indexed(path: &str, source: &str) -> (RustSymbolTable, RustCallGraph) {
        let ast = RustAstDocument::parse(path, source).expect("ast");
        let table = RustSymbolTable::build(&ast, source);
        let resolution = RustResolutionIndex::build(&ast, source, &table);
        let graph = RustCallGraph::build(&ast, &table, &resolution);
        (table, graph)
    }

    #[test]
    fn expands_reverse_callers_and_selects_targeted_test() {
        let source = "fn leaf() {} fn middle() { leaf(); } fn test_leaf() { middle(); }";
        let (table, graph) = indexed("crates/widget/src/lib.rs", source);
        let leaf = table.find_simple("leaf")[0].id.clone();
        let files = [RustImpactFile {
            symbols: &table,
            call_graph: &graph,
        }];
        let impact = analyze_rust_symbol_impact(&files, &[leaf]);

        assert_eq!(impact.affected_symbols.len(), 3);
        assert_eq!(impact.test_symbols.len(), 1);
        assert_eq!(impact.commands, vec!["cargo test -p widget test_leaf"]);
    }

    #[test]
    fn falls_back_to_package_scope_when_no_test_symbol_is_known() {
        let source = "fn leaf() {} fn caller() { leaf(); }";
        let (table, graph) = indexed("crates/widget/src/lib.rs", source);
        let leaf = table.find_simple("leaf")[0].id.clone();
        let files = [RustImpactFile {
            symbols: &table,
            call_graph: &graph,
        }];
        let impact = analyze_rust_symbol_impact(&files, &[leaf]);

        assert_eq!(impact.commands, vec!["cargo test -p widget --all-features"]);
    }

    #[test]
    fn serialization_is_deterministic() {
        let source = "fn changed() {}";
        let (table, graph) = indexed("src/lib.rs", source);
        let changed = table.find_simple("changed")[0].id.clone();
        let files = [RustImpactFile {
            symbols: &table,
            call_graph: &graph,
        }];
        let impact = analyze_rust_symbol_impact(&files, &[changed]);
        let encoded = serde_json::to_string(&impact).expect("serialize");
        let decoded: RustSymbolImpact = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, impact);
    }
}
