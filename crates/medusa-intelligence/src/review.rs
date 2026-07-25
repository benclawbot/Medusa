use std::{collections::BTreeSet, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{CodeIndex, SemanticGraph, Symbol, TestImpact, select_tests_with_index};

/// Deterministic evidence bundle for reviewing a repository change.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReviewImpact {
    pub changed_paths: Vec<PathBuf>,
    pub affected_paths: Vec<PathBuf>,
    pub impacted_tests: Vec<PathBuf>,
    pub changed_symbols: Vec<String>,
    pub public_api_risk: bool,
    pub validation: TestImpact,
}

impl ReviewImpact {
    /// Builds reviewer evidence from the current semantic index and changed paths.
    #[must_use]
    pub fn analyze(index: &CodeIndex, changed_paths: &[PathBuf]) -> Self {
        let graph = SemanticGraph::build(index);
        let changed = changed_paths.iter().cloned().collect::<BTreeSet<_>>();
        let changed_symbols = index
            .symbols
            .iter()
            .filter(|symbol| changed.contains(&symbol.path))
            .map(symbol_label)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let public_api_risk = index
            .symbols
            .iter()
            .any(|symbol| changed.contains(&symbol.path) && is_public_api_candidate(symbol));

        Self {
            changed_paths: changed.into_iter().collect(),
            affected_paths: graph.affected_files(changed_paths),
            impacted_tests: graph.impacted_test_files(changed_paths),
            changed_symbols,
            public_api_risk,
            validation: select_tests_with_index(index, changed_paths),
        }
    }

    /// Returns a compact prompt fragment for an independent reviewer.
    #[must_use]
    pub fn reviewer_context(&self) -> String {
        format!(
            "SEMANTIC REVIEW IMPACT\nChanged files: [{}]\nAffected files: [{}]\nImpacted tests: [{}]\nChanged symbols: [{}]\nPublic API risk: {}\nRequired validation: [{}]",
            display_paths(&self.changed_paths),
            display_paths(&self.affected_paths),
            display_paths(&self.impacted_tests),
            self.changed_symbols.join(", "),
            self.public_api_risk,
            self.validation.commands.join(", ")
        )
    }
}

fn symbol_label(symbol: &Symbol) -> String {
    format!(
        "{}:{}:{}",
        symbol.path.display(),
        symbol.start_line,
        symbol.name
    )
}

fn is_public_api_candidate(symbol: &Symbol) -> bool {
    let path = symbol.path.to_string_lossy().replace('\\', "/");
    path.ends_with("src/lib.rs") || path.contains("/src/lib.rs")
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn review_impact_reports_transitive_tests_and_public_api_risk() {
        let repository = tempfile::tempdir().expect("repository");
        fs::create_dir_all(repository.path().join("crates/widget/src")).expect("src");
        fs::create_dir_all(repository.path().join("crates/widget/tests")).expect("tests");
        fs::write(
            repository.path().join("crates/widget/src/lib.rs"),
            "pub fn answer() -> u8 { 42 }\n",
        )
        .expect("source");
        fs::write(
            repository.path().join("crates/widget/tests/api.rs"),
            "use widget::answer;\nfn api_test() { assert_eq!(answer(), 42); }\n",
        )
        .expect("test");

        let index = CodeIndex::build(repository.path()).expect("index");
        let impact = ReviewImpact::analyze(&index, &[PathBuf::from("crates/widget/src/lib.rs")]);

        assert!(impact.public_api_risk);
        assert_eq!(
            impact.impacted_tests,
            vec![PathBuf::from("crates/widget/tests/api.rs")]
        );
        assert!(
            impact
                .changed_symbols
                .iter()
                .any(|symbol| symbol.ends_with(":answer"))
        );
        assert!(
            impact
                .reviewer_context()
                .contains("cargo test -p widget --test api")
        );
    }
}
