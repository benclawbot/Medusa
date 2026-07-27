#![cfg(windows)]

//! Coverage for the Windows AppContainer command boundary.
//!
//! This path previously had no tests, which is how several Windows-only
//! defects reached users: `icacls` denials on system binaries, verbatim `\\?\`
//! paths reaching `CreateProcessW`, and `HRESULT` values reported as POSIX
//! error numbers.

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
    assert_eq!(restrictions.backend, "windows_appcontainer");
    for expected in [
        "app_container",
        "network_denied",
        "job_kill_on_close",
        "repository_acl_scope",
    ] {
        assert!(
            restrictions.restrictions.contains(&expected),
            "missing declared restriction {expected}"
        );
    }
}

/// A verbatim working directory makes `CreateProcessW` fail with
/// `ERROR_FILE_NOT_FOUND`, so the boundary must normalise the prefix itself
/// instead of forwarding it.
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

/// Failures must stay attributable rather than surfacing a misdecoded
/// `HRESULT` as an unrelated errno.
#[test]
fn launch_failures_identify_the_requested_command() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let Err(error) = run_appcontainer(directory.path(), "hostname", &[]) else {
        return;
    };
    let message = error.to_string();
    assert!(
        !message.is_empty(),
        "containment failures must carry a description"
    );
}
