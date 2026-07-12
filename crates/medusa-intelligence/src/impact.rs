use std::{collections::BTreeSet, path::PathBuf};

use serde::{Deserialize, Serialize};

/// Deterministic test-impact recommendation for changed files.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TestImpact {
    pub commands: Vec<String>,
    pub reasons: Vec<String>,
}

#[must_use]
pub fn select_tests(changed_paths: &[PathBuf]) -> TestImpact {
    let mut commands = BTreeSet::new();
    let mut reasons = BTreeSet::new();
    for path in changed_paths {
        let text = path.to_string_lossy();
        if path.extension().is_some_and(|ext| ext == "rs") {
            commands.insert("cargo test --workspace --all-features".to_owned());
            reasons.insert(format!("Rust source changed: {text}"));
        }
        if text.contains("Cargo.toml") || text.contains("Cargo.lock") {
            commands.insert("cargo test --workspace --all-features".to_owned());
            commands.insert(
                "cargo clippy --workspace --all-targets --all-features -- -D warnings".to_owned(),
            );
            reasons.insert(format!(
                "Rust dependency or workspace metadata changed: {text}"
            ));
        }
        if text.starts_with(".github/workflows/") {
            commands.insert("cargo test --workspace --all-features".to_owned());
            reasons.insert(format!("CI workflow changed: {text}"));
        }
    }
    TestImpact {
        commands: commands.into_iter().collect(),
        reasons: reasons.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_unrelated_changes_select_no_tests() {
        assert_eq!(
            select_tests(&[]),
            TestImpact {
                commands: Vec::new(),
                reasons: Vec::new(),
            }
        );
        assert_eq!(
            select_tests(&[PathBuf::from("README.md")]),
            TestImpact {
                commands: Vec::new(),
                reasons: Vec::new(),
            }
        );
    }

    #[test]
    fn rust_dependency_and_workflow_changes_are_deduplicated_and_sorted() {
        let impact = select_tests(&[
            PathBuf::from("src/lib.rs"),
            PathBuf::from("Cargo.toml"),
            PathBuf::from("Cargo.lock"),
            PathBuf::from(".github/workflows/ci.yml"),
            PathBuf::from("src/main.rs"),
        ]);
        assert_eq!(
            impact.commands,
            vec![
                "cargo clippy --workspace --all-targets --all-features -- -D warnings",
                "cargo test --workspace --all-features",
            ]
        );
        assert_eq!(impact.reasons.len(), 5);
        assert!(
            impact
                .reasons
                .iter()
                .any(|reason| reason == "CI workflow changed: .github/workflows/ci.yml")
        );
        assert!(
            impact
                .reasons
                .iter()
                .any(|reason| reason == "Rust source changed: src/lib.rs")
        );
    }
}
