use medusa_runtime::component_runtime::{
    CapabilityPolicyCompiler, ComponentGeneration, ComponentId, ComponentInstanceId,
    ComponentProvenance, ComponentRuntime, ComponentRuntimeError, ComponentSpec,
    ContainmentControl, ContainmentPlatform, ContainmentPolicyError, HostCapability,
};

fn identity(generation: u64) -> ComponentInstanceId {
    ComponentInstanceId {
        component_id: ComponentId::new("worker").expect("component id"),
        generation: ComponentGeneration::new(generation),
    }
}

#[test]
fn one_resolved_capability_set_feeds_host_authority_and_containment_intent() {
    let spec = ComponentSpec::new("worker")
        .with_capability(HostCapability::FilesystemRead)
        .with_capability(HostCapability::Network);
    let platform = ContainmentPlatform::new(
        "test-linux",
        [
            ContainmentControl::Filesystem,
            ContainmentControl::Network,
            ContainmentControl::Environment,
            ContainmentControl::Process,
            ContainmentControl::ResourceLimits,
        ],
    );
    let policy = CapabilityPolicyCompiler::compile(&spec, &identity(1), 42, &platform)
        .expect("capability policy");
    assert!(policy.host_authority.has(HostCapability::FilesystemRead));
    assert!(policy.host_authority.has(HostCapability::Network));
    assert!(!policy.host_authority.has(HostCapability::ProcessSpawn));
    assert!(policy.os_controls.contains(&ContainmentControl::Filesystem));
    assert!(policy.os_controls.contains(&ContainmentControl::Network));
    assert!(policy.unsupported.is_empty());
    assert_eq!(policy.desired_revision, 42);
    assert!(policy.policy_generation.starts_with("sha256:"));
    assert!(
        policy
            .host_authority
            .require(HostCapability::ProcessSpawn)
            .is_err()
    );
}

#[test]
fn unsupported_os_guarantees_fail_closed_with_typed_reporting() {
    let spec = ComponentSpec::new("worker").with_capability(HostCapability::Network);
    let platform = ContainmentPlatform::new("minimal", [ContainmentControl::Filesystem]);
    let error = CapabilityPolicyCompiler::compile(&spec, &identity(1), 1, &platform)
        .expect_err("network must not silently downgrade");
    assert!(matches!(
        error,
        ContainmentPolicyError::Unsupported {
            control: ContainmentControl::Network,
            ..
        }
    ));
}

#[test]
fn policy_fingerprint_changes_for_generation_and_revision_changes() {
    let spec = ComponentSpec::new("worker").with_capability(HostCapability::FilesystemRead);
    let platform = ContainmentPlatform::current();
    let first =
        CapabilityPolicyCompiler::compile(&spec, &identity(1), 1, &platform).expect("first policy");
    let next_generation = CapabilityPolicyCompiler::compile(&spec, &identity(2), 1, &platform)
        .expect("next generation policy");
    let next_revision = CapabilityPolicyCompiler::compile(&spec, &identity(1), 2, &platform)
        .expect("next revision policy");
    assert_ne!(first.policy_generation, next_generation.policy_generation);
    assert_ne!(first.policy_generation, next_revision.policy_generation);
}

#[test]
fn runtime_instantiation_fails_closed_when_platform_cannot_enforce_capability() {
    let mut runtime = ComponentRuntime::new();
    let platform = ContainmentPlatform::new("minimal", [ContainmentControl::Filesystem]);
    let error = runtime
        .instantiate_with_platform(
            ComponentSpec::new("worker").with_capability(HostCapability::Network),
            ComponentProvenance::new(1, "test"),
            &platform,
        )
        .expect_err("runtime must reject an unenforceable capability");
    assert!(matches!(
        error,
        ComponentRuntimeError::ContainmentPolicy(ContainmentPolicyError::Unsupported {
            control: ContainmentControl::Network,
            ..
        })
    ));
    assert!(runtime.active_instances().is_empty());
}
