use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::Command,
    sync::mpsc,
    thread,
    time::Duration,
};

use medusa_config::Config;
use medusa_provider::{
    ImageSource, Message, MessageBlock, ModelProvider, ModelRequest, OpenAiProvider, Role,
};
use serde_json::{Value, json};

const IMAGE_DATA: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB";

fn with_insecure_loopback_for_test(test_name: &str, test: impl FnOnce()) {
    if std::env::var_os("MEDUSA_ALLOW_INSECURE_PROVIDER_HTTP").is_some() {
        test();
        return;
    }

    // The recorder is deliberately local and plaintext. Run the body in a
    // child process with the explicit opt-in instead of mutating this test
    // process's environment while other tests may be running.
    let status = Command::new(std::env::current_exe().expect("multimodal test executable"))
        .env("MEDUSA_ALLOW_INSECURE_PROVIDER_HTTP", "1")
        .args(["--exact", test_name, "--nocapture"])
        .status()
        .expect("spawn opted-in multimodal test");
    assert!(
        status.success(),
        "opted-in multimodal test failed: {status}"
    );
}

fn request(blocks: Vec<MessageBlock>) -> ModelRequest {
    ModelRequest {
        system: "Inspect the supplied screenshot.".to_owned(),
        messages: vec![Message {
            role: Role::User,
            content: blocks,
        }],
        tools: Vec::new(),
        max_tokens: 64,
        temperature_milli: 0,
    }
}

fn image_block() -> MessageBlock {
    MessageBlock::Image {
        source: ImageSource::Base64 {
            media_type: "image/png".to_owned(),
            data: IMAGE_DATA.to_owned(),
        },
        alt_text: Some("deterministic one-pixel fixture".to_owned()),
    }
}

fn provider_config(provider: &str, base_url: String) -> Config {
    let mut config = Config::default();
    config.model.provider = provider.to_owned();
    config.model.name = "multimodal-contract-model".to_owned();
    config.model.auth = "none".to_owned();
    config.model.base_url = Some(base_url);
    config.model.tool_calling = true;
    config.model.streaming = false;
    config
}

fn read_http_body(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer).expect("read request");
        assert!(read > 0, "connection closed before request headers");
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().expect("content length"))
            })
        })
        .expect("content-length header");
    while bytes.len() < header_end + content_length {
        let read = stream.read(&mut buffer).expect("read request body");
        assert!(read > 0, "connection closed before request body");
        bytes.extend_from_slice(&buffer[..read]);
    }
    String::from_utf8(bytes[header_end..header_end + content_length].to_vec())
        .expect("utf-8 JSON request")
}

fn spawn_recording_server() -> (String, mpsc::Receiver<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind recording server");
    let address = listener.local_addr().expect("recording address");
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept provider request");
        let body = read_http_body(&mut stream);
        let payload = serde_json::from_str(&body).expect("provider JSON request");
        sender.send(payload).expect("record provider request");
        let response = json!({
            "id": "response-1",
            "choices": [{
                "message": {"content": "grounded response", "tool_calls": []},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 4, "completion_tokens": 2}
        })
        .to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(),
            response
        )
        .expect("write provider response");
    });
    (format!("http://{address}/v1"), receiver)
}

#[test]
fn mixed_text_and_image_reach_the_recording_provider() {
    with_insecure_loopback_for_test(
        stringify!(mixed_text_and_image_reach_the_recording_provider),
        || {
            let (base_url, recorded) = spawn_recording_server();
            let provider = OpenAiProvider::from_config_with_api_key(
                &provider_config("openai", base_url),
                None,
            )
            .expect("openai provider");

            provider
                .complete(&request(vec![
                    MessageBlock::Text {
                        text: "What is shown?".to_owned(),
                    },
                    image_block(),
                ]))
                .expect("recording provider response");

            let payload = recorded
                .recv_timeout(Duration::from_secs(5))
                .expect("recorded request");
            let content = payload["messages"][1]["content"]
                .as_array()
                .expect("multimodal content array");
            assert_eq!(
                content[0],
                json!({"type": "text", "text": "What is shown?"})
            );
            assert_eq!(content[1]["type"], "image_url");
            assert_eq!(
                content[1]["image_url"]["url"],
                format!("data:image/png;base64,{IMAGE_DATA}")
            );
        },
    );
}

#[test]
fn image_only_prompt_reaches_the_recording_provider() {
    with_insecure_loopback_for_test(
        stringify!(image_only_prompt_reaches_the_recording_provider),
        || {
            let (base_url, recorded) = spawn_recording_server();
            let provider = OpenAiProvider::from_config_with_api_key(
                &provider_config("openai", base_url),
                None,
            )
            .expect("openai provider");

            provider
                .complete(&request(vec![image_block()]))
                .expect("recording provider response");

            let payload = recorded
                .recv_timeout(Duration::from_secs(5))
                .expect("recorded request");
            let content = payload["messages"][1]["content"]
                .as_array()
                .expect("image-only content array");
            assert_eq!(content.len(), 1);
            assert_eq!(content[0]["type"], "image_url");
        },
    );
}

#[test]
fn unsupported_route_fails_before_any_http_request() {
    with_insecure_loopback_for_test(
        stringify!(unsupported_route_fails_before_any_http_request),
        || {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind rejection observer");
            listener
                .set_nonblocking(true)
                .expect("set rejection observer nonblocking");
            let address = listener.local_addr().expect("observer address");
            let provider = OpenAiProvider::from_config_with_api_key(
                &provider_config("minimax", format!("http://{address}/v1")),
                None,
            )
            .expect("text-only compatible provider");

            let error = provider
                .complete(&request(vec![image_block()]))
                .expect_err("unsupported image route must fail");

            assert_eq!(error.context["content_type"], "image");
            assert!(!error.to_string().contains(IMAGE_DATA));
            thread::sleep(Duration::from_millis(100));
            assert!(
                listener.accept().is_err(),
                "unsupported image request unexpectedly reached the network"
            );
        },
    );
}
