use std::sync::Arc;

use medusa_runtime::component_runtime::{
    ComponentId, ComponentRuntime, ComponentSpec, DesiredStateError, DesiredStateMutation,
    DesiredStateStore, ReconcileAction, Reconciler,
};

#[test]
fn compare_and_swap_rejects_stale_writers_without_advancing_revision() {
    let store = DesiredStateStore::new();
    let first = store
        .compare_and_swap(
            0,
            DesiredStateMutation::upsert(ComponentSpec::new("provider")),
            "agent-a",
        )
        .expect("first commit");
    assert_eq!(first.revision, 1);

    let stale = store
        .compare_and_swap(
            0,
            DesiredStateMutation::upsert(ComponentSpec::new("stale")),
            "agent-b",
        )
        .expect_err("stale writer");
    assert!(matches!(
        stale,
        DesiredStateError::RevisionConflict { expected: 0, .. }
    ));
    assert_eq!(store.snapshot().revision, 1);
    assert!(
        store
            .snapshot()
            .components
            .contains_key(&ComponentId::new("provider").expect("id"))
    );
    assert!(
        !store
            .snapshot()
            .components
            .contains_key(&ComponentId::new("stale").expect("id"))
    );
}

#[test]
fn concurrent_writers_from_one_base_revision_have_one_winner_and_idempotent_retry() {
    let store = Arc::new(DesiredStateStore::new());
    let left = Arc::clone(&store);
    let right = Arc::clone(&store);
    let left_thread = std::thread::spawn(move || {
        left.compare_and_swap_with_idempotency(
            0,
            DesiredStateMutation::upsert(ComponentSpec::new("left")),
            "agent-left",
            Some("request-left".to_owned()),
        )
    });
    let right_thread = std::thread::spawn(move || {
        right.compare_and_swap_with_idempotency(
            0,
            DesiredStateMutation::upsert(ComponentSpec::new("right")),
            "agent-right",
            Some("request-right".to_owned()),
        )
    });
    let left_result = left_thread.join().expect("left thread");
    let right_result = right_thread.join().expect("right thread");
    assert_eq!(left_result.is_ok() as u8 + right_result.is_ok() as u8, 1);

    let winner = match (left_result, right_result) {
        (Ok(winner), Err(_)) | (Err(_), Ok(winner)) => winner,
        _ => unreachable!("exactly one writer should commit"),
    };
    let retry = store
        .compare_and_swap_with_idempotency(
            winner.revision - 1,
            DesiredStateMutation::upsert(ComponentSpec::new("different")),
            "same-agent",
            winner.idempotency_key.clone(),
        )
        .expect("idempotent retry");
    assert_eq!(retry.revision, winner.revision);
    assert_eq!(retry.snapshot, winner.snapshot);
}

#[test]
fn validation_failure_does_not_publish_a_partial_desired_state() {
    let store = DesiredStateStore::new();
    let invalid = ComponentSpec::new("consumer").with_requirement(
        medusa_runtime::component_runtime::DependencyRequirement::required("missing"),
    );
    let error = store
        .compare_and_swap(0, DesiredStateMutation::upsert(invalid), "agent")
        .expect_err("invalid desired graph");
    assert!(matches!(error, DesiredStateError::Validation { .. }));
    assert_eq!(store.snapshot().revision, 0);
    assert!(store.snapshot().components.is_empty());
}

#[test]
fn reconciler_adds_disables_and_noops_from_one_authoritative_snapshot() {
    let store = DesiredStateStore::new();
    let desired = store
        .compare_and_swap(
            0,
            DesiredStateMutation::upsert(ComponentSpec::new("worker")),
            "agent",
        )
        .expect("desired state")
        .snapshot;
    let mut runtime = ComponentRuntime::new();

    let added = Reconciler::reconcile(&mut runtime, &desired).expect("add");
    assert!(
        added
            .actions
            .iter()
            .any(|action| matches!(action, ReconcileAction::Added { .. }))
    );
    let converged = Reconciler::reconcile(&mut runtime, &desired).expect("no-op");
    assert!(!converged.applied);
    assert!(
        converged
            .actions
            .iter()
            .all(|action| matches!(action, ReconcileAction::Noop { .. }))
    );

    let disabled = store
        .compare_and_swap(
            desired.revision,
            DesiredStateMutation::set_enabled(ComponentId::new("worker").expect("id"), false),
            "agent",
        )
        .expect("disable")
        .snapshot;
    let disabled_report = Reconciler::reconcile(&mut runtime, &disabled).expect("disable");
    assert!(
        disabled_report
            .actions
            .iter()
            .any(|action| matches!(action, ReconcileAction::Disabled { .. }))
    );
}
