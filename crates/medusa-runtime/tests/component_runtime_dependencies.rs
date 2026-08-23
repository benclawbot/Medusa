use medusa_runtime::component_runtime::{
    ComponentGeneration, ComponentId, ComponentInstanceId, ComponentSpec, DependencyCardinality,
    DependencyRequirement, DependencyResolutionError, DependencyResolver, ProviderCandidate,
    VersionConstraint,
};

fn provider(id: &str, service: &str, version: &str) -> ProviderCandidate {
    ProviderCandidate::new(
        ComponentInstanceId {
            component_id: ComponentId::new(id).expect("component id"),
            generation: ComponentGeneration::new(1),
        },
        ComponentSpec::new(id).with_provided_service(service, version),
    )
}

#[test]
fn required_and_optional_dependencies_resolve_explicitly() {
    let consumer = ComponentSpec::new("consumer")
        .with_requirement(
            DependencyRequirement::required("database")
                .with_version(VersionConstraint::AtLeast("1.0.0".to_owned())),
        )
        .with_requirement(
            DependencyRequirement::optional("metrics").with_cardinality(DependencyCardinality::One),
        );
    let view = DependencyResolver::resolve(&consumer, &[provider("db", "database", "1.2.0")])
        .expect("required dependency resolves");

    assert_eq!(view.providers("database").len(), 1);
    assert!(view.providers("metrics").is_empty());
    assert_eq!(view.providers("database")[0].component_id.as_str(), "db");
}

#[test]
fn missing_required_dependency_is_actionable() {
    let consumer = ComponentSpec::new("consumer")
        .with_requirement(DependencyRequirement::required("database"));
    let error = DependencyResolver::resolve(&consumer, &[]).expect_err("missing dependency");
    assert!(matches!(
        error,
        DependencyResolutionError::MissingRequired { ref service, .. } if service == "database"
    ));
}

#[test]
fn ambiguity_and_incompatible_versions_are_deterministic_errors() {
    let consumer = ComponentSpec::new("consumer")
        .with_requirement(DependencyRequirement::required("database"));
    let ambiguity = DependencyResolver::resolve(
        &consumer,
        &[
            provider("db-z", "database", "1.0.0"),
            provider("db-a", "database", "1.0.0"),
        ],
    )
    .expect_err("ambiguous providers");
    assert!(matches!(
        ambiguity,
        DependencyResolutionError::Ambiguous { ref providers, .. }
            if providers == &vec!["db-a".to_owned(), "db-z".to_owned()]
    ));

    let incompatible = DependencyResolver::resolve(
        &ComponentSpec::new("consumer").with_requirement(
            DependencyRequirement::required("database")
                .with_version(VersionConstraint::Exact("2.0.0".to_owned())),
        ),
        &[provider("db", "database", "1.0.0")],
    )
    .expect_err("incompatible provider");
    assert!(matches!(
        incompatible,
        DependencyResolutionError::IncompatibleVersion { ref service, .. } if service == "database"
    ));
}

#[test]
fn unsatisfied_dependency_cycles_are_detected_before_activation() {
    let a = ComponentSpec::new("a")
        .with_provided_service("a-api", "1.0.0")
        .with_requirement(DependencyRequirement::required("b-api"));
    let b = ComponentSpec::new("b")
        .with_provided_service("b-api", "1.0.0")
        .with_requirement(DependencyRequirement::required("a-api"));

    let error = DependencyResolver::validate_graph(&[a, b]).expect_err("cycle");
    assert!(matches!(error, DependencyResolutionError::Cycle { .. }));
}
