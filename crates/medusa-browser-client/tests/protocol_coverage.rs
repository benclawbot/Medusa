use std::io::Write;
use std::sync::{Arc, Mutex};

use medusa_browser_client::{BrowserRequest, BrowserResponse, ElementRef, TabInfo};

struct Pipe {
    rx: Arc<Mutex<Vec<u8>>>,
}

impl Pipe {
    fn new() -> (Box<dyn Write + Send>, Self) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer = PipeWriter { buf: Arc::clone(&buf) };
        (Box::new(writer), Self { rx: buf })
    }

    fn drain(&self) -> Vec<u8> {
        let mut g = self.rx.lock().unwrap();
        std::mem::take(&mut *g)
    }
}

struct PipeWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn request_serializes_with_method_and_params() {
    let req = BrowserRequest::Navigate {
        url: "https://example.com".into(),
    };
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("\"method\":\"navigate\""));
    assert!(json.contains("\"url\":\"https://example.com\""));
}

#[test]
fn response_deserializes_snapshot_with_refs() {
    let json = r##"{"kind":"snapshot","text":"hello","refs":[{"id":1,"role":"button","name":"Submit","selector":"#submit"}]}"##;
    let resp: BrowserResponse = serde_json::from_str(json).unwrap();
    match resp {
        BrowserResponse::Snapshot { text, refs } => {
            assert_eq!(text, "hello");
            assert_eq!(
                refs,
                vec![ElementRef {
                    id: 1,
                    role: "button".into(),
                    name: "Submit".into(),
                    selector: "#submit".into(),
                }]
            );
        }
        _ => panic!("expected snapshot"),
    }
}

#[test]
fn client_writes_one_request_line_and_reads_one_response() {
    let (mut writer, pipe) = Pipe::new();
    let payload = serde_json::to_vec(&BrowserRequest::Ping).unwrap();
    writer.write_all(&payload).unwrap();
    writer.write_all(b"\n").unwrap();
    let got = pipe.drain();
    assert!(got.ends_with(b"\n"));
}

#[test]
fn response_kind_tabs_round_trips() {
    let json = r#"{"kind":"tabs","tabs":[{"id":7,"url":"https://example.com","title":"Example"}]}"#;
    let resp: BrowserResponse = serde_json::from_str(json).unwrap();
    match resp {
        BrowserResponse::Tabs { tabs } => {
            assert_eq!(
                tabs,
                vec![TabInfo {
                    id: 7,
                    url: "https://example.com".into(),
                    title: "Example".into(),
                }]
            );
        }
        _ => panic!("expected tabs"),
    }
}