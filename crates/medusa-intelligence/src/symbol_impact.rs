use std::collections::{BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::{SemanticGraph, SymbolId};

/// Symbol-level impact derived from the semantic call and dependency graphs.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SymbolImpact {
    /// Symbols explicitly changed by the edit.
    pub changed: Vec<SymbolId>,
    /// Direct and transitive callers that can observe the change.
    pub callers: Vec<SymbolId>,
    /// Test files reachable from the changed symbols and their callers.
    pub test_files: Vec<std::path::PathBuf>,
}

impl SemanticGraph {
    /// Returns the symbols directly called by `caller`.
    #[must_use]
    pub fn direct_callees(&self, caller: &SymbolId) -> Vec<SymbolId> {
        self.calls
            .iter()
            .filter(|edge| &edge.caller == caller)
            .map(|edge| edge.callee.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Returns symbols that directly call `callee`.
    #[must_use]
    pub fn direct_callers(&self, callee: &SymbolId) -> Vec<SymbolId> {
        self.calls
            .iter()
            .filter(|edge| &edge.callee == callee)
            .map(|edge| edge.caller.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Returns direct and transitive callers in deterministic order.
    #[must_use]
    pub fn transitive_callers(&self, symbols: &[SymbolId]) -> Vec<SymbolId> {
        let mut seen = symbols.iter().cloned().collect::<BTreeSet<_>>();
        let mut callers = BTreeSet::new();
        let mut pending = symbols.iter().cloned().collect::<VecDeque<_>>();

        while let Some(symbol) = pending.pop_front() {
            for caller in self.direct_callers(&symbol) {
                if seen.insert(caller.clone()) {
                    callers.insert(caller.clone());
                    pending.push_back(caller);
                }
            }
        }

        callers.into_iter().collect()
    }

    /// Computes symbol-level blast radius and the narrowest known test files.
    #[must_use]
    pub fn impact_for_symbols(&self, changed: &[SymbolId]) -> SymbolImpact {
        let changed = changed.iter().cloned().collect::<BTreeSet<_>>();
        let callers = self.transitive_callers(&changed.iter().cloned().collect::<Vec<_>>());
        let mut affected_paths = changed
            .iter()
            .map(|symbol| symbol.path.clone())
            .collect::<BTreeSet<_>>();
        affected_paths.extend(callers.iter().map(|symbol| symbol.path.clone()));
        let test_files =
            self.impacted_test_files(&affected_paths.iter().cloned().collect::<Vec<_>>());

        SymbolImpact {
            changed: changed.into_iter().collect(),
            callers,
            test_files,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use crate::{CodeIndex, SemanticGraph, SymbolId};

    #[test]
    fn exposes_reverse_call_graph_and_symbol_test_impact() {
        let repository = tempfile::tempdir().expect("repository");
        fs::create_dir_all(repository.path().join("src")).expect("src");
        fs::create_dir_all(repository.path().join("tests")).expect("tests");
        fs::write(
            repository.path().join("src/core.rs"),
            "pub fn leaf() -> u8 { 42 }\n",
        )
        .expect("core");
        fs::write(
            repository.path().join("src/lib.rs"),
            "mod core;\npub fn middle() -> u8 { core::leaf() }\npub fn facade() -> u8 { middle() }\n",
        )
        .expect("lib");
        fs::write(
            repository.path().join("tests/api.rs"),
            "use fixture::facade;\nfn api_test() { assert_eq!(facade(), 42); }\n",
        )
        .expect("test");

        let index = CodeIndex::build(repository.path()).expect("index");
        let graph = SemanticGraph::build(&index);
        let leaf: SymbolId = index
            .definitions("leaf")
            .into_iter()
            .next()
            .map(Into::into)
            .expect("leaf");
        let middle: SymbolId = index
            .definitions("middle")
            .into_iter()
            .next()
            .map(Into::into)
            .expect("middle");
        let facade: SymbolId = index
            .definitions("facade")
            .into_iter()
            .next()
            .map(Into::into)
            .expect("facade");
        let api_test: SymbolId = index
            .definitions("api_test")
            .into_iter()
            .next()
            .map(Into::into)
            .expect("api_test");

        assert_eq!(graph.direct_callers(&leaf), vec![middle.clone()]);
        assert_eq!(graph.direct_callees(&middle), vec![leaf.clone()]);
        assert_eq!(
            graph.transitive_callers(std::slice::from_ref(&leaf)),
            vec![api_test.clone(), facade.clone(), middle.clone()]
        );

        let impact = graph.impact_for_symbols(&[leaf]);
        assert_eq!(impact.callers, vec![api_test, facade, middle]);
        assert_eq!(impact.test_files, vec![PathBuf::from("tests/api.rs")]);
    }
}
