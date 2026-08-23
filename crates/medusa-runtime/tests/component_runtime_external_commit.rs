use medusa_runtime::component_runtime::{
    ComponentGeneration, ComponentId, ComponentInstanceId, ComponentRuntimeError, EffectJournal,
    ExternalCommitLedger, ExternalCommitRequest, ExternalCommitSemantics, ExternalCommitStatus,
};

fn owner() -> ComponentInstanceId {
    ComponentInstanceId {
        component_id: ComponentId::new("publisher").expect("valid id"),
        generation: ComponentGeneration::new(1),
    }
}

#[test]
fn irreversible_commit_cannot_be_registered_as_a_reversible_effect() {
    let mut journal = EffectJournal::new(owner());
    let request = ExternalCommitRequest::new(
        "deploy-1",
        ExternalCommitSemantics::AtMostOnce,
        "sha256:payload",
        "agent:one",
    );

    let error = journal
        .record_external_commit(&request)
        .expect_err("external commits must not get fake inverses");
    assert!(matches!(
        error,
        ComponentRuntimeError::ExternalCommitNotReversible { operation_id }
            if operation_id == "deploy-1"
    ));
    assert_eq!(journal.pending_effect_count(), 0);
}

#[test]
fn ledger_is_idempotent_and_keeps_commit_point_explicit() {
    let mut ledger = ExternalCommitLedger::new();
    let request = ExternalCommitRequest::new(
        "send-1",
        ExternalCommitSemantics::AtLeastOnce,
        "sha256:payload",
        "agent:one",
    )
    .with_idempotency_key("send-key");

    let prepared = ledger.prepare(request.clone()).expect("prepare");
    assert_eq!(prepared.status, ExternalCommitStatus::Prepared);
    assert_eq!(prepared.attempts, 1);
    assert_eq!(ledger.prepare(request).expect("replay prepare"), prepared);

    let unknown = ledger
        .mark_unknown("send-1", "connection dropped")
        .expect("mark uncertain");
    assert_eq!(unknown.status, ExternalCommitStatus::Unknown);
    assert!(ledger.retryable("send-1").expect("status"));
    let retry = ledger.retry("send-1").expect("retry at-least-once");
    assert_eq!(retry.status, ExternalCommitStatus::Prepared);
    assert_eq!(retry.attempts, 2);
    let committed = ledger.mark_committed("send-1").expect("commit point");
    assert_eq!(committed.status, ExternalCommitStatus::Committed);
    assert!(!ledger.retryable("send-1").expect("status"));
}

#[test]
fn at_most_once_unknown_commit_requires_manual_compensation() {
    let mut ledger = ExternalCommitLedger::new();
    ledger
        .prepare(ExternalCommitRequest::new(
            "charge-1",
            ExternalCommitSemantics::AtMostOnce,
            "sha256:payload",
            "agent:one",
        ))
        .expect("prepare");
    ledger
        .mark_unknown("charge-1", "provider timeout")
        .expect("mark uncertain");
    assert!(!ledger.retryable("charge-1").expect("status"));
    let compensated = ledger
        .mark_compensation_required("charge-1", "reconcile with provider")
        .expect("record compensation");
    assert_eq!(
        compensated.status,
        ExternalCommitStatus::CompensationRequired
    );
}
