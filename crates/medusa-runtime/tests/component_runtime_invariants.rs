use medusa_runtime::component_runtime::{
    ComponentGeneration, ComponentId, ComponentInstanceId, ComponentRuntime, ComponentSpec,
    EffectJournal, ExternalCommitLedger, ExternalCommitRequest, ExternalCommitSemantics,
    FaultInjector, FaultPoint, LifecycleState, ReplacementOptions,
};

fn identity(component: &str, generation: u64) -> ComponentInstanceId {
    ComponentInstanceId {
        component_id: ComponentId::new(component).expect("valid component id"),
        generation: ComponentGeneration::new(generation),
    }
}

#[test]
fn deterministic_fault_trace_replays_and_does_not_record_failed_effects() {
    let owner = identity("worker", 1);
    let mut first = FaultInjector::new(77);
    first.fail_once(FaultPoint::ActivationEffect, "injected activation failure");
    let mut journal = EffectJournal::new(owner.clone());
    let result = journal.apply_with_fault(
        &mut first,
        FaultPoint::ActivationEffect,
        "activate",
        || Ok::<_, String>(()),
        || Ok(()),
    );
    assert!(result.is_err());
    assert_eq!(journal.pending_effect_count(), 0);

    let fingerprint = first.replay_fingerprint();
    let mut replay = FaultInjector::new(77);
    replay.fail_once(FaultPoint::ActivationEffect, "injected activation failure");
    assert!(replay.check(FaultPoint::ActivationEffect).is_err());
    assert_eq!(replay.replay_fingerprint(), fingerprint);
    assert_eq!(first.trace(), replay.trace());
}

#[test]
fn lifecycle_and_replacement_failures_leave_runtime_invariants_valid() {
    let mut runtime = ComponentRuntime::new();
    let old = runtime
        .instantiate(
            ComponentSpec::new("provider").with_provided_service("api", "1.0.0"),
            1,
        )
        .expect("old instance");
    runtime.activate(&old).expect("activate old");
    runtime.validate_invariants().expect("active invariants");

    let mut injector = FaultInjector::new(11);
    injector.fail_once(FaultPoint::CandidateHealth, "candidate is unhealthy");
    let error = runtime
        .replace_component(
            &old,
            ComponentSpec::new("provider").with_provided_service("api", "1.0.0"),
            |_context, _journal| Ok(()),
            move |_context| {
                injector
                    .check(FaultPoint::CandidateHealth)
                    .map_err(|error| error.to_string())
            },
            ReplacementOptions::default(),
        )
        .expect_err("faulted candidate must not commit");
    assert!(
        error
            .to_string()
            .contains("candidate generation was rejected")
    );
    assert_eq!(runtime.lifecycle_state(&old), Some(LifecycleState::Active));
    assert!(runtime.active_generations("provider").contains(&old));
    runtime.validate_invariants().expect("rollback invariants");
}

#[test]
fn all_transaction_boundaries_have_deterministic_fault_hooks() {
    let points = [
        FaultPoint::ActivationEffect,
        FaultPoint::CandidateHealth,
        FaultPoint::ConsumerTeardown,
        FaultPoint::ProviderTeardown,
        FaultPoint::DesiredStatePersist,
        FaultPoint::ReconciliationCommit,
        FaultPoint::ExternalPrepare,
        FaultPoint::ExternalCommit,
    ];
    for point in points {
        let mut injector = FaultInjector::new(123);
        injector.fail_once(point, "chaos test");
        assert!(injector.check(point).is_err(), "fault hook {point:?}");
        assert!(
            injector.check(point).is_ok(),
            "fault hook must be one-shot: {point:?}"
        );
        assert_eq!(injector.trace().len(), 2);
    }
}

#[test]
fn external_commit_faults_are_tracked_without_reversible_cleanup() {
    let mut injector = FaultInjector::new(5);
    injector.fail_once(FaultPoint::ExternalCommit, "provider unavailable");
    let mut ledger = ExternalCommitLedger::new();
    let request = ExternalCommitRequest::new(
        "publish-1",
        ExternalCommitSemantics::AtLeastOnce,
        "sha256:payload",
        "test",
    );
    ledger.prepare(request).expect("prepare");
    assert!(injector.check(FaultPoint::ExternalCommit).is_err());
    let record = ledger
        .mark_unknown("publish-1", "fault injected before acknowledgement")
        .expect("unknown commit recorded");
    assert_eq!(
        record.status,
        medusa_runtime::component_runtime::ExternalCommitStatus::Unknown
    );
    assert!(ledger.retryable("publish-1").expect("retry policy"));
}
