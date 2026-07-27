#![cfg(windows)]

//! Coverage for the Windows composable sandbox command boundary.

use medusa_process_containment::{WindowsSandboxRestrictions, run_appcontainer};

#[test]
fn unresolvable_programs_fail_closed() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let error = run_appcontainer(directory.path(), "medusa-absent-program", &[])
        .expect_err("unknown programs must not launch");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn declared_restrictions_describe_the_enforced_boundary() {
    let restrictions = WindowsSandboxRestrictions::default();
    assert_eq!(restrictions.backend, "windows_base_container");
    for expected in [
        "app_container",
        "network_denied",
        "bound_filesystem_repository_rw",
        "bound_filesystem_toolchain_ro",
        "job_kill_on_close",
        "no_host_acl_mutation",
    ] {
        assert!(
            restrictions.restrictions.contains(&expected),
            "missing declared restriction {expected}"
        );
    }
}

#[test]
fn verbatim_repository_paths_are_normalised() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let canonical = directory.path().canonicalize().expect("canonical path");
    assert!(
        canonical.to_string_lossy().starts_with(r"\\?\"),
        "precondition: canonicalize yields a verbatim path"
    );
    let error = run_appcontainer(&canonical, "medusa-absent-program", &[])
        .expect_err("unknown program must still fail closed");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn launch_failures_remain_diagnosable() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let Err(error) = run_appcontainer(directory.path(), "hostname", &[]) else {
        return;
    };
    assert!(!error.to_string().is_empty());
}
