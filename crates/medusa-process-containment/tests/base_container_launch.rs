#![cfg(windows)]

use std::{
    io,
    net::TcpListener,
    path::Path,
    process::{Command, Output},
    time::Duration,
};

use medusa_process_containment::{WindowsSandboxRestrictions, run_appcontainer};

#[test]
fn launches_reads_and_writes_without_changing_repository_acl() {
    let repo = tempfile::tempdir().expect("temporary repository");
    let before = acl_snapshot(repo.path());

    let Some(where_output) = run_or_skip_unsupported(
        repo.path(),
        "where.exe",
        &["hostname.exe".into()],
        "where.exe should launch in the base container",
    ) else {
        return;
    };
    assert!(where_output.status.success());
    assert!(!where_output.stdout.is_empty());

    let Some(output) = run_or_skip_unsupported(
        repo.path(),
        "cmd.exe",
        &[
            "/D".into(),
            "/C".into(),
            "echo sandbox-write>proof.txt & echo sandbox-stdout & echo sandbox-stderr 1>&2".into(),
        ],
        "write probe should launch",
    ) else {
        return;
    };
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("sandbox-stdout"),
        "sandboxed stdout was not captured"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("sandbox-stderr"),
        "sandboxed stderr was not captured"
    );
    assert!(repo.path().join("proof.txt").is_file());
    assert_eq!(before, acl_snapshot(repo.path()));
}

#[test]
fn denies_loopback_network_access() {
    let repo = tempfile::tempdir().expect("temporary repository");
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let port = listener.local_addr().expect("listener address").port();

    let script =
        format!("$client = New-Object Net.Sockets.TcpClient; $client.Connect('127.0.0.1',{port})");
    let Some(output) = run_or_skip_unsupported(
        repo.path(),
        "powershell.exe",
        &[
            "-NoLogo".into(),
            "-NoProfile".into(),
            "-Command".into(),
            script,
        ],
        "network probe should launch",
    ) else {
        return;
    };
    assert!(
        !output.status.success(),
        "AppContainer unexpectedly reached host loopback"
    );

    std::thread::sleep(Duration::from_millis(100));
    assert!(listener.accept().is_err());
}

#[test]
fn reports_effective_boundary() {
    let restrictions = WindowsSandboxRestrictions::default();
    assert_eq!(restrictions.backend, "windows_base_container");
    assert!(restrictions.restrictions.contains(&"network_denied"));
    assert!(
        restrictions
            .restrictions
            .contains(&"bound_filesystem_repository_rw")
    );
    assert!(restrictions.restrictions.contains(&"no_host_acl_mutation"));
}

fn acl_snapshot(path: &std::path::Path) -> Vec<u8> {
    let output = Command::new("icacls.exe")
        .arg(path)
        .output()
        .expect("read repository ACL");
    assert!(output.status.success());
    output.stdout
}

fn run_or_skip_unsupported(
    repo: &Path,
    program: &str,
    args: &[String],
    context: &str,
) -> Option<Output> {
    match run_appcontainer(repo, program, args) {
        Ok(output) => Some(output),
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            eprintln!("SKIP: Windows composable sandbox backend is unavailable: {error}");
            None
        }
        Err(error) => panic!("{context}: {error}"),
    }
}
