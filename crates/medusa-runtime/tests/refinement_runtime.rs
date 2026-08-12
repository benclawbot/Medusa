use std::{fs, process::Command, sync::mpsc};

use medusa_context::refinement::{
    EvaluationResult, EvidenceKind, EvidenceRef, ProposerMetadata, RefinementArtifactKind,
    RefinementContent, RefinementProposal, RefinementRisk, RefinementScope,
};
use medusa_core::learning_policy::LearningPrivacyPolicy;
use medusa_improvement::refinement_authority::{ApprovalActorClass, RefinementAuthorityStore};
use medusa_runtime::{
    RuntimeEvent,
    learning_retrieval::{self, RuntimeLearningContext},
    prompt::PromptDraft,
};

fn repository() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("repository");
    let init = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(repo.path())
        .status()
        .expect("git init");
    assert!(init.success());
    let commit = Command::new("git")
        .args([
            "-c",
            "user.email=tests@example.invalid",
            "-c",
            "user.name=Medusa Tests",
            "commit",
            "--allow-empty",
            "-m",
            "test root",
            "--quiet",
        ])
        .current_dir(repo.path())
        .status()
        .expect("git commit");
    assert!(commit.success());
    let authority = RefinementAuthorityStore::open(repo.path()).expect("authority");
    authority
        .update_privacy(
            LearningPrivacyPolicy {
                capture_enabled: true,
                user_persistence_enabled: true,
                cross_repository_reuse_enabled: true,
                telemetry_enabled: true,
                automatic_proposals_enabled: true,
            },
            0,
        )
        .expect("privacy");
    repo
}

fn proposal(id: &str, version: u64, value: &str) -> RefinementProposal {
    RefinementProposal {
        id: id.into(),
        version,
        artifact_kind: RefinementArtifactKind::RepositoryConvention,
        scope: RefinementScope::Repository,
        evidence: vec![EvidenceRef {
            id: format!("evidence-{id}-{version}"),
            kind: EvidenceKind::UserCorrection,
            trajectory_id: "runtime-test".into(),
            start_sequence: 1,
            end_sequence: 1,
        }],
        before: None,
        after: RefinementContent::RepositoryConvention {
            key: "delivery.workflow".into(),
            value: value.into(),
        },
        rationale: "explicit correction for runtime selection".into(),
        expected_outcome: "matching later tasks use the correction".into(),
        proposer: ProposerMetadata {
            model: "test".into(),
            route: "runtime-test".into(),
            version: "1".into(),
        },
        risk: RefinementRisk::Low,
    }
}

fn activate(repo: &std::path::Path, id: &str, value: &str) {
    let mut store = RefinementAuthorityStore::open(repo).expect("authority");
    let mut snapshot = store.propose(proposal(id, 1, value), 0).expect("propose");
    snapshot = store.validate(id, 1, snapshot.revision).expect("validate");
    snapshot = store
        .record_evaluation(
            id,
            1,
            EvaluationResult {
                evaluator: "runtime-test".into(),
                validation_passed: true,
                regression_passed: true,
                effectiveness_passed: true,
                notes: "passed".into(),
            },
            snapshot.revision,
        )
        .expect("evaluate");
    snapshot = store
        .approve(
            id,
            1,
            ApprovalActorClass::User,
            &format!("approval-{id}"),
            1,
            snapshot.revision,
        )
        .expect("approve");
    store.activate(id, 1, snapshot.revision).expect("activate");
}

fn select(repo: &std::path::Path, objective: &str) -> RuntimeLearningContext {
    let (events, _received) = mpsc::channel::<RuntimeEvent>();
    learning_retrieval::select(
        repo,
        &PromptDraft {
            text: objective.into(),
            ..PromptDraft::default()
        },
        Some("runtime-session"),
        &events,
    )
}

#[test]
fn approved_correction_reaches_matching_turn_only_with_provenance() {
    let repo = repository();
    activate(repo.path(), "runtime-p1", "run canonical checks");

    let matching = select(repo.path(), "run canonical checks before the release");
    let prompt = matching.prompt_context.expect("matching refinement");
    assert!(prompt.contains("id=runtime-p1"));
    assert!(prompt.contains("version=1"));
    assert!(prompt.contains("approval-runtime-p1"));
    assert!(prompt.contains("journal_head="));

    let nonmatching = select(repo.path(), "write release notes");
    assert!(nonmatching.prompt_context.is_none());

    let audit = fs::read_to_string(repo.path().join(".medusa/learning-selection-audit.jsonl"))
        .expect("selection audit");
    assert!(audit.contains("runtime-p1"));
    assert!(audit.contains("\"approval_receipt_id\":\"approval-runtime-p1\""));
}

#[test]
fn restart_and_projection_recovery_preserve_selection() {
    let repo = repository();
    activate(repo.path(), "runtime-p1", "run canonical checks");
    let projection = repo.path().join(".medusa/refinement-authority/active.json");
    fs::write(&projection, b"corrupt projection").expect("corrupt projection");
    assert!(
        select(repo.path(), "run canonical checks")
            .prompt_context
            .is_some()
    );
    assert!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(projection).expect("projection"))
            .is_ok()
    );
}

#[test]
fn corrupt_authority_fails_closed_without_prompt_context() {
    let repo = repository();
    activate(repo.path(), "runtime-p1", "run canonical checks");
    fs::write(
        repo.path()
            .join(".medusa/refinement-authority/journal.json"),
        b"corrupt journal",
    )
    .expect("corrupt authority");
    assert!(
        select(repo.path(), "run canonical checks")
            .prompt_context
            .is_none()
    );
}

#[test]
fn suspension_removes_a_previously_selected_refinement() {
    let repo = repository();
    activate(repo.path(), "runtime-p1", "run canonical checks");
    let mut store = RefinementAuthorityStore::open(repo.path()).expect("authority");
    let revision = store.snapshot().expect("snapshot").revision;
    store
        .suspend("runtime-p1", 1, "test suspension", revision)
        .expect("suspend");
    assert!(
        select(repo.path(), "run canonical checks")
            .prompt_context
            .is_none()
    );
}
