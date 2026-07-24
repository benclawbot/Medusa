//! Syntax-aware indexing, reference discovery, transactional patches, and test impact.

mod discovery;
mod format;
mod impact;
mod index;
mod language;
mod patch;
mod support;

pub use discovery::{FileFingerprint, RepositorySnapshot, SnapshotChanges};
pub use format::format_changed;
pub use impact::{TestImpact, select_tests};
pub use language::{CodeIndex, Language, Reference, Symbol, SymbolKind};
pub use patch::{PatchTransaction, TextEdit, TransactionReceipt};

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;
    use crate::support::hash;

    #[test]
    fn indexes_definitions_and_references() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::create_dir(directory.path().join("src")).expect("src");
        fs::write(
            directory.path().join("src/lib.rs"),
            "pub fn old_name() -> u8 { 42 }\npub fn caller() -> u8 { old_name() }\n",
        )
        .expect("source");
        let index = CodeIndex::build(directory.path()).expect("index");
        assert_eq!(index.definitions("old_name").len(), 1);
        assert_eq!(index.references("old_name").len(), 2);
        assert!(index.parse_errors.is_empty());
    }

    #[test]
    fn repository_snapshot_ignores_generated_vendor_and_binary_content() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(directory.path().join("src")).expect("src");
        fs::create_dir_all(directory.path().join("node_modules/pkg")).expect("node_modules");
        fs::create_dir_all(directory.path().join("vendor/lib")).expect("vendor");
        fs::create_dir_all(directory.path().join("dist")).expect("dist");
        fs::write(directory.path().join("src/lib.rs"), "pub fn kept() {}\n").expect("source");
        fs::write(directory.path().join("README.md"), "kept\n").expect("readme");
        fs::write(
            directory.path().join("node_modules/pkg/index.js"),
            "ignored\n",
        )
        .expect("node module");
        fs::write(directory.path().join("vendor/lib/code.rs"), "ignored\n").expect("vendor");
        fs::write(directory.path().join("dist/app.js"), "ignored\n").expect("dist");
        fs::write(directory.path().join("src/blob.rs"), b"pub\0binary").expect("binary");

        let snapshot = RepositorySnapshot::scan(directory.path()).expect("snapshot");
        assert_eq!(
            snapshot.files.keys().cloned().collect::<Vec<_>>(),
            vec![PathBuf::from("README.md"), PathBuf::from("src/lib.rs")]
        );
    }

    #[test]
    fn repository_snapshot_reports_stable_incremental_changes() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::create_dir(directory.path().join("src")).expect("src");
        fs::write(directory.path().join("src/a.rs"), "fn a() {}\n").expect("a");
        fs::write(directory.path().join("src/b.rs"), "fn b() {}\n").expect("b");
        let previous = RepositorySnapshot::scan(directory.path()).expect("previous");

        fs::write(directory.path().join("src/a.rs"), "fn a() -> u8 { 1 }\n").expect("modify");
        fs::remove_file(directory.path().join("src/b.rs")).expect("remove");
        fs::write(directory.path().join("src/c.rs"), "fn c() {}\n").expect("add");
        let current = RepositorySnapshot::scan(directory.path()).expect("current");

        assert_eq!(
            current.changes_since(&previous),
            SnapshotChanges {
                added: vec![PathBuf::from("src/c.rs")],
                modified: vec![PathBuf::from("src/a.rs")],
                removed: vec![PathBuf::from("src/b.rs")],
            }
        );
    }

    #[test]
    fn multi_file_refactor_preserves_unrelated_files() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::create_dir(directory.path().join("src")).expect("src");
        fs::create_dir(directory.path().join("tests")).expect("tests");
        fs::write(
            directory.path().join("src/lib.rs"),
            "pub fn old_name() -> u8 { 42 }\n",
        )
        .expect("lib");
        fs::write(
            directory.path().join("tests/use_it.rs"),
            "use fixture::old_name;\nfn check() { assert_eq!(old_name(), 42); }\n",
        )
        .expect("test");
        fs::write(directory.path().join("README.md"), "unchanged\n").expect("readme");
        let unrelated_before = hash(&fs::read(directory.path().join("README.md")).expect("readme"));

        let index = CodeIndex::build(directory.path()).expect("index");
        let mut transaction = PatchTransaction::new();
        assert_eq!(
            transaction
                .rename_symbol(&index, "old_name", "answer")
                .expect("rename"),
            3
        );
        let receipt = transaction.commit(directory.path()).expect("commit");

        assert_eq!(
            receipt.changed_paths,
            vec![
                PathBuf::from("src/lib.rs"),
                PathBuf::from("tests/use_it.rs")
            ]
        );
        assert!(
            fs::read_to_string(directory.path().join("src/lib.rs"))
                .expect("lib")
                .contains("answer")
        );
        assert!(
            fs::read_to_string(directory.path().join("tests/use_it.rs"))
                .expect("test")
                .contains("answer")
        );
        assert_eq!(
            hash(&fs::read(directory.path().join("README.md")).expect("readme")),
            unrelated_before
        );
        let impact = select_tests(&receipt.changed_paths);
        assert_eq!(
            impact.commands,
            vec!["cargo test --workspace --all-features"]
        );
    }

    #[test]
    fn stale_and_overlapping_edits_fail_before_mutation() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("file.rs"), "abcdef").expect("file");
        let mut transaction = PatchTransaction::new();
        transaction
            .add_edit(TextEdit {
                path: "file.rs".into(),
                start_byte: 0,
                end_byte: 3,
                expected: "wrong".into(),
                replacement: "x".into(),
            })
            .expect("edit");
        assert!(transaction.commit(directory.path()).is_err());
        assert_eq!(
            fs::read_to_string(directory.path().join("file.rs")).expect("file"),
            "abcdef"
        );
    }

    #[cfg(unix)]
    #[test]
    fn patch_transaction_preserves_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("script.rs");
        fs::write(&path, "abcdef").expect("file");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o744)).expect("permissions");
        let mut transaction = PatchTransaction::new();
        transaction
            .add_edit(TextEdit {
                path: "script.rs".into(),
                start_byte: 0,
                end_byte: 3,
                expected: "abc".into(),
                replacement: "xyz".into(),
            })
            .expect("edit");
        transaction.commit(directory.path()).expect("commit");

        assert_eq!(
            fs::metadata(path).expect("metadata").permissions().mode() & 0o777,
            0o744
        );
    }
}
