use std::sync::{Arc, Mutex};

use medusa_runtime::component_runtime::{
    ComponentRuntime, ComponentSpec, DependencyReconciliationAction, DependencyRequirement,
    DependencyResolver, LifecycleState,
};

fn provider_spec(id: &str, service: &str) -> ComponentSpec {
    ComponentSpec::new(id).with_provided_service(service, "1.0.0")
}

#[test]
fn provider_retirement_tears_down_consumers_before_provider_and_preserves_committed_views() {
    let mut runtime = ComponentRuntime::new();
    let provider = runtime
        .instantiate(provider_spec("provider", "database"), 1)
        .expect("provider");
    let consumer = runtime
        .instantiate(
            ComponentSpec::new("consumer")
                .with_requirement(DependencyRequirement::required("database")),
            1,
        )
        .expect("consumer");
    runtime.activate(&provider).expect("activate provider");
    runtime.activate(&consumer).expect("activate consumer");
    let view = DependencyResolver::resolve(
        runtime.spec(&consumer).expect("consumer spec"),
        &[runtime.provider_candidate(&provider).expect("candidate")],
    )
    .expect("dependency view");
    runtime
        .set_committed_dependency_view(&consumer, view)
        .expect("commit view");

    let order = Arc::new(Mutex::new(Vec::new()));
    for (identity, label) in [(&consumer, "consumer"), (&provider, "provider")] {
        let order = Arc::clone(&order);
        runtime
            .record_effect(identity, label, move || {
                order.lock().expect("order lock").push(label.to_owned());
                Ok(())
            })
            .expect("record effect");
    }

    let report = runtime.retire_provider(&provider).expect("retirement");
    assert!(report.withdrawn);
    assert!(report.blocked.is_empty());
    assert_eq!(
        *order.lock().expect("order lock"),
        vec!["consumer".to_owned(), "provider".to_owned()]
    );
    assert_eq!(
        runtime.lifecycle_state(&consumer),
        Some(LifecycleState::Inactive)
    );
    assert_eq!(
        runtime.lifecycle_state(&provider),
        Some(LifecycleState::Inactive)
    );
    assert!(runtime.is_provider_retiring(&provider));
    assert!(
        runtime
            .committed_dependency_view(&consumer)
            .expect("view")
            .is_empty()
    );
}

#[test]
fn failed_consumer_teardown_blocks_provider_with_cleanup_debt() {
    let mut runtime = ComponentRuntime::new();
    let provider = runtime
        .instantiate(provider_spec("provider", "database"), 1)
        .expect("provider");
    let consumer = runtime
        .instantiate(
            ComponentSpec::new("consumer")
                .with_requirement(DependencyRequirement::required("database")),
            1,
        )
        .expect("consumer");
    runtime.activate(&provider).expect("activate provider");
    runtime.activate(&consumer).expect("activate consumer");
    let committed = DependencyResolver::resolve(
        runtime.spec(&consumer).expect("consumer spec"),
        &[runtime.provider_candidate(&provider).expect("candidate")],
    )
    .expect("committed view");
    runtime
        .set_committed_dependency_view(&consumer, committed)
        .expect("commit view");
    runtime
        .record_effect(&consumer, "consumer", || {
            Err("consumer still needs provider".to_owned())
        })
        .expect("record effect");
    runtime
        .record_effect(&provider, "provider", || Ok(()))
        .expect("provider effect");

    let report = runtime
        .retire_provider(&provider)
        .expect("retirement report");
    assert!(!report.withdrawn);
    assert_eq!(report.blocked, vec![consumer.clone()]);
    assert_eq!(
        runtime.lifecycle_state(&consumer),
        Some(LifecycleState::BlockedRetirement)
    );
    assert_eq!(
        runtime.lifecycle_state(&provider),
        Some(LifecycleState::BlockedRetirement)
    );
    assert!(runtime.effect_pending(&provider).expect("provider effects"));
}

#[test]
fn unrelated_provider_changes_do_not_restart_an_active_consumer() {
    let mut runtime = ComponentRuntime::new();
    let provider = runtime
        .instantiate(provider_spec("provider", "database"), 1)
        .expect("provider");
    let unrelated = runtime
        .instantiate(provider_spec("metrics", "metrics"), 1)
        .expect("unrelated provider");
    let consumer = runtime
        .instantiate(
            ComponentSpec::new("consumer")
                .with_requirement(DependencyRequirement::required("database")),
            1,
        )
        .expect("consumer");
    runtime.activate(&provider).expect("activate provider");
    runtime.activate(&consumer).expect("activate consumer");
    let committed = DependencyResolver::resolve(
        runtime.spec(&consumer).expect("consumer spec"),
        &[runtime.provider_candidate(&provider).expect("candidate")],
    )
    .expect("committed view");
    runtime
        .set_committed_dependency_view(&consumer, committed)
        .expect("commit view");

    let plan = runtime
        .dependency_reconciliation_plan()
        .expect("reconciliation plan");
    assert!(plan.iter().any(|action| {
        matches!(
            action,
            DependencyReconciliationAction::Noop { component } if component == &consumer
        )
    }));
    assert!(!plan.iter().any(|action| {
        matches!(
            action,
            DependencyReconciliationAction::Restart { component, .. } if component == &unrelated
        )
    }));
}
