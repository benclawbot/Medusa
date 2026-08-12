use std::fs;

use medusa_improvement::{
    refinement_authority::RefinementAuthorityStore,
    refinement_migration::{MigrationDisposition, RefinementMigrator},
};

fn write_fixture(repo: &std::path::Path, name: &str, target: &str) {
    let path = repo.join(target);
    fs::create_dir_all(path.parent().expect("fixture parent")).expect("create parent");
    fs::write(
        path,
        fs::read(format!("tests/fixtures/refinement-migration/{name}")).expect("fixture"),
    )
    .expect("write fixture");
}

#[test]
fn legacy_active_state_is_compatibility_only_and_rerun_is_idempotent() {
    let repo = tempfile::tempdir().expect("repo");
    write_fixture(
        repo.path(),
        "legacy-learning-review.json",
        ".medusa/learning-review/state.json",
    );
    let mut store = RefinementAuthorityStore::open(repo.path()).expect("authority");
    let report = RefinementMigrator::run(repo.path(), &mut store).expect("migration");
    assert_eq!(report.receipts.len(), 1);
    assert_eq!(
        report.receipts[0].disposition,
        MigrationDisposition::CompatibilityOnly
    );
    assert!(store.snapshot().expect("snapshot").active.is_empty());

    let second = RefinementMigrator::run(repo.path(), &mut store).expect("rerun");
    assert_eq!(
        second.receipts[0].disposition,
        MigrationDisposition::AlreadyImported
    );
    assert!(store.snapshot().expect("snapshot").active.is_empty());
}

#[test]
fn engineering_approval_string_does_not_become_canonical_approval() {
    let repo = tempfile::tempdir().expect("repo");
    write_fixture(
        repo.path(),
        "legacy-engineering.json",
        ".medusa/engineering/improvements.json",
    );
    let mut store = RefinementAuthorityStore::open(repo.path()).expect("authority");
    let report = RefinementMigrator::run(repo.path(), &mut store).expect("migration");
    assert_eq!(
        report.receipts[0].disposition,
        MigrationDisposition::CompatibilityOnly
    );
    let snapshot = store.snapshot().expect("snapshot");
    assert!(snapshot.active.is_empty());
    assert!(snapshot.records[0].approval_receipt_id.is_none());
}

#[test]
fn memory_lessons_are_imported_as_repository_candidates() {
    let repo = tempfile::tempdir().expect("repo");
    write_fixture(
        repo.path(),
        "legacy-lesson.json",
        ".medusa/memory/lessons/lesson-1.json",
    );
    let mut store = RefinementAuthorityStore::open(repo.path()).expect("authority");
    let report = RefinementMigrator::run(repo.path(), &mut store).expect("migration");
    assert_eq!(
        report.receipts[0].disposition,
        MigrationDisposition::CompatibilityOnly
    );
    let snapshot = store.snapshot().expect("snapshot");
    assert!(snapshot.active.is_empty());
    assert_eq!(
        snapshot.records[0].scope,
        medusa_context::refinement::RefinementScope::Repository
    );
}

#[test]
fn corrupt_legacy_source_is_quarantined_and_does_not_stop_other_sources() {
    let repo = tempfile::tempdir().expect("repo");
    write_fixture(repo.path(), "corrupt.json", ".medusa/learnings.json");
    let mut store = RefinementAuthorityStore::open(repo.path()).expect("authority");
    let report = RefinementMigrator::run(repo.path(), &mut store).expect("migration");
    assert_eq!(
        report.receipts[0].disposition,
        MigrationDisposition::Quarantined
    );
    assert!(
        repo.path()
            .join(".medusa/refinement-authority/quarantine")
            .read_dir()
            .expect("quarantine")
            .next()
            .is_some()
    );
}
