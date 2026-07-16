#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
};

use medusa_browser_client::{BrowserClient, BrowserRequest, BrowserResponse};
use medusa_core::ErrorCode;

#[test]
fn browser_client_spawns_stdio_sidecar_round_trips_and_terminates_it() {
    let directory = tempfile::tempdir().expect("tempdir");
    let sidecar = directory.path().join("fake-browserd.sh");
    fs::write(
        &sidecar,
        "#!/bin/sh\nread request\nprintf '%s\\n' '{\"kind\":\"ok\"}'\nread request || true\n",
    )
    .expect("write sidecar");
    let mut permissions = fs::metadata(&sidecar).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&sidecar, permissions).expect("make executable");

    let mut client = BrowserClient::spawn(sidecar.to_str().expect("sidecar path"))
        .expect("spawn browser client");
    assert_eq!(
        client.request(BrowserRequest::Ping).expect("round trip"),
        BrowserResponse::Ok
    );
    drop(client);
}

#[test]
fn browser_client_reports_missing_sidecar_as_retryable_dependency_error() {
    let error = BrowserClient::spawn("medusa-browser-sidecar-that-does-not-exist")
        .expect_err("missing sidecar");
    assert_eq!(error.code, ErrorCode::DependencyUnavailable);
    assert!(error.retryable);
    assert!(error.message.contains("could not launch"));
}
