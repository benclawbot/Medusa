use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use medusa_runtime::component_runtime::{
    ComponentRuntime, ComponentSpec, DependencyRequirement, DependencyResolver, LifecycleState,
    ReplacementError, ReplacementOptions,
};

#[test]
fn healthy_replacement_validates_before_withdrawing_the_old_generation() {
    let mut runtime = ComponentRuntime::new();
    let old = runtime
        .instantiate(
            ComponentSpec::new("provider").with_provided_service("database", "1.0.0"),
            1,
        )
        .expect("old provider");
    let consumer = runtime
        .instantiate(
            ComponentSpec::new("consumer")
                .with_requirement(DependencyRequirement::required("database")),
            1,
        )
        .expect("consumer");
    runtime.activate(&old).expect("activate old");
    runtime.activate(&consumer).expect("activate consumer");
    let committed = DependencyResolver::resolve(
        runtime.spec(&consumer).expect("consumer spec"),
        &[runtime.provider_candidate(&old).expect("old candidate")],
    )
    .expect("consumer dependency view");
    runtime
        .set_committed_dependency_view(&consumer, committed)
        .expect("commit consumer view");

    let replacement = runtime
        .replace_component(
            &old,
            ComponentSpec::new("provider").with_provided_service("database", "2.0.0"),
            |_, _| Ok(()),
            |_| Ok(()),
            ReplacementOptions::default(),
        )
        .expect("healthy replacement");

    assert_ne!(replacement.candidate, old);
    assert!(replacement.candidate.generation().get() > old.generation().get());
    assert!(replacement.old_withdrawn);
    assert_eq!(replacement.migrated_consumers, vec![consumer]);
    assert_eq!(
        runtime.lifecycle_state(&old),
        Some(LifecycleState::Inactive)
    );
    assert_eq!(
        runtime.lifecycle_state(&replacement.candidate),
        Some(LifecycleState::Active)
    );
    assert!(runtime.is_provider_retiring(&old));
}

#[test]
fn failed_candidate_health_check_rolls_back_candidate_and_keeps_old_provider() {
    let mut runtime = ComponentRuntime::new();
    let old = runtime
        .instantiate(
            ComponentSpec::new("provider").with_provided_service("database", "1.0.0"),
            1,
        )
        .expect("old provider");
    runtime.activate(&old).expect("activate old");

    let error = runtime
        .replace_component(
            &old,
            ComponentSpec::new("provider").with_provided_service("database", "2.0.0"),
            |_, journal| {
                journal.record_successful_effect("candidate setup", || Ok(()));
                Ok(())
            },
            |_| Err("readiness probe failed".to_owned()),
            ReplacementOptions::default(),
        )
        .expect_err("candidate must fail health validation");

    assert!(matches!(error, ReplacementError::CandidateRejected { .. }));
    assert_eq!(runtime.lifecycle_state(&old), Some(LifecycleState::Active));
    assert_eq!(runtime.active_generations("provider").len(), 1);
}

#[test]
fn cancellation_timeout_and_exclusive_resource_conflicts_fail_closed() {
    let mut runtime = ComponentRuntime::new();
    let old = runtime
        .instantiate(
            ComponentSpec::new("provider")
                .with_provided_service("database", "1.0.0")
                .with_exclusive_resource("database-socket"),
            1,
        )
        .expect("old provider");
    runtime.activate(&old).expect("activate old");

    let cancelled = Arc::new(AtomicBool::new(true));
    let error = runtime
        .replace_component(
            &old,
            ComponentSpec::new("provider").with_provided_service("database", "2.0.0"),
            |_, _| Ok(()),
            |_| Ok(()),
            ReplacementOptions {
                cancellation: Some(Arc::clone(&cancelled)),
                timeout: None,
            },
        )
        .expect_err("cancelled replacement");
    assert!(matches!(error, ReplacementError::Cancelled));
    assert_eq!(runtime.lifecycle_state(&old), Some(LifecycleState::Active));

    cancelled.store(false, Ordering::SeqCst);
    let timeout = runtime
        .replace_component(
            &old,
            ComponentSpec::new("provider").with_provided_service("database", "2.0.0"),
            |_, _| Ok(()),
            |_| Ok(()),
            ReplacementOptions {
                cancellation: None,
                timeout: Some(Duration::ZERO),
            },
        )
        .expect_err("timed out replacement");
    assert!(matches!(timeout, ReplacementError::TimedOut));
    assert_eq!(runtime.lifecycle_state(&old), Some(LifecycleState::Active));

    let conflict = runtime
        .replace_component(
            &old,
            ComponentSpec::new("provider")
                .with_provided_service("database", "2.0.0")
                .with_exclusive_resource("database-socket"),
            |_, _| Ok(()),
            |_| Ok(()),
            ReplacementOptions::default(),
        )
        .expect_err("exclusive resource conflict");
    assert!(matches!(
        conflict,
        ReplacementError::ExclusiveResourceConflict { .. }
    ));
    assert_eq!(runtime.lifecycle_state(&old), Some(LifecycleState::Active));
}
