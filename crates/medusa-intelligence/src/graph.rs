use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{CodeIndex, Symbol, SymbolKind};

/// Stable identity for one indexed symbol definition.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SymbolId {
    pub name: String,
    pub path: PathBuf,
    pub start_line: usize,
}

impl From<&Symbol> for SymbolId {
    fn from(symbol: &Symbol) -> Self {
        Self {
            name: symbol.name.clone(),
            path: symbol.path.clone(),
            start_line: symbol.start_line,
        }
    }
}

/// A syntax-derived caller-to-callee relationship.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CallEdge {
    pub caller: SymbolId,
    pub callee: SymbolId,
}

/// A source-file dependency inferred from a reference to a definition in another file.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DependencyEdge {
    pub source: PathBuf,
    pub target: PathBuf,
    pub symbol: String,
}

/// Deterministic semantic relationships derived from a [`CodeIndex`].
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticGraph {
    pub calls: Vec<CallEdge>,
    pub dependencies: Vec<DependencyEdge>,
    pub test_files: Vec<PathBuf>,
}

impl SemanticGraph {
    /// Derives call, file-dependency, and test relationships without reparsing source files.
    #[must_use]
    pub fn build(index: &CodeIndex) -> Self {
        let definitions = definitions_by_name(index);
        let mut calls = BTreeSet::new();
        let mut dependencies = BTreeSet::new();

        for references in index.references.values() {
            for reference in references
                .iter()
                .filter(|reference| !reference.is_definition)
            {
                let Some(targets) = definitions.get(reference.name.as_str()) else {
                    continue;
                };
                for target in targets {
                    if reference.path != target.path {
                        dependencies.insert(DependencyEdge {
                            source: reference.path.clone(),
                            target: target.path.clone(),
                            symbol: reference.name.clone(),
                        });
                    }
                    if !matches!(target.kind, SymbolKind::Function | SymbolKind::Macro) {
                        continue;
                    }
                    for caller in containing_functions(index, &reference.path, reference.start_byte)
                    {
                        if caller.path != target.path || caller.start_byte != target.start_byte {
                            calls.insert(CallEdge {
                                caller: SymbolId::from(caller),
                                callee: SymbolId::from(*target),
                            });
                        }
                    }
                }
            }
        }

        let mut test_files = index
            .symbols
            .iter()
            .map(|symbol| symbol.path.clone())
            .filter(|path| is_test_path(path))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        test_files.sort();

        Self {
            calls: calls.into_iter().collect(),
            dependencies: dependencies.into_iter().collect(),
            test_files,
        }
    }

    /// Returns directly and transitively dependent files, including the changed files.
    #[must_use]
    pub fn affected_files(&self, changed_paths: &[PathBuf]) -> Vec<PathBuf> {
        let reverse = self.reverse_dependencies();
        let mut affected = changed_paths.iter().cloned().collect::<BTreeSet<_>>();
        let mut pending = affected.iter().cloned().collect::<VecDeque<_>>();
        while let Some(path) = pending.pop_front() {
            if let Some(dependents) = reverse.get(&path) {
                for dependent in dependents {
                    if affected.insert(dependent.clone()) {
                        pending.push_back(dependent.clone());
                    }
                }
            }
        }
        affected.into_iter().collect()
    }

    /// Returns test files affected through the file dependency graph.
    #[must_use]
    pub fn impacted_test_files(&self, changed_paths: &[PathBuf]) -> Vec<PathBuf> {
        let affected = self
            .affected_files(changed_paths)
            .into_iter()
            .collect::<BTreeSet<_>>();
        self.test_files
            .iter()
            .filter(|path| affected.contains(*path) || changed_paths.contains(*path))
            .cloned()
            .collect()
    }

    fn reverse_dependencies(&self) -> BTreeMap<PathBuf, BTreeSet<PathBuf>> {
        let mut reverse = BTreeMap::<PathBuf, BTreeSet<PathBuf>>::new();
        for edge in &self.dependencies {
            reverse
                .entry(edge.target.clone())
                .or_default()
                .insert(edge.source.clone());
        }
        reverse
    }
}

fn definitions_by_name(index: &CodeIndex) -> BTreeMap<&str, Vec<&Symbol>> {
    let mut definitions = BTreeMap::<&str, Vec<&Symbol>>::new();
    for symbol in &index.symbols {
        definitions.entry(&symbol.name).or_default().push(symbol);
    }
    definitions
}

fn containing_functions<'a>(
    index: &'a CodeIndex,
    path: &Path,
    byte: usize,
) -> impl Iterator<Item = &'a Symbol> {
    index.symbols.iter().filter(move |symbol| {
        symbol.path == path
            && symbol.kind == SymbolKind::Function
            && symbol.start_byte <= byte
            && byte < symbol.end_byte
    })
}

fn is_test_path(path: &Path) -> bool {
    let text = path.to_string_lossy().replace('\\', "/");
    text.starts_with("tests/")
        || text.contains("/tests/")
        || text.ends_with("_test.py")
        || text.ends_with("test.rs")
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;

    #[test]
    fn derives_calls_dependencies_and_transitive_test_impact() {
        let repository = tempfile::tempdir().expect("repository");
        fs::create_dir_all(repository.path().join("src")).expect("src");
        fs::create_dir_all(repository.path().join("tests")).expect("tests");
        fs::write(
            repository.path().join("src/core.rs"),
            "pub fn answer() -> u8 { 42 }\n",
        )
        .expect("core");
        fs::write(
            repository.path().join("src/lib.rs"),
            "mod core;\npub fn facade() -> u8 { core::answer() }\n",
        )
        .expect("lib");
        fs::write(
            repository.path().join("tests/facade.rs"),
            "use fixture::facade;\nfn facade_test() { assert_eq!(facade(), 42); }\n",
        )
        .expect("test");

        let index = CodeIndex::build(repository.path()).expect("index");
        let graph = SemanticGraph::build(&index);

        assert!(
            graph
                .calls
                .iter()
                .any(|edge| { edge.caller.name == "facade" && edge.callee.name == "answer" })
        );
        assert!(graph.dependencies.iter().any(|edge| {
            edge.source == PathBuf::from("tests/facade.rs")
                && edge.target == PathBuf::from("src/lib.rs")
        }));
        assert_eq!(
            graph.impacted_test_files(&[PathBuf::from("src/core.rs")]),
            vec![PathBuf::from("tests/facade.rs")]
        );
    }
}
