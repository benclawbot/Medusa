use std::{collections::BTreeSet, fs};

use medusa_context::refinement::{
    EvaluationResult, EvidenceKind, EvidenceRef, ProposerMetadata, RefinementArtifactKind,
    RefinementContent, RefinementProposal, RefinementRisk, RefinementScope,
};
use medusa_improvement::refinement_authority::{
    ApprovalActorClass, RefinementAuthorityError, RefinementAuthorityStore, SelectionContext,
};
use medusa_improvement::scoped_memory::RepositoryIdentity;

fn proposal(id: &str, version: u64, value: &str) -> RefinementProposal {
    RefinementProposal {
        id: id.into(),
        version,
        artifact_kind: RefinementArtifactKind::RepositoryConvention,
        scope: RefinementScope::Repository,
        evidence: vec![EvidenceRef {
            id: format!("evidence-{id}"),
            kind: EvidenceKind::UserCorrection,
            trajectory_id: "trajectory-1".into(),
            start_sequence: 1,
            end_sequence: 1,
        }],
        before: None,
        after: RefinementContent::RepositoryConvention {
            key: "delivery.workflow".into(),
            value: value.into(),
        },
        rationale: "a verified correction should guide matching work".into(),
        expected_outcome: "matching work follows the correction".into(),
        proposer: ProposerMetadata {
            model: "model-a".into(),
            route: "primary".into(),
            version: "1".into(),
        },
        risk: RefinementRisk::Low,
    }
}

fn evaluated() -> EvaluationResult {
    EvaluationResult {
        evaluator: "deterministic-suite".into(),
        validation_passed: true,
        regression_passed: true,
        effectiveness_passed: true,
        notes: "passed".into(),
    }
}

fn selection() -> SelectionContext {
    SelectionContext {
        repository: Some(RepositoryIdentity::new("https://example.test/repo", "/clone").unwrap()),
        user_id: "user-1".into(),
        session_id: Some("session-1".into()),
        task_kind: Some("verification".into()),
        artifact_kind: Some("repository_convention".into()),
        context_tags: BTreeSet::new(),
        explicit_exclusions: BTreeSet::new(),
        objective: "delivery workflow run all checks for the repository".into(),
        now_unix_ms: 1,
    }
}

#[test]
fn approvals_rebind_to_exact_proposal_and_survive_restart() {
    let repo = tempfile::tempdir().expect("repo");
    let mut store = RefinementAuthorityStore::open(repo.path()).expect("open");
    let mut snapshot = store
        .propose(proposal("p1", 1, "run all checks"), 0)
        .expect("propose");
    snapshot = store
        .validate("p1", 1, snapshot.revision)
        .expect("validate");
    snapshot = store
        .record_evaluation("p1", 1, evaluated(), snapshot.revision)
        .expect("evaluate");
    snapshot = store
        .approve(
            "p1",
            1,
            ApprovalActorClass::User,
            "decision-p1",
            10,
            snapshot.revision,
        )
        .expect("approve");
    assert!(snapshot.active.is_empty());
    snapshot = store
        .activate("p1", 1, snapshot.revision)
        .expect("activate");
    assert_eq!(snapshot.active[0].id, "p1");

    let reopened = RefinementAuthorityStore::open(repo.path()).expect("restart");
    assert_eq!(reopened.snapshot().expect("snapshot").active[0].id, "p1");
}

#[test]
fn approval_binding_rejects_changed_proposal_content() {
    let repo = tempfile::tempdir().expect("repo");
    let mut store = RefinementAuthorityStore::open(repo.path()).expect("open");
    let mut snapshot = store.propose(proposal("p1", 1, "v1"), 0).expect("propose");
    snapshot = store
        .validate("p1", 1, snapshot.revision)
        .expect("validate");
    snapshot = store
        .record_evaluation("p1", 1, evaluated(), snapshot.revision)
        .expect("evaluate");
    store
        .approve(
            "p1",
            1,
            ApprovalActorClass::User,
            "decision-p1",
            10,
            snapshot.revision,
        )
        .expect("approve");
    let path = repo
        .path()
        .join(".medusa/refinement-authority/approvals.json");
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("read")).expect("approval JSON");
    document["bindings"][0]["proposal_digest"] = serde_json::Value::String("0".repeat(64));
    fs::write(&path, serde_json::to_vec_pretty(&document).expect("encode")).expect("tamper");
    let error = RefinementAuthorityStore::open(repo.path()).expect_err("changed proposal");
    assert!(matches!(
        error,
        RefinementAuthorityError::CorruptAuthority { .. }
    ));
}

#[test]
fn stale_revision_writes_nothing() {
    let repo = tempfile::tempdir().expect("repo");
    let mut store = RefinementAuthorityStore::open(repo.path()).expect("open");
    let before = store.snapshot().expect("before");
    let error = store
        .propose(proposal("p1", 1, "v1"), 7)
        .expect_err("stale");
    assert!(matches!(error, RefinementAuthorityError::Conflict { .. }));
    assert_eq!(store.snapshot().expect("after"), before);
}

