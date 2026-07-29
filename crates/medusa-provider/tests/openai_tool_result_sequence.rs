use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

use medusa_config::Config;
use medusa_provider::{
    Message, MessageBlock, ModelProvider, ModelRequest, OpenAiProvider, Role, ToolDefinition,
};
use serde_json::{Value, json};

#[test]
fn tool_results_do_not_emit_an_extra_empty_user_message() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock provider");
    let address = listener.local_addr().expect("mock provider address");

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept request");
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = stream.read(&mut buffer).expect("read request");
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().expect("content length"))
                    })
                    .unwrap_or(0);
                if bytes.len() >= header_end + 4 + content_length {
                    break;
                }
            }
        }

        let header_end = bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("request headers");
        let body: Value = serde_json::from_slice(&bytes[header_end + 4..]).expect("request json");

        let response = json!({
            "id": "response-1",
            "choices": [{
                "message": {"content": "done", "tool_calls": []},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        })
        .to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response.len(),
            response
        )
        .expect("write response");
        body
    });

    let mut config = Config::default();
    config.model.provider = "minimax".to_owned();
    config.model.protocol = "openai".to_owned();
    config.model.base_url = Some(format!("http://{address}"));
    config.model.auth = "api-key".to_owned();

    let provider = OpenAiProvider::from_config_with_api_key(&config, Some("test-key".to_owned()))
        .expect("build provider");
    let request = ModelRequest {
        system: "system".to_owned(),
        messages: vec![
            Message {
                role: Role::Assistant,
                content: vec![MessageBlock::ToolUse {
                    id: "call-1".to_owned(),
                    name: "read".to_owned(),
                    input: json!({"path": "README.md"}),
                }],
            },
            Message {
                role: Role::User,
                content: vec![MessageBlock::ToolResult {
                    tool_use_id: "call-1".to_owned(),
                    content: "contents".to_owned(),
                    is_error: false,
                }],
            },
        ],
        tools: vec![ToolDefinition {
            name: "read".to_owned(),
            description: "Read a file".to_owned(),
            input_schema: json!({"type": "object"}),
        }],
        max_tokens: 128,
        temperature_milli: 0,
    };

    provider.complete(&request).expect("provider response");
    let body = server.join().expect("mock provider thread");
    let messages = body["messages"].as_array().expect("messages array");

    assert_eq!(
        messages.len(),
        3,
        "system, assistant tool call, tool result"
    );
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[2]["role"], "tool");
    assert!(
        messages.iter().all(|message| {
            message["role"] == "tool"
                || message["tool_calls"]
                    .as_array()
                    .is_some_and(|tool_calls| !tool_calls.is_empty())
                || message["content"]
                    .as_str()
                    .is_none_or(|content| !content.is_empty())
        }),
        "serializer must not append an empty user message after a tool result"
    );
}
