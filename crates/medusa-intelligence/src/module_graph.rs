use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::{RustAstDocument, SourceRange};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RustDependencyKind {
    Module,
    Use,
    ExternCrate,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct RustDependencyEdge {
    pub from: String,
    pub to: String,
    pub kind: RustDependencyKind,
    pub range: SourceRange,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustModuleGraph {
    pub modules: BTreeSet<String>,
    pub edges: Vec<RustDependencyEdge>,
    pub outgoing: BTreeMap<String, Vec<usize>>,
    pub incoming: BTreeMap<String, Vec<usize>>,
}

impl RustModuleGraph {
    #[must_use]
    pub fn build(document: &RustAstDocument, source: &str) -> Self {
        let root = module_name(&document.path);
        let mut graph = Self::default();
        graph.modules.insert(root.clone());

        for node in &document.nodes {
            let kind = match node.kind.as_str() {
                "mod_item" => RustDependencyKind::Module,
                "use_declaration" => RustDependencyKind::Use,
                "extern_crate_declaration" => RustDependencyKind::ExternCrate,
                _ => continue,
            };
            let Some(text) = source.get(node.range.start_byte..node.range.end_byte) else {
                continue;
            };
            for target in dependency_targets(text, &kind) {
                graph.modules.insert(target.clone());
                let edge_id = graph.edges.len();
                graph
                    .outgoing
                    .entry(root.clone())
                    .or_default()
                    .push(edge_id);
                graph
                    .incoming
                    .entry(target.clone())
                    .or_default()
                    .push(edge_id);
                graph.edges.push(RustDependencyEdge {
                    from: root.clone(),
                    to: target,
                    kind: kind.clone(),
                    range: node.range,
                });
            }
        }
        graph.edges.sort();
        graph.reindex();
        graph
    }

    #[must_use]
    pub fn dependencies_of(&self, module: &str) -> Vec<&RustDependencyEdge> {
        self.outgoing
            .get(module)
            .into_iter()
            .flatten()
            .filter_map(|id| self.edges.get(*id))
            .collect()
    }

    #[must_use]
    pub fn dependents_of(&self, module: &str) -> Vec<&RustDependencyEdge> {
        self.incoming
            .get(module)
            .into_iter()
            .flatten()
            .filter_map(|id| self.edges.get(*id))
            .collect()
    }

    #[must_use]
    pub fn transitive_dependencies(&self, module: &str) -> BTreeSet<String> {
        self.walk(module, true)
    }

    #[must_use]
    pub fn transitive_dependents(&self, module: &str) -> BTreeSet<String> {
        self.walk(module, false)
    }

    fn walk(&self, start: &str, forward: bool) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::from([start.to_owned()]);
        while let Some(current) = queue.pop_front() {
            let edges = if forward {
                self.outgoing.get(&current)
            } else {
                self.incoming.get(&current)
            };
            for edge in edges
                .into_iter()
                .flatten()
                .filter_map(|id| self.edges.get(*id))
            {
                let next = if forward { &edge.to } else { &edge.from };
                if seen.insert(next.clone()) {
                    queue.push_back(next.clone());
                }
            }
        }
        seen.remove(start);
        seen
    }

    fn reindex(&mut self) {
        self.outgoing.clear();
        self.incoming.clear();
        for (id, edge) in self.edges.iter().enumerate() {
            self.outgoing.entry(edge.from.clone()).or_default().push(id);
            self.incoming.entry(edge.to.clone()).or_default().push(id);
        }
    }
}

fn module_name(path: &std::path::Path) -> String {
    path.file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("crate")
        .to_owned()
}

fn dependency_targets(text: &str, kind: &RustDependencyKind) -> Vec<String> {
    match kind {
        RustDependencyKind::Module => text
            .strip_prefix("mod ")
            .and_then(|v| {
                v.split(|c: char| c == ';' || c == '{' || c.is_whitespace())
                    .find(|p| !p.is_empty())
            })
            .map(|v| vec![v.to_owned()])
            .unwrap_or_default(),
        RustDependencyKind::ExternCrate => text
            .strip_prefix("extern crate ")
            .and_then(|v| {
                v.split(|c: char| c == ';' || c.is_whitespace())
                    .find(|p| !p.is_empty())
            })
            .map(|v| vec![v.to_owned()])
            .unwrap_or_default(),
        RustDependencyKind::Use => {
            let value = text.trim_start_matches("use ").trim_end_matches(';').trim();
            let root = value
                .trim_start_matches("::")
                .split("::")
                .next()
                .unwrap_or_default();
            if root.is_empty() {
                Vec::new()
            } else {
                vec![root.to_owned()]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_forward_reverse_and_transitive_module_edges() {
        let source = "mod api; use api::service; extern crate serde;";
        let ast = RustAstDocument::parse("src/lib.rs", source).expect("ast");
        let graph = RustModuleGraph::build(&ast, source);
        assert_eq!(graph.dependencies_of("lib").len(), 3);
        assert_eq!(graph.dependents_of("api").len(), 2);
        assert!(graph.transitive_dependencies("lib").contains("serde"));
    }

    #[test]
    fn serialization_is_deterministic() {
        let source = "use crate::alpha; mod beta;";
        let ast = RustAstDocument::parse("src/lib.rs", source).expect("ast");
        let graph = RustModuleGraph::build(&ast, source);
        let encoded = serde_json::to_string(&graph).expect("serialize");
        let decoded: RustModuleGraph = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, graph);
    }
}
