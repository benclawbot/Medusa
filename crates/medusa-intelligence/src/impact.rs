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
