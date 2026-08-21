use std::fs;

use medusa_evidence::{ChangeKind, ChangedComponent, VerificationPlanner};

#[test]
fn repository_defined_python_verifier_does_not_imply_pytest() {
    let repository = tempfile::tempdir().expect("repository");
    fs::create_dir_all(repository.path().join("src")).expect("src");
    fs::write(
        repository.path().join("src/slugify.py"),
        "def slugify(value):\n    return value.lower().replace(' ', '-')\n",
    )
    .expect("python source");
    fs::write(
        repository.path().join("verify.py"),
        "print('repository-defined verification')\n",
    )
    .expect("verification script");

    let components = vec![
        ChangedComponent::new(ChangeKind::Modified, "src/slugify.py").expect("changed component"),
    ];
    let plan = VerificationPlanner::plan(
        repository.path(),
        "repository-fingerprint",
        "commit-sha",
        &components,
        &[],
    )
    .expect("verification plan");

    assert!(plan.checks.iter().any(|check| {
        check.program.as_deref() == Some("python") && check.args == ["verify.py"]
    }));
    assert!(!plan.checks.iter().any(|check| {
        check.program.as_deref() == Some("python") && check.args == ["-m", "pytest"]
    }));
}

#[cfg(not(windows))]
#[test]
fn repository_defined_shell_verifier_uses_posix_sh() {
    let repository = tempfile::tempdir().expect("repository");
    fs::create_dir_all(repository.path().join("src")).expect("src");
    fs::write(
        repository.path().join("src/lib.rs"),
        "pub fn value() -> u32 { 1 }\n",
    )
    .expect("source");
    fs::write(
        repository.path().join("verify.sh"),
        "#!/bin/sh\nset -eu\ntest -f src/lib.rs\n",
    )
    .expect("verification script");

    let components =
        vec![ChangedComponent::new(ChangeKind::Modified, "src/lib.rs").expect("changed component")];
    let plan = VerificationPlanner::plan(
        repository.path(),
        "repository-fingerprint",
        "commit-sha",
        &components,
        &[],
    )
    .expect("verification plan");

    assert!(
        plan.checks
            .iter()
            .any(|check| { check.program.as_deref() == Some("sh") && check.args == ["verify.sh"] })
    );
}
