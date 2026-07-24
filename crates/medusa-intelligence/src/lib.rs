//! Syntax-aware indexing, reference discovery, transactional patches, and test impact.

mod format;
mod impact;
mod index;
mod language;
mod patch;
mod retrieval;
pub mod snapshot;
mod support;

pub use format::format_changed;
pub use impact::{TestImpact, select_tests};
pub use index::IndexRefresh;
pub use language::{CodeIndex, Language, Reference, Symbol, SymbolKind};
pub use patch::{PatchTransaction, TextEdit, TransactionReceipt};
pub use retrieval::{RetrievalBudget, RetrievalExclusion, RetrievalReport, RetrievalResult};
pub use snapshot::{FileFingerprint, IndexSnapshot, SnapshotDelta};

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
