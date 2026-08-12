use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{CodeIndex, SemanticGraph};

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

/// Uses semantic file dependencies to select the narrowest known tests for source changes.
/// Workspace-wide checks remain mandatory for dependency, lockfile, or CI workflow changes.
#[must_use]
pub fn select_tests_with_index(index: &CodeIndex, changed_paths: &[PathBuf]) -> TestImpact {
    let broad = select_tests(changed_paths);
    if requires_workspace_validation(changed_paths) {
        return broad;
    }

    let graph = SemanticGraph::build(index);
    let impacted_tests = graph.impacted_test_files(changed_paths);
    let mut commands = BTreeSet::new();
    let mut reasons = BTreeSet::new();

    for test in impacted_tests {
        if let Some(command) = command_for_test(&test) {
            commands.insert(command);
            reasons.insert(format!(
                "Semantic dependency path reaches test: {}",
                test.display()
            ));
        }
    }

    for path in changed_paths {
        if path.extension().is_some_and(|extension| extension == "rs") {
            if let Some(package) = crate_name(path) {
                commands.insert(format!("cargo test -p {package} --all-features"));
                reasons.insert(format!(
                    "Changed Rust source belongs to package {package}: {}",
                    path.display()
                ));
            } else if commands.is_empty() {
                commands.insert("cargo test --all-features".to_owned());
                reasons.insert(format!("Changed root Rust source: {}", path.display()));
            }
        }
    }

    TestImpact {
        commands: commands.into_iter().collect(),
        reasons: reasons.into_iter().collect(),
    }
}

fn requires_workspace_validation(changed_paths: &[PathBuf]) -> bool {
    changed_paths.iter().any(|path| {
        let text = path.to_string_lossy();
        text.contains("Cargo.toml")
            || text.contains("Cargo.lock")
            || text.starts_with(".github/workflows/")
    })
}

fn command_for_test(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?;
    if extension == "py" {
        return Some(format!("python -m pytest {}", shell_path(path)));
    }
    if extension != "rs" {
        return None;
    }

    let test_name = path.file_stem()?.to_str()?;
    Some(match crate_name(path) {
        Some(package) => format!("cargo test -p {package} --test {test_name}"),
        None => format!("cargo test --test {test_name}"),
    })
}

fn crate_name(path: &Path) -> Option<&str> {
    let mut components = path.components();
    while let Some(component) = components.next() {
        if component.as_os_str() == "crates" {
            return components.next()?.as_os_str().to_str();
        }
    }
    None
}

fn shell_path(path: &Path) -> String {
    format!("\"{}\"", path.to_string_lossy().replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use std::fs;

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

    #[test]
    fn semantic_impact_selects_package_and_integration_test() {
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

        let impact = select_tests_with_index(&index, &[PathBuf::from("crates/widget/src/lib.rs")]);

        assert_eq!(
            impact.commands,
            vec![
                "cargo test -p widget --all-features",
                "cargo test -p widget --test api",
            ]
        );
        assert!(
            impact
                .reasons
                .iter()
                .any(|reason| reason.contains("api.rs"))
        );
    }

    #[test]
    fn python_source_without_impacted_test_does_not_invent_pytest() {
        let repository = tempfile::tempdir().expect("repository");
        fs::create_dir_all(repository.path().join("src")).expect("src");
        fs::write(
            repository.path().join("src/slugify.py"),
            "def slugify(value):\n    return value.lower()\n",
        )
        .expect("source");
        fs::write(
            repository.path().join("verify.py"),
            "print('repository-defined verification')\n",
        )
        .expect("verification script");
        let index = CodeIndex::build(repository.path()).expect("index");

        let impact = select_tests_with_index(&index, &[PathBuf::from("src/slugify.py")]);

        assert!(impact.commands.is_empty());
        assert!(impact.reasons.is_empty());
    }
}
