#![allow(dead_code)]

#[path = "../src/skill_dependencies.rs"]
mod skill_dependencies;

use std::{fs, path::Path};

use skill_dependencies::{inspect_project_skill, resolve_project_skill, validate_project_graph};

fn skill(root: &Path, name: &str, requires: &[&str], body: &str) {
    let directory = root.join(name);
    fs::create_dir_all(&directory).expect("create skill");
    fs::write(directory.join("SKILL.md"), body).expect("write skill");
    if !requires.is_empty() {
        fs::write(
            directory.join("dependencies.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 1,
                "requires": requires,
            }))
            .expect("manifest json"),
        )
        .expect("write manifest");
    }
}

#[test]
fn resolves_diamond_graph_deterministically() {
    let temp = tempfile::tempdir().expect("tempdir");
    skill(temp.path(), "base", &[], "base");
    skill(temp.path(), "alpha", &["base"], "alpha");
    skill(temp.path(), "beta", &["base"], "beta");
    skill(temp.path(), "selected", &["beta", "alpha"], "selected");

    let resolved = resolve_project_skill(temp.path(), "selected", 1024).expect("resolve");
    assert_eq!(resolved.order, ["base", "alpha", "beta", "selected"]);
    assert_eq!(resolved.direct, ["alpha", "beta"]);
    assert_eq!(resolved.content.matches("base").count(), 3);
}

#[test]
fn rejects_missing_duplicate_and_self_dependencies() {
    let missing = tempfile::tempdir().expect("missing");
    skill(missing.path(), "selected", &["absent"], "selected");
    assert!(
        validate_project_graph(missing.path())
            .unwrap_err()
            .contains("missing")
    );

    let duplicate = tempfile::tempdir().expect("duplicate");
    skill(duplicate.path(), "base", &[], "base");
    skill(duplicate.path(), "selected", &["base", "base"], "selected");
    assert!(
        validate_project_graph(duplicate.path())
            .unwrap_err()
            .contains("duplicate")
    );

    let own = tempfile::tempdir().expect("self");
    skill(own.path(), "selected", &["selected"], "selected");
    assert!(
        validate_project_graph(own.path())
            .unwrap_err()
            .contains("itself")
    );
}

#[test]
fn reports_cycles_with_a_readable_chain() {
    let temp = tempfile::tempdir().expect("tempdir");
    skill(temp.path(), "alpha", &["beta"], "alpha");
    skill(temp.path(), "beta", &["gamma"], "beta");
    skill(temp.path(), "gamma", &["alpha"], "gamma");
    let error = validate_project_graph(temp.path()).unwrap_err();
    assert!(error.contains("alpha -> beta -> gamma -> alpha"));
}

#[test]
fn reports_direct_transitive_and_reverse_relations() {
    let temp = tempfile::tempdir().expect("tempdir");
    skill(temp.path(), "base", &[], "base");
    skill(temp.path(), "middle", &["base"], "middle");
    skill(temp.path(), "selected", &["middle"], "selected");
    let inspection = inspect_project_skill(temp.path(), "middle").expect("inspect");
    assert_eq!(inspection.direct, ["base"]);
    assert_eq!(inspection.transitive_order, ["base", "middle"]);
    assert_eq!(inspection.reverse_dependents, ["selected"]);
}

#[test]
fn enforces_total_graph_budget_and_safe_names() {
    let temp = tempfile::tempdir().expect("tempdir");
    skill(temp.path(), "base", &[], "12345");
    skill(temp.path(), "selected", &["base"], "67890");
    assert!(resolve_project_skill(temp.path(), "selected", 9).is_err());
    assert!(resolve_project_skill(temp.path(), "../selected", 1024).is_err());
}

#[cfg(unix)]
#[test]
fn rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("root");
    let outside = tempfile::tempdir().expect("outside");
    skill(outside.path(), "escaped", &[], "escaped");
    symlink(outside.path().join("escaped"), root.path().join("escaped")).expect("symlink");
    assert!(
        validate_project_graph(root.path())
            .unwrap_err()
            .contains("escapes")
    );
}
