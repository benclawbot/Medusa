use std::{fs, path::PathBuf};

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
        ChangedComponent::new(ChangeKind::Modified, PathBuf::from("src/slugify.py"))
            .expect("changed component"),
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
