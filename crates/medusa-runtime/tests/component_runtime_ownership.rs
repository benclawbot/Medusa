use std::sync::{Arc, Mutex};

use medusa_runtime::component_runtime::{
    ComponentRuntime, ComponentSpec, EffectJournal, HostCapability, ResourceKind,
    ResourceOwnershipRegistry,
};

#[test]
fn effect_journal_rolls_back_successful_effects_in_reverse_order_and_is_idempotent() {
    let mut runtime = ComponentRuntime::new();
    let identity = runtime
        .instantiate(ComponentSpec::new("worker"), 1)
        .expect("component");
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut journal = EffectJournal::new(identity.clone());

    for label in ["first", "second"] {
        let order = Arc::clone(&order);
        journal.record_successful_effect(label, move || {
            order.lock().expect("order lock").push(label.to_owned());
            Ok(())
        });
    }

    let report = journal.rollback();
    assert!(report.is_clean());
    assert_eq!(report.reverted, 2);
    assert_eq!(
        *order.lock().expect("order lock"),
        vec!["second".to_owned(), "first".to_owned()]
    );
    assert_eq!(journal.pending_effect_count(), 0);

    let second_report = journal.rollback();
    assert!(second_report.is_clean());
    assert_eq!(second_report.attempted, 0);
    assert_eq!(
        *order.lock().expect("order lock"),
        vec!["second".to_owned(), "first".to_owned()]
    );
}

#[test]
fn rollback_failure_is_retained_as_cleanup_debt_and_can_be_retried() {
    let mut runtime = ComponentRuntime::new();
    let identity = runtime
        .instantiate(
            ComponentSpec::new("worker").with_capability(HostCapability::FilesystemWrite),
            1,
        )
        .expect("component");
    let attempts = Arc::new(Mutex::new(0_u32));
    let mut journal = EffectJournal::new(identity);
    let attempts_for_inverse = Arc::clone(&attempts);
    journal.record_successful_effect("flaky", move || {
        let mut attempts = attempts_for_inverse.lock().expect("attempts lock");
        *attempts += 1;
        if *attempts == 1 {
            Err("temporary cleanup failure".to_owned())
        } else {
            Ok(())
        }
    });

    let first = journal.rollback();
    assert!(!first.is_clean());
    assert_eq!(first.failures, 1);
    assert_eq!(journal.cleanup_debt().len(), 1);
    assert_eq!(journal.pending_effect_count(), 1);

    let retry = journal.rollback();
    assert!(retry.is_clean());
    assert_eq!(retry.reverted, 1);
    assert!(journal.cleanup_debt().is_empty());
    assert_eq!(*attempts.lock().expect("attempts lock"), 2);
}

#[test]
fn ownership_is_scoped_to_the_exact_component_generation() {
    let mut runtime = ComponentRuntime::new();
    let first = runtime
        .instantiate(ComponentSpec::new("worker"), 1)
        .expect("first generation");
    let second = runtime
        .instantiate(ComponentSpec::new("worker"), 1)
        .expect("second generation");
    let first_context = runtime.context(&first).expect("first context");
    let second_context = runtime.context(&second).expect("second context");
    let mut ownership = ResourceOwnershipRegistry::new();

    ownership
        .register(&first_context, "route-a", ResourceKind::Route)
        .expect("first resource");
    assert_eq!(ownership.resources_for(first_context.identity()).len(), 1);
    assert!(
        ownership
            .resources_for(second_context.identity())
            .is_empty()
    );

    let denied = ownership
        .release(&second_context, "route-a")
        .expect_err("a replacement generation must not clean old resources");
    assert!(denied.to_string().contains("does not own"));
    assert_eq!(ownership.resources_for(first_context.identity()).len(), 1);

    ownership.mark_process_dead(first_context.identity());
    let retained = ownership
        .resources_for(first_context.identity())
        .into_iter()
        .next()
        .expect("ownership survives process death");
    assert!(!retained.owner_process_alive);
}
