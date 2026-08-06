//! Syntax-aware indexing, reference discovery, transactional patches, and test impact.

mod ast;
mod call_graph;
mod capabilities;
mod format;
mod graph;
mod guarded_rename;
mod impact;
mod index;
mod language;
mod lsp;
mod lsp_actions;
mod lsp_navigation;
mod lsp_semantics;
mod module_graph;
mod patch;
mod rename;
mod resolution;
mod retrieval;
mod review;
pub mod rust_ast;
mod rust_structured_edit;
pub mod rust_symbols_v2;
pub mod snapshot;
mod structured_edit;
mod structured_transaction;
mod support;
mod symbol_impact;
mod symbol_table;
mod typescript_workspace;

pub use ast::{ParseDiagnostic, RustAstDocument, RustAstNode, SourcePosition, SourceRange};
pub use call_graph::{RustCallEdge, RustCallGraph};
pub use capabilities::{
    LanguageCapabilityClaim, LanguageCapabilityLevel, LanguageCapabilityProfile,
    LanguageCapabilityStatus, language_capability_profiles,
};
pub use format::format_changed;
pub use graph::{CallEdge, DependencyEdge, SemanticGraph, SymbolId};
pub use guarded_rename::{
    GuardedRenamePlan, RevisionBoundRenamePlan, bind_guarded_rename_snapshot,
    lsp_position_to_byte_offset, prepare_guarded_rename_transaction, validate_guarded_rename,
    validate_guarded_rename_snapshot,
};
pub use impact::{TestImpact, select_tests, select_tests_with_index};
pub use index::IndexRefresh;
pub use language::{CodeIndex, Language, Reference, Symbol, SymbolKind};
pub use lsp::{LspClient, LspError, LspServerConfig, LspServerManager, LspServerState};
pub use lsp_actions::{
    LspAnnotatedTextEdit, LspCapabilityResult, LspChangeAnnotation, LspCodeAction, LspCommand,
    LspCommandPolicy, LspRenameComparison, LspResourceOperation, LspWorkspaceEdit,
    LspWorkspaceOperation, code_actions, compare_rename_paths, execute_command_guarded,
    normalize_workspace_edit, prepare_rename, rename as lsp_rename, resolve_code_action,
};
pub use lsp_navigation::{
    LspLocation, LspNavigationKind, LspNavigationResult, LspPosition, LspRange,
    compare_with_static, document_symbols, find_references, go_to_declaration, go_to_definition,
    workspace_symbols,
};
pub use lsp_semantics::{
    DiagnosticSeverity, DiagnosticSnapshot, LspDiagnostic, LspHover,
    LspPosition as LspSemanticPosition, LspRange as LspSemanticRange, LspRelatedDiagnostic,
    SemanticToken, SemanticTokenState,
};
pub use module_graph::{RustDependencyEdge, RustDependencyKind, RustModuleGraph};
pub use patch::{
    PatchTransaction, TextEdit, TransactionReceipt, finalize_patch_transactions,
    recover_patch_transactions,
};
pub use rename::{
    RustRenameConflict, RustRenameConflictKind, RustRenameFile, RustRenamePlan, plan_rust_rename,
};
pub use resolution::{ResolutionStatus, RustResolutionIndex, RustResolvedReference};
pub use retrieval::{RetrievalBudget, RetrievalExclusion, RetrievalReport, RetrievalResult};
pub use review::ReviewImpact;
pub use rust_structured_edit::{RustStructuredEditPlanner, rust_snapshot_ast_nodes};
pub use snapshot::{FileFingerprint, IndexSnapshot, SnapshotDelta};
pub use structured_edit::{
    EditMetadata, EditPreconditions, EditPreview, EditRange, FileSnapshot, StructuredEditAudit,
    StructuredEditError, StructuredEditPlan, StructuredFileOperation, StructuredTextEdit,
};
pub use structured_transaction::{
    StructuredTransactionError, StructuredTransactionReceipt, StructuredTransactionState,
    TransactionFailurePoint, apply_structured_transaction, recover_structured_transactions,
};
pub use symbol_impact::{RustImpactFile, RustSymbolImpact, analyze_rust_symbol_impact};
pub use symbol_table::{
    RustScope, RustScopeKind, RustSymbol, RustSymbolId, RustSymbolKind, RustSymbolTable,
};
pub use typescript_workspace::{
    TypeScriptWorkspace, TypeScriptWorkspaceError, discover_typescript_workspace,
};

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
    fn guarded_rename_refuses_ambiguous_and_incomplete_indexes() {
        let ambiguous = tempfile::tempdir().expect("ambiguous");
        fs::write(ambiguous.path().join("first.rs"), "fn duplicate() {}\n").expect("first");
        fs::write(ambiguous.path().join("second.rs"), "fn duplicate() {}\n").expect("second");
        let index = CodeIndex::build(ambiguous.path()).expect("ambiguous index");
        let mut transaction = PatchTransaction::new();
        let error = transaction
            .rename_symbol(&index, "duplicate", "renamed")
            .expect_err("ambiguous rename must fail");
        assert!(error.to_string().contains("ambiguous symbol rename"));

        let incomplete = tempfile::tempdir().expect("incomplete");
        fs::write(
            incomplete.path().join("broken.rs"),
            "fn target( { target();\n",
        )
        .expect("broken");
        let index = CodeIndex::build(incomplete.path()).expect("incomplete index");
        assert!(!index.parse_errors.is_empty());
        let mut transaction = PatchTransaction::new();
        let error = transaction
            .rename_symbol(&index, "target", "renamed")
            .expect_err("parse errors must fail closed");
        assert!(error.to_string().contains("parse errors"));
    }

    #[test]
    fn guarded_rename_refuses_python_lexical_matches() {
        let directory = tempfile::tempdir().expect("python");
        fs::write(
            directory.path().join("module.py"),
            "def old_name():\n    return 1\n",
        )
        .expect("python source");
        let index = CodeIndex::build(directory.path()).expect("index");
        let mut transaction = PatchTransaction::new();
        let error = transaction
            .rename_symbol(&index, "old_name", "answer")
            .expect_err("Python lexical rename must fail closed");
        assert!(error.to_string().contains("Rust only"));
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
