#![cfg(unix)]

use std::{fs, process::Command};

use medusa_browser_client::{BrowserClient, BrowserRequest, BrowserResponse};
use medusa_core::ErrorCode;

const SIDECAR_SOURCE: &str = r#"
use std::io::{self, BufRead, Write};

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Some(after_id) = line.split("\"request_id\":").nth(1) else { continue };
        let request_id = after_id
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        if request_id.is_empty() { continue; }
        writeln!(stdout, "{{\"request_id\":{request_id},\"kind\":\"ok\"}}")
            .expect("write response");
        stdout.flush().expect("flush response");
    }
}
"#;

fn compile_sidecar(directory: &std::path::Path) -> std::path::PathBuf {
    let source = directory.join("fake_browserd.rs");
    let executable = directory.join("fake-browserd");
    fs::write(&source, SIDECAR_SOURCE).expect("write sidecar source");
    let status = Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .status()
        .expect("launch rustc for sidecar fixture");
    assert!(status.success(), "compile sidecar fixture");
    executable
}

#[test]
fn browser_client_spawns_stdio_sidecar_round_trips_and_terminates_it() {
    let directory = tempfile::tempdir().expect("tempdir");
    let sidecar = compile_sidecar(directory.path());

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
    let error = match BrowserClient::spawn("medusa-browser-sidecar-that-does-not-exist") {
        Ok(_) => panic!("missing sidecar unexpectedly launched"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::DependencyUnavailable);
    assert!(error.retryable);
    assert!(error.message.contains("could not launch"));
}
