use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use medusa_browser_client::protocol::{BrowserRequest, BrowserResponse};

#[test]
#[ignore = "requires Playwright + Chromium (browser/verify.mjs prerequisites)"]
fn navigate_then_snapshot_round_trip() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind fixture server");
    let address = listener.local_addr().expect("fixture address");
    let fixture_server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept fixture request");
        let mut request = Vec::new();
        let mut byte = [0_u8; 1];
        while request.len() < 32 * 1024 {
            let count = stream.read(&mut byte).expect("read fixture request");
            if count == 0 {
                break;
            }
            request.push(byte[0]);
            if request.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let body = "<button id='x'>Go</button>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write fixture response");
        stream.flush().expect("flush fixture response");
    });

    let sidecar = env!("CARGO_BIN_EXE_medusa-browserd");
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root");
    let mut child = Command::new(sidecar)
        .arg("--stdio")
        .env("MEDUSA_BROWSER_ALLOW_LOOPBACK", "1")
        .current_dir(repository_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn sidecar");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    let req = BrowserRequest::Navigate {
        url: format!("http://{address}/"),
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

    let req = BrowserRequest::Close;
    line = serde_json::to_string(&req).unwrap();
    line.push('\n');
    stdin.write_all(line.as_bytes()).unwrap();
    stdin.flush().unwrap();
    response.clear();
    reader.read_line(&mut response).unwrap();
    let parsed: BrowserResponse = serde_json::from_str(response.trim()).unwrap();
    assert!(parsed.is_ok(), "close should succeed: {parsed:?}");
    child.wait().expect("sidecar shutdown");
    fixture_server.join().expect("fixture server");
}
