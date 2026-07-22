use std::{fs, path::Path};

use medusa_runtime::skill_dependency_locks::{
    verify_dependency_lock, verify_dependency_lock_if_present,
    verify_restorable_dependency_lock, write_dependency_lock, LOCK_FILE,
};

fn write_skill(root: &Path, name: &str, body: &str, requires: &[&str]) {
    let directory = root.join(name);
    fs::create_dir_all(&directory).expect("skill directory");
    fs::write(directory.join("SKILL.md"), body).expect("skill body");
    if !requires.is_empty() {
        fs::write(
            directory.join("dependencies.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 1,
                "requires": requires,
            }))
            .expect("manifest json"),
        )
        .expect("manifest");
    }
}

#[test]
fn lock_round_trip_is_deterministic_and_optional_before_rollout() {
    let repo = tempfile::tempdir().expect("repo");
    let root = repo.path().join("skills");
    fs::create_dir_all(&root).expect("root");
    write_skill(&root, "base", "# Base\n", &[]);
    write_skill(&root, "release", "# Release\n", &["base"]);

    assert!(verify_dependency_lock_if_present(&root, "release")
        .expect("optional verification")
        .is_none());

    let first = write_dependency_lock(&root, "release").expect("first lock");
    let second = write_dependency_lock(&root, "release").expect("replacement lock");
    assert_eq!(first, second);
    assert_eq!(first.order, vec!["base", "release"]);
    assert_eq!(first.skills.len(), 2);

    let verified = verify_dependency_lock(&root, "release").expect("verify lock");
    assert!(verified.locked);
    assert!(verified.valid);
    assert_eq!(verified.selected, "release");
    assert_eq!(verified.graph_sha256, first.graph_sha256);
    assert!(root.join("release").join(LOCK_FILE).is_file());
}

#[test]
fn dependency_content_drift_and_malformed_receipts_fail_closed() {
    let repo = tempfile::tempdir().expect("repo");
    let root = repo.path().join("skills");
    fs::create_dir_all(&root).expect("root");
    write_skill(&root, "base", "# Base\n", &[]);
    write_skill(&root, "release", "# Release\n", &["base"]);
    write_dependency_lock(&root, "release").expect("lock");

    fs::write(root.join("base/SKILL.md"), "# Changed base\n").expect("drift");
    let stale = verify_dependency_lock(&root, "release").expect_err("drift must fail");
    assert!(stale.contains("stale"));

    fs::write(root.join("release").join(LOCK_FILE), b"not json").expect("malformed lock");
    let malformed = verify_dependency_lock(&root, "release").expect_err("parse must fail");
    assert!(malformed.contains("parse"));
}

#[test]
fn quarantined_skill_lock_is_verified_before_restore() {
    let repo = tempfile::tempdir().expect("repo");
    let active = repo.path().join("active");
    let quarantine = repo.path().join("quarantine");
    fs::create_dir_all(&active).expect("active root");
    fs::create_dir_all(&quarantine).expect("quarantine root");
    write_skill(&active, "base", "# Base\n", &[]);
    write_skill(&active, "release", "# Release\n", &["base"]);
    let locked = write_dependency_lock(&active, "release").expect("lock");

    let candidate = quarantine.join("release");
    fs::rename(active.join("release"), &candidate).expect("quarantine skill");
    let verified = verify_restorable_dependency_lock(&active, &candidate, "release")
        .expect("restore verification")
        .expect("stored lock");
    assert_eq!(verified.graph_sha256, locked.graph_sha256);

    fs::write(active.join("base/SKILL.md"), "# Drifted base\n").expect("dependency drift");
    let error = verify_restorable_dependency_lock(&active, &candidate, "release")
        .expect_err("stale quarantined lock must fail");
    assert!(error.contains("stale"));
}

#[test]
fn missing_required_lock_and_invalid_skill_names_are_rejected() {
    let repo = tempfile::tempdir().expect("repo");
    let root = repo.path().join("skills");
    fs::create_dir_all(&root).expect("root");
    write_skill(&root, "plain", "# Plain\n", &[]);

    let missing = verify_dependency_lock(&root, "plain").expect_err("required lock");
    assert!(missing.contains("missing"));
    assert!(write_dependency_lock(&root, "../escape").is_err());
    assert!(verify_dependency_lock_if_present(&root, "nested/name").is_err());
}
