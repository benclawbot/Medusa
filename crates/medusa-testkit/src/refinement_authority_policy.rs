//! Static architecture guardrails for the single refinement authority.

use std::{
    fs,
    path::{Path, PathBuf},
};

const FORBIDDEN_PATHS: &[&str] = &[
    ".medusa/learning-review",
    ".medusa/learnings.json",
    ".medusa/engineering/improvements.json",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorityPolicyReport {
    pub violations: Vec<String>,
}

impl AuthorityPolicyReport {
    #[must_use]
    pub const fn is_compliant(&self) -> bool {
        self.violations.is_empty()
    }
}

pub fn inspect_repository(repo: &Path) -> AuthorityPolicyReport {
    let mut violations = Vec::new();
    for root in [repo.join("crates"), repo.join("apps"), repo.join("src")] {
        scan_tree(&root, &mut violations);
    }

    let context_root = repo.join("crates/medusa-context/src");
    for path in source_files(&context_root) {
        if let Ok(text) = fs::read_to_string(&path)
            && (text.contains("std::fs") || text.contains("use fs") || text.contains("fs::"))
        {
            violations.push(format!(
                "medusa-context contains filesystem I/O: {}",
                path.display()
            ));
        }
    }
    violations.sort();
    violations.dedup();
    AuthorityPolicyReport { violations }
}

pub fn assert_compliant(repo: &Path) -> Result<(), AuthorityPolicyReport> {
    let report = inspect_repository(repo);
    report.is_compliant().then_some(()).ok_or(report)
}

fn scan_tree(root: &Path, violations: &mut Vec<String>) {
    for path in source_files(root) {
        if is_compatibility_boundary(&path)
            || is_fixture(&path)
            || path.ends_with("refinement_authority_policy.rs")
        {
            continue;
        }
        let Ok(full_text) = fs::read_to_string(&path) else {
            continue;
        };
        let text = full_text.split("#[cfg(test)]").next().unwrap_or(&full_text);
        if FORBIDDEN_PATHS
            .iter()
            .any(|forbidden| text.contains(forbidden))
            && has_write_operation(text)
        {
            violations.push(format!(
                "legacy improvement authority write outside a compatibility boundary: {}",
                path.display()
            ));
        }
    }
}

fn source_files(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    collect_source_files(root, &mut paths);
    paths
}

fn collect_source_files(root: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_source_files(&path, paths);
        } else if path.extension().is_some_and(|extension| {
            extension == "rs" || extension == "ts" || extension == "tsx" || extension == "js"
        }) {
            paths.push(path);
        }
    }
}

fn is_compatibility_boundary(path: &Path) -> bool {
    path.ends_with("crates/medusa-improvement/src/learning_review.rs")
        || path.ends_with("crates/medusa-improvement/src/scoped_memory.rs")
        || path.ends_with("crates/medusa-improvement/src/refinement_migration.rs")
}

fn is_fixture(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "tests")
}

fn has_write_operation(text: &str) -> bool {
    ["fs::write", "OpenOptions::new", "rename(", "write_all("]
        .iter()
        .any(|marker| text.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_checkout_has_no_competing_authority_writers() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        assert_compliant(repo).unwrap_or_else(|report| {
            panic!(
                "refinement authority policy violations: {:?}",
                report.violations
            )
        });
    }

    #[test]
    fn context_remains_filesystem_free() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let report = inspect_repository(repo);
        assert!(
            report
                .violations
                .iter()
                .all(|violation| !violation.contains("medusa-context"))
        );
    }
}
