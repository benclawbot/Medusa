use medusa_runtime::component_runtime::{
    ComponentId, ComponentRuntime, ComponentSpec, HostCapability, LifecycleState,
};

#[test]
fn replacing_a_component_allocates_a_distinct_generation_and_scoped_context() {
    let mut runtime = ComponentRuntime::new();
    let spec = ComponentSpec::new("search").with_capability(HostCapability::FilesystemRead);

    let first = runtime
        .instantiate(spec.clone(), 7)
        .expect("first generation");
    let replacement = runtime
        .instantiate(spec, 8)
        .expect("replacement generation");

    assert_eq!(
        first.component_id(),
        &ComponentId::new("search").expect("component id")
    );
    assert_ne!(first.generation(), replacement.generation());
    assert_eq!(
        runtime.lifecycle_state(&first),
        Some(LifecycleState::Inactive)
    );

    let context = runtime.context(&first).expect("scoped context");
    assert_eq!(context.identity(), first);
    assert_eq!(context.desired_revision(), 7);
    assert!(context.has_capability(HostCapability::FilesystemRead));
    assert!(!context.has_capability(HostCapability::Network));
}

#[test]
fn contexts_are_isolated_between_component_generations() {
    let mut runtime = ComponentRuntime::new();
    let first = runtime
        .instantiate(
            ComponentSpec::new("worker").with_capability(HostCapability::ProcessSpawn),
            1,
        )
        .expect("first generation");
    let second = runtime
        .instantiate(
            ComponentSpec::new("worker").with_capability(HostCapability::Network),
            2,
        )
        .expect("second generation");

    let first_context = runtime.context(&first).expect("first context");
    let second_context = runtime.context(&second).expect("second context");
    assert_ne!(first_context.identity(), second_context.identity());
    assert!(first_context.has_capability(HostCapability::ProcessSpawn));
    assert!(!first_context.has_capability(HostCapability::Network));
    assert!(second_context.has_capability(HostCapability::Network));
    assert!(!second_context.has_capability(HostCapability::ProcessSpawn));
}
