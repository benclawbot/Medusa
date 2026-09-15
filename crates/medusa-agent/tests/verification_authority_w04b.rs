use std::fs;

use medusa_agent::authoritative_verification_for_components_at;
use medusa_evidence::{ChangeKind, ChangedComponent, VerificationCheckKind};

#[test]
fn empty_changed_scope_is_rejected_before_verification() {
    let repository = tempfile::tempdir().expect("repository");
    let evidence = tempfile::tempdir().expect("evidence");

    let result = authoritative_verification_for_components_at(
        repository.path(),
        evidence.path(),
        "repository",
        "commit",
        &[],
    );

    assert!(result.is_err());
    assert!(
        result
            .expect_err("empty scope must not be verified")
            .to_string()
            .contains("changed-component scope cannot be empty")
    );
}

#[test]
fn deleted_only_semantic_material_is_rejected_without_evidence() {
    let repository = tempfile::tempdir().expect("repository");
    let evidence = tempfile::tempdir().expect("evidence");
    let component = ChangedComponent::new(ChangeKind::Deleted, "removed.txt").expect("component");

    let result = authoritative_verification_for_components_at(
        repository.path(),
        evidence.path(),
        "repository",
        "commit",
        &[component],
    )
    .expect("verification receipt");

    assert!(!result.receipt.passed);
    let semantic = result
        .receipt
        .checks
        .iter()
        .find(|check| check.kind == VerificationCheckKind::ArtifactSemantic)
        .expect("semantic check");
    assert!(!semantic.passed);
    assert!(semantic.evidence_ids.is_empty());
    assert!(semantic.artifact_ids.is_empty());
    assert!(
        semantic
            .details
            .iter()
            .any(|detail| detail == "semantic_applicability=not_applicable")
    );
    assert!(
        semantic
            .details
            .iter()
            .any(|detail| detail == "verification_blocked=true")
    );
    result.receipt.validate().expect("valid rejected receipt");
}

#[test]
fn readme_only_material_produces_semantic_evidence() {
    let repository = tempfile::tempdir().expect("repository");
    let evidence = tempfile::tempdir().expect("evidence");
    fs::write(repository.path().join("README.md"), "# Medusa\n").expect("README");
    let component = ChangedComponent::new(ChangeKind::Modified, "README.md").expect("component");

    let result = authoritative_verification_for_components_at(
        repository.path(),
        evidence.path(),
        "repository",
        "commit",
        &[component],
    )
    .expect("verification receipt");

    assert!(result.receipt.passed);
    let semantic = result
        .receipt
        .checks
        .iter()
        .find(|check| check.kind == VerificationCheckKind::ArtifactSemantic)
        .expect("semantic check");
    assert!(semantic.passed);
    assert!(!semantic.evidence_ids.is_empty());
    assert!(!semantic.artifact_ids.is_empty());
    result.receipt.validate().expect("valid receipt");
}

#[test]
fn new_javascript_material_produces_semantic_evidence() {
    let repository = tempfile::tempdir().expect("repository");
    let evidence = tempfile::tempdir().expect("evidence");
    fs::write(
        repository.path().join("app.js"),
        "export const answer = 42;\n",
    )
    .expect("JavaScript");
    let component = ChangedComponent::new(ChangeKind::Added, "app.js").expect("component");

    let result = authoritative_verification_for_components_at(
        repository.path(),
        evidence.path(),
        "repository",
        "commit",
        &[component],
    )
    .expect("verification receipt");

    assert!(result.receipt.passed);
    let semantic = result
        .receipt
        .checks
        .iter()
        .find(|check| check.kind == VerificationCheckKind::ArtifactSemantic)
        .expect("semantic check");
    assert!(semantic.passed);
    assert!(!semantic.evidence_ids.is_empty());
    assert!(!semantic.artifact_ids.is_empty());
    result.receipt.validate().expect("valid receipt");
}
