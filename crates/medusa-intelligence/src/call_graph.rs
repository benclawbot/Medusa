use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    ResolutionStatus, RustAstDocument, RustResolutionIndex, RustSymbolId, RustSymbolKind,
    RustSymbolTable, SourceRange,
};

/// One semantically resolved call edge.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustCallEdge {
    pub caller: RustSymbolId,
    pub callee: RustSymbolId,
    pub call_range: SourceRange,
    pub reference_index: usize,
}

/// Directed Rust call graph with forward and reverse indexes.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustCallGraph {
    pub edges: Vec<RustCallEdge>,
    pub callees_by_caller: BTreeMap<RustSymbolId, Vec<usize>>,
    pub callers_by_callee: BTreeMap<RustSymbolId, Vec<usize>>,
}

impl RustCallGraph {
    #[must_use]
    pub fn build(
        document: &RustAstDocument,
        table: &RustSymbolTable,
        resolution: &RustResolutionIndex,
    ) -> Self {
        let mut graph = Self::default();
        let mut seen = BTreeSet::new();

        for (reference_index, reference) in resolution.references.iter().enumerate() {
            if reference.status != ResolutionStatus::Resolved
                || !is_call_reference(
                    document,
                    reference.range.start_byte,
                    reference.range.end_byte,
                )
            {
                continue;
            }
            let Some(caller) = containing_callable(table, reference.range.start_byte) else {
                continue;
            };
            let callee = reference.targets[0].clone();
            let key = (
                caller.clone(),
                callee.clone(),
                reference.range.start_byte,
                reference.range.end_byte,
            );
            if !seen.insert(key) {
                continue;
            }

            let edge_index = graph.edges.len();
            graph.edges.push(RustCallEdge {
                caller: caller.clone(),
                callee: callee.clone(),
                call_range: reference.range,
                reference_index,
            });
            graph
                .callees_by_caller
                .entry(caller)
                .or_default()
                .push(edge_index);
            graph
                .callers_by_callee
                .entry(callee)
                .or_default()
                .push(edge_index);
        }

        graph
    }

    #[must_use]
    pub fn callees_of(&self, caller: &RustSymbolId) -> Vec<&RustCallEdge> {
        self.callees_by_caller
            .get(caller)
            .into_iter()
            .flatten()
            .filter_map(|index| self.edges.get(*index))
            .collect()
    }

    #[must_use]
    pub fn callers_of(&self, callee: &RustSymbolId) -> Vec<&RustCallEdge> {
        self.callers_by_callee
            .get(callee)
            .into_iter()
            .flatten()
            .filter_map(|index| self.edges.get(*index))
            .collect()
    }

    #[must_use]
    pub fn reachable_from(&self, root: &RustSymbolId) -> Vec<RustSymbolId> {
        let mut visited = BTreeSet::new();
        let mut stack = vec![root.clone()];
        while let Some(current) = stack.pop() {
            for edge in self.callees_of(&current) {
                if visited.insert(edge.callee.clone()) {
                    stack.push(edge.callee.clone());
                }
            }
        }
        visited.into_iter().collect()
    }
}

fn is_call_reference(document: &RustAstDocument, start: usize, end: usize) -> bool {
    let Some(node) = document
        .nodes
        .iter()
        .find(|node| node.range.start_byte == start && node.range.end_byte == end)
    else {
        return false;
    };

    std::iter::successors(node.parent, |id| {
        document.node(*id).and_then(|node| node.parent)
    })
    .filter_map(|id| document.node(id))
    .take_while(|node| {
        !matches!(
            node.kind.as_str(),
            "function_item" | "closure_expression" | "source_file"
        )
    })
    .any(|node| matches!(node.kind.as_str(), "call_expression" | "macro_invocation"))
}

fn containing_callable(table: &RustSymbolTable, byte: usize) -> Option<RustSymbolId> {
    table
        .symbols
        .values()
        .filter(|symbol| {
            matches!(
                symbol.kind,
                RustSymbolKind::Function | RustSymbolKind::Method
            ) && symbol.range.start_byte <= byte
                && byte <= symbol.range.end_byte
        })
        .min_by_key(|symbol| symbol.range.end_byte - symbol.range.start_byte)
        .map(|symbol| symbol.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RustAstDocument, RustResolutionIndex, RustSymbolTable};

    fn graph(source: &str) -> (RustSymbolTable, RustCallGraph) {
        let ast = RustAstDocument::parse("src/lib.rs", source).expect("ast");
        let table = RustSymbolTable::build(&ast, source);
        let resolution = RustResolutionIndex::build(&ast, source, &table);
        let graph = RustCallGraph::build(&ast, &table, &resolution);
        (table, graph)
    }

    #[test]
    fn builds_forward_reverse_and_transitive_call_edges() {
        let source = "fn leaf() {} fn middle() { leaf(); } fn root() { middle(); leaf(); }";
        let (table, graph) = graph(source);
        let root = &table.find_qualified("lib::root")[0].id;
        let middle = &table.find_qualified("lib::middle")[0].id;
        let leaf = &table.find_qualified("lib::leaf")[0].id;

        assert_eq!(graph.callees_of(root).len(), 2);
        assert_eq!(graph.callers_of(leaf).len(), 2);
        assert_eq!(graph.callers_of(middle).len(), 1);
        assert_eq!(graph.reachable_from(root).len(), 2);
    }

    #[test]
    fn excludes_non_call_references_and_ambiguous_calls() {
        let source = r#"
mod alpha { pub fn run() {} }
mod beta { pub fn run() {} }
fn value() { let _name = alpha::run; }
fn ambiguous() { run(); }
"#;
        let (_, graph) = graph(source);
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn serialization_round_trip_preserves_indexes() {
        let (_, graph) = graph("fn answer() {} fn call() { answer(); }");
        let encoded = serde_json::to_string(&graph).expect("serialize");
        let decoded: RustCallGraph = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, graph);
    }
}
