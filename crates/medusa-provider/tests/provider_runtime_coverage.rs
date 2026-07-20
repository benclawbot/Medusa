use std::{io::{Read, Write}, net::{TcpListener, TcpStream}, sync::{Mutex, OnceLock}, thread};

use medusa_config::Config;
use medusa_provider::{ConfiguredProvider, ImageSource, Message, MessageBlock, ModelProvider, ModelRequest, OpenAiProvider, ResponseBlock, Role, ToolDefinition};
use serde_json::json;

fn request() -> ModelRequest {
    ModelRequest {
        system: "system".into(),
        messages: vec![
            Message { role: Role::User, content: vec![MessageBlock::Text { text: "hello".into() }] },
            Message { role: Role::Assistant, content: vec![
                MessageBlock::Text { text: "working".into() },
                MessageBlock::ToolUse { id: "call-1".into(), name: "read".into(), input: json!({"path":"a"}) },
                MessageBlock::ToolResult { tool_use_id: "call-0".into(), content: "ok".into(), is_error: false },
                MessageBlock::Image { source: ImageSource::AttachmentRef { attachment_id: "img".into() }, alt_text: Some("ignored".into()) },
            ]},
        ],
        tools: vec![ToolDefinition { name: "read".into(), description: "read file".into(), input_schema: json!({"type":"object"}) }],
        max_tokens: 128,
        temperature_milli: 250,
    }
}

fn serve(responses: Vec<(&'static str, &'static str)>) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut bodies = Vec::new();
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            bodies.push(read_request(&mut stream));
            let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            stream.write_all(response.as_bytes()).unwrap();
        }
        bodies
    });
    (address, handle)
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut data = Vec::new();
    let mut buf = [0_u8; 4096];
    loop {
        let count = stream.read(&mut buf).unwrap();
        if count == 0 { break; }
        data.extend_from_slice(&buf[..count]);
        if let Some(split) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&data[..split + 4]);
            let length = headers.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(str::trim).and_then(|v| v.parse::<usize>().ok())).unwrap_or(0);
            if data.len() >= split + 4 + length { break; }
        }
    }
    String::from_utf8(data).unwrap()
}

fn openai_config(base_url: String, auth: &str) -> Config {
    let mut config = Config::default();
    config.model.provider = "test-route".into();
    config.model.protocol = "openai".into();
    config.model.name = "test-model".into();
    config.model.base_url = Some(base_url);
    config.model.auth = auth.into();
    config
}

#[test]
fn openai_success_maps_messages_tools_usage_and_auth() {
    let body = r#"{"id":"resp-1","choices":[{"finish_reason":"tool_calls","message":{"content":"answer","tool_calls":[{"id":"call-2","function":{"name":"write","arguments":"{\"path\":\"b\"}"}}]}}],"usage":{"prompt_tokens":11,"completion_tokens":7}}"#;
    let (url, server) = serve(vec![("200 OK", body)]);
    let provider = OpenAiProvider::from_config_with_api_key(&openai_config(url, "api-key"), Some("secret".into())).unwrap();
    let response = provider.complete(&request()).unwrap();
    assert_eq!(response.response_id.as_deref(), Some("resp-1"));
    assert_eq!(response.stop_reason.as_deref(), Some("tool_calls"));
    assert_eq!(response.usage.input_tokens, 11);
    assert_eq!(response.usage.output_tokens, 7);
    assert!(matches!(&response.blocks[0], ResponseBlock::Text { text } if text == "answer"));
    assert!(matches!(&response.blocks[1], ResponseBlock::ToolUse { name, .. } if name == "write"));
    let wire = server.join().unwrap().pop().unwrap();
    assert!(wire.contains("Authorization: Bearer secret") || wire.contains("authorization: Bearer secret"));
    assert!(wire.contains("chat/completions"));
    assert!(wire.contains("tool_calls"));
    assert!(wire.contains("tool_call_id"));
}

#[test]
fn openai_handles_no_auth_empty_choices_and_policy_errors() {
    let missing = OpenAiProvider::from_config_with_api_key(&openai_config("http://127.0.0.1:1".into(), "api-key"), None);
    assert!(missing.is_err());

    let (url, server) = serve(vec![("200 OK", r#"{"choices":[]}"#)]);
    let provider = OpenAiProvider::from_config_with_api_key(&openai_config(url, "none"), None).unwrap();
    assert!(provider.complete(&request()).unwrap_err().to_string().contains("no choices"));
    server.join().unwrap();

    let (url, server) = serve(vec![("401 Unauthorized", r#"{"error":"bad key"}"#)]);
    let provider = OpenAiProvider::from_config_with_api_key(&openai_config(url, "none"), None).unwrap();
    let error = provider.complete(&request()).unwrap_err();
    assert!(error.to_string().contains("401"));
    server.join().unwrap();
}

#[test]
fn openai_retries_transient_statuses_and_accepts_session_configuration() {
    let (url, server) = serve(vec![
        ("500 Internal Server Error", r#"{"error":"one"}"#),
        ("429 Too Many Requests", r#"{"error":"two"}"#),
        ("200 OK", r#"{"id":"ok","choices":[{"finish_reason":"stop","message":{"content":"done","tool_calls":[]}}]}"#),
    ]);
    let config = openai_config(format!("{url}/"), "existing");
    let provider = ConfiguredProvider::from_config_with_api_key(&config, Some("session".into())).unwrap();
    assert!(provider.capabilities().supported_image_media_types.is_empty());
    let response = provider.complete(&request()).unwrap();
    assert!(matches!(&response.blocks[0], ResponseBlock::Text { text } if text == "done"));
    assert_eq!(server.join().unwrap().len(), 3);
}

#[test]
fn anthropic_selection_rejects_unknown_provider_and_missing_credentials() {
    let mut config = Config::default();
    config.model.provider = "unknown".into();
    config.model.protocol = "anthropic".into();
    assert!(ConfiguredProvider::from_config_with_api_key(&config, Some("x".into())).is_err());

    config.model.provider = "anthropic-compatible".into();
    let provider = ConfiguredProvider::from_config_with_api_key(&config, Some("x".into())).unwrap();
    assert!(!provider.capabilities().image_input);
    let image_request = ModelRequest { messages: vec![Message { role: Role::User, content: vec![MessageBlock::Image { source: ImageSource::Base64 { media_type: "image/png".into(), data: "AA==".into() }, alt_text: None }] }], ..request() };
    assert!(provider.complete(&image_request).unwrap_err().to_string().contains("image-input"));
}

#[test]
fn configured_provider_constructors_cover_protocol_selection() {
    let (url, server) = serve(vec![("200 OK", r#"{"choices":[{"finish_reason":"stop","message":{"content":"ok","tool_calls":[]}}]}"#)]);
    let config = openai_config(url, "none");
    let provider = ConfiguredProvider::from_config(&config).unwrap();
    assert!(provider.complete(&request()).is_ok());
    server.join().unwrap();
}
