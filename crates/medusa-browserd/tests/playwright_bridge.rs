use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use medusa_browser_client::protocol::{BrowserRequest, BrowserResponse};

#[test]
#[ignore = "requires Playwright + Chromium (browser/verify.mjs prerequisites)"]
fn navigate_then_snapshot_round_trip() {
    let sidecar = env!("CARGO_BIN_EXE_medusa-browserd");
    let mut child = Command::new(sidecar)
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn sidecar");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    let req = BrowserRequest::Navigate {
        url: "data:text/html,<button id='x'>Go</button>".into(),
    };
    let mut line = serde_json::to_string(&req).unwrap();
    line.push('\n');
    stdin.write_all(line.as_bytes()).unwrap();
    stdin.flush().unwrap();

    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    let parsed: BrowserResponse = serde_json::from_str(response.trim()).unwrap();
    assert!(parsed.is_ok(), "navigate should succeed: {parsed:?}");

    let req = BrowserRequest::Snapshot;
    line = serde_json::to_string(&req).unwrap();
    line.push('\n');
    stdin.write_all(line.as_bytes()).unwrap();
    stdin.flush().unwrap();

    response.clear();
    reader.read_line(&mut response).unwrap();
    let parsed: BrowserResponse = serde_json::from_str(response.trim()).unwrap();
    match parsed {
        BrowserResponse::Snapshot { text, .. } => assert!(text.contains("Go")),
        other => panic!("expected snapshot, got {other:?}"),
    }

    let _ = child.kill();
    let _ = child.wait();
}