#[test]
fn corrupt_projection_rebuilds_but_corrupt_authority_is_quarantined() {
    let repo = tempfile::tempdir().expect("repo");
    let mut store = RefinementAuthorityStore::open(repo.path()).expect("open");
    store.propose(proposal("p1", 1, "v1"), 0).expect("propose");
    let authority_root = repo.path().join(".medusa/refinement-authority");
    fs::write(authority_root.join("active.json"), b"not-json").expect("corrupt projection");
    let reopened = RefinementAuthorityStore::open(repo.path()).expect("rebuild");
    assert_eq!(reopened.snapshot().expect("snapshot").revision, 1);

    fs::write(authority_root.join("journal.json"), b"not-json").expect("corrupt journal");
    let error = RefinementAuthorityStore::open(repo.path()).expect_err("corrupt authority");
    assert!(matches!(
        error,
        RefinementAuthorityError::CorruptAuthority { .. }
    ));
    assert!(
        authority_root
            .join("quarantine")
            .read_dir()
            .expect("quarantine")
            .next()
            .is_some()
    );
}

#[test]
fn projection_failure_does_not_publish_activation() {
    let repo = tempfile::tempdir().expect("repo");
    let mut store = RefinementAuthorityStore::open(repo.path()).expect("open");
    let mut snapshot = store.propose(proposal("p1", 1, "v1"), 0).expect("propose");
    snapshot = store
        .validate("p1", 1, snapshot.revision)
        .expect("validate");
    snapshot = store
        .record_evaluation("p1", 1, evaluated(), snapshot.revision)
        .expect("evaluate");
    snapshot = store
        .approve(
            "p1",
            1,
            ApprovalActorClass::User,
            "decision-p1",
            10,
            snapshot.revision,
        )
        .expect("approve");
    let active_path = repo.path().join(".medusa/refinement-authority/active.json");
    fs::remove_file(&active_path).expect("remove projection");
    fs::create_dir(&active_path).expect("block projection replacement");
    let error = store
        .activate("p1", 1, snapshot.revision)
        .expect_err("projection failure");
    assert!(matches!(
        error,
        RefinementAuthorityError::ProjectionFailure { .. }
    ));
    assert_eq!(
        store.snapshot().expect("snapshot").revision,
        snapshot.revision
    );
}

#[test]
fn selection_excludes_nonmatching_scope_and_reports_conflicts() {
    let repo = tempfile::tempdir().expect("repo");
    let mut store = RefinementAuthorityStore::open(repo.path()).expect("open");
    let mut snapshot = store
        .propose(proposal("p1", 1, "run all checks"), 0)
        .expect("p1");
    snapshot = store
        .validate("p1", 1, snapshot.revision)
        .expect("validate");
    snapshot = store
        .record_evaluation("p1", 1, evaluated(), snapshot.revision)
        .expect("evaluate");
    snapshot = store
        .approve(
            "p1",
            1,
            ApprovalActorClass::User,
            "decision-p1",
            10,
            snapshot.revision,
        )
        .expect("approve");
    store
        .activate("p1", 1, snapshot.revision)
        .expect("activate");
    let selected = store.select(&selection()).expect("selection");
    assert_eq!(selected.selected[0].proposal.id, "p1");
    assert_eq!(selected.selected[0].approval_receipt_id, "decision-p1");
}

#[test]
fn rollback_restores_the_direct_superseded_predecessor() {
    let repo = tempfile::tempdir().expect("repo");
    let mut store = RefinementAuthorityStore::open(repo.path()).expect("open");
    let first = proposal("p1", 1, "run all checks");
    let mut snapshot = store.propose(first.clone(), 0).expect("p1");
    snapshot = store
        .validate("p1", 1, snapshot.revision)
        .expect("validate p1");
    snapshot = store
        .record_evaluation("p1", 1, evaluated(), snapshot.revision)
        .expect("evaluate p1");
    snapshot = store
        .approve(
            "p1",
            1,
            ApprovalActorClass::User,
            "decision-p1",
            10,
            snapshot.revision,
        )
        .expect("approve p1");
    snapshot = store
        .activate("p1", 1, snapshot.revision)
        .expect("activate p1");

    let mut replacement = proposal("p2", 1, "run every check");
    replacement.before = Some(first.after);
    snapshot = store.propose(replacement, snapshot.revision).expect("p2");
    snapshot = store
        .validate("p2", 1, snapshot.revision)
        .expect("validate p2");
    snapshot = store
        .record_evaluation("p2", 1, evaluated(), snapshot.revision)
        .expect("evaluate p2");
    snapshot = store
        .approve(
            "p2",
            1,
            ApprovalActorClass::User,
            "decision-p2",
            11,
            snapshot.revision,
        )
        .expect("approve p2");
    snapshot = store
        .supersede("p1", 1, "p2", 1, snapshot.revision)
        .expect("supersede");
    assert!(snapshot.active.is_empty());
    snapshot = store
        .activate("p2", 1, snapshot.revision)
        .expect("activate p2");
    assert_eq!(snapshot.active[0].id, "p2");
    snapshot = store
        .rollback(
            "p2",
            1,
            Some("p1"),
            Some(1),
            "restore predecessor",
            snapshot.revision,
        )
        .expect("rollback");
    assert_eq!(snapshot.active[0].id, "p1");
}
