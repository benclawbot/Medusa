//! Canonical Markdown memory with validated proposals and a rebuildable SQLite index.

mod engine;
mod index;
mod lifecycle;
mod persistence;
mod proposal;
mod retrieval;
mod schema;
mod session_recall;
mod session_recall_inbox;
mod session_recall_lifecycle;
mod support;

pub use engine::MemoryEngine;
pub use schema::{MemoryDocument, MemoryProposal, RetrievedMemory, Scope, Status, Validation};
pub use session_recall::{
    SessionComparison, SessionEvent, SessionRecallStore, SessionRecord, SessionSearchHit,
    SessionSearchQuery, SessionWindow,
};
pub use session_recall_inbox::open_session_recall;
pub use session_recall_lifecycle::delete_session_recall;

#[cfg(test)]
mod tests {
    use std::{sync::Arc, thread};

    use super::*;

    fn proposal(title: &str, claim: &str) -> MemoryProposal {
        MemoryProposal {
            memory_type: "command".into(),
            title: title.into(),
            claim: claim.into(),
            evidence: vec!["artifact://sessions/ses-test/verification-1".into()],
            confidence_milli: 950,
            validation: Validation::TestVerified,
            scope: Scope::Project,
            project_id: Some("sha256:test-project".into()),
            session_id: Some("ses-test".into()),
            tags: vec!["rust".into(), "testing".into()],
        }
    }

    #[test]
    fn frontmatter_round_trips() {
        let directory = tempfile::tempdir().expect("tempdir");
        let engine = MemoryEngine::new(directory.path()).expect("engine");
        let document = engine
            .commit_proposal(&proposal(
                "Run workspace tests",
                "Use `cargo test --workspace`.",
            ))
            .expect("commit");
        assert_eq!(
            MemoryDocument::from_markdown(&document.to_markdown()).expect("parse"),
            document
        );
    }

    #[test]
    fn validated_command_is_reused_in_later_session() {
        let directory = tempfile::tempdir().expect("tempdir");
        let first_session = MemoryEngine::new(directory.path()).expect("first session");
        let proposal = proposal(
            "Validated workspace test command",
            "The verified command is `cargo test --workspace --all-features` from repository root.",
        );
        let proposal_path = first_session.propose(&proposal).expect("proposal");
        assert!(proposal_path.exists());
        let committed = first_session.commit_proposal(&proposal).expect("commit");
        drop(first_session);

        let later_session = MemoryEngine::new(directory.path()).expect("later session");
        later_session.rebuild_index().expect("rebuild index");
        let results = later_session
            .search("workspace test command", Scope::Project, 5)
            .expect("retrieve");
        assert_eq!(results.first().expect("memory").document.id, committed.id);
        assert!(
            results[0]
                .document
                .body
                .contains("cargo test --workspace --all-features")
        );
        later_session
            .record_reuse(
                &committed.id,
                "artifact://sessions/ses-later/verification-2",
            )
            .expect("reuse");
        let reused = later_session
            .search("cargo test workspace", Scope::Project, 1)
            .expect("search after reuse");
        assert_eq!(reused[0].document.successful_reuse_count, 1);
    }

    #[test]
    fn concurrent_reuse_updates_preserve_both_increments() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = Arc::new(directory.path().to_path_buf());
        let initial = MemoryEngine::new(root.as_path()).expect("engine");
        let committed = initial
            .commit_proposal(&proposal("Concurrent command", "Use the verified command."))
            .expect("commit");
        let first_engine = MemoryEngine::new(root.as_path()).expect("first worker engine");
        let second_engine = MemoryEngine::new(root.as_path()).expect("second worker engine");
        let id = Arc::new(committed.id);

        let first_id = Arc::clone(&id);
        let first_worker = thread::spawn(move || {
            first_engine.record_reuse(
                first_id.as_str(),
                "artifact://sessions/concurrent/verification-a",
            )
        });
        let second_id = Arc::clone(&id);
        let second_worker = thread::spawn(move || {
            second_engine.record_reuse(
                second_id.as_str(),
                "artifact://sessions/concurrent/verification-b",
            )
        });
        first_worker
            .join()
            .expect("first worker thread")
            .expect("first reuse update");
        second_worker
            .join()
            .expect("second worker thread")
            .expect("second reuse update");

        let final_engine = MemoryEngine::new(root.as_path()).expect("final engine");
        let (_, document) = final_engine.read_by_id(id.as_str()).expect("read document");
        assert_eq!(document.successful_reuse_count, 2);
        assert!(
            document
                .sources
                .iter()
                .any(|source| source.ends_with("verification-a"))
        );
        assert!(
            document
                .sources
                .iter()
                .any(|source| source.ends_with("verification-b"))
        );
    }

    #[test]
    fn supersession_removes_old_claim_from_retrieval() {
        let directory = tempfile::tempdir().expect("tempdir");
        let engine = MemoryEngine::new(directory.path()).expect("engine");
        let old = engine
            .commit_proposal(&proposal("Old test command", "Use `cargo test`."))
            .expect("old");
        let new = engine
            .commit_proposal(&proposal(
                "New test command",
                "Use `cargo test --workspace --all-features`.",
            ))
            .expect("new");
        engine.supersede(&old.id, &new.id).expect("supersede");
        let results = engine
            .search("test command", Scope::Project, 10)
            .expect("search");
        assert!(results.iter().all(|result| result.document.id != old.id));
        assert!(results.iter().any(|result| result.document.id == new.id));
    }

    #[test]
    fn compaction_preserves_source_documents() {
        let directory = tempfile::tempdir().expect("tempdir");
        let engine = MemoryEngine::new(directory.path()).expect("engine");
        let first = engine
            .commit_proposal(&proposal("Command one", "Use command one."))
            .expect("first");
        let second = engine
            .commit_proposal(&proposal("Command two", "Use command two."))
            .expect("second");
        let summary = engine
            .compact(&[first.id.clone(), second.id.clone()], "Command summary")
            .expect("compact");
        assert!(summary.body.contains("Command one"));
        assert!(summary.body.contains("Command two"));
        assert!(engine.read_by_id(&first.id).is_ok());
        assert!(engine.read_by_id(&second.id).is_ok());
    }

    #[test]
    fn secret_like_proposal_is_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        let engine = MemoryEngine::new(directory.path()).expect("engine");
        let mut unsafe_proposal = proposal("Credential", "api_key=sk-example-secret");
        unsafe_proposal.validation = Validation::Observed;
        assert!(engine.commit_proposal(&unsafe_proposal).is_err());
    }
}
