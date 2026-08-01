use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use super::types::{
    TelegramBotParseMode, TelegramEditMessageOutcome, TelegramEditMessageText, TelegramSendMessage,
    TelegramTransportUpdate, TelegramUpdate, TelegramUpdateCursor,
};
use super::{
    TelegramBotApiClient, TelegramBotApiError, TelegramBotToken, TelegramChatAction,
    TelegramTransportFailure,
};
use crate::telegram::TelegramReaction;

#[test]
fn token_debug_never_exposes_secret() {
    let token = TelegramBotToken::new("123456:abc_DEF-789").expect("valid token");
    let debug = format!("{token:?}");
    assert!(debug.contains("REDACTED"));
    assert!(!debug.contains("abc_DEF-789"));
}

#[test]
fn token_validation_rejects_whitespace_and_malformed_values() {
    for token in [" 123:abc", "123:abc ", "abc:def", "123:", "123:a/b"] {
        assert_eq!(
            TelegramBotToken::new(token),
            Err(TelegramBotApiError::InvalidToken)
        );
    }
}

#[test]
fn cursor_advances_monotonically_and_rejects_regression() {
    let mut cursor = TelegramUpdateCursor::default();
    cursor.acknowledge(7).expect("acknowledge");
    assert_eq!(cursor.next_offset(), Some(8));
    cursor.acknowledge(8).expect("acknowledge");
    assert_eq!(cursor.next_offset(), Some(9));
    assert_eq!(
        cursor.acknowledge(6),
        Err(TelegramBotApiError::InvalidUpdate)
    );
}

#[test]
fn unsupported_updates_remain_acknowledgeable() {
    let update = TelegramUpdate {
        update_id: 11,
        message: None,
        callback_query: None,
    };
    assert_eq!(
        TelegramTransportUpdate::try_from(update).expect("typed update"),
        TelegramTransportUpdate::Unsupported { update_id: 11 }
    );
}

#[test]
fn transient_classification_is_explicit() {
    assert!(TelegramBotApiError::RetryAfter { seconds: 3 }.is_transient());
    assert!(
        TelegramBotApiError::Transport {
            kind: TelegramTransportFailure::Timeout,
            status: None,
        }
        .is_transient()
    );
    assert!(!TelegramBotApiError::InvalidToken.is_transient());
}

#[test]
fn media_updates_deserialize_with_album_and_document_metadata() {
    let update: TelegramUpdate = serde_json::from_value(serde_json::json!({
        "update_id": 12,
        "message": {
            "message_id": 7,
            "date": 1700000000,
            "media_group_id": "album-1",
            "chat": {"id": 42, "type": "private"},
            "from": {"id": 42, "is_bot": false, "first_name": "Ada"},
            "photo": [{
                "file_id": "photo-file",
                "file_unique_id": "photo-unique",
                "width": 320,
                "height": 200,
                "file_size": 1000
            }],
            "document": {
                "file_id": "document-file",
                "file_unique_id": "document-unique",
                "file_name": "notes.txt",
                "mime_type": "text/plain",
                "file_size": 20
            },
            "caption": "inspect"
        }
    }))
    .expect("deserialize media update");
    let message = update.message.expect("message");
    assert_eq!(message.media_group_id.as_deref(), Some("album-1"));
    assert_eq!(message.photo.len(), 1);
    assert_eq!(
        message.document.and_then(|document| document.file_name),
        Some("notes.txt".to_owned())
    );
}

#[test]
fn file_paths_reject_traversal_and_accept_telegram_paths() {
    assert!(super::validate_file_path("photos/file_0.jpg").is_ok());
    assert!(super::validate_file_path("../secret").is_err());
    assert!(super::validate_file_path("photos//file.jpg").is_err());
}

#[test]
fn bot_api_client_covers_success_retry_rejection_and_file_bounds() {
    const TOKEN: &str = "123456789:test_token_abcdefghijklmnopqrstuvwxyz";
    let responses = VecDeque::from([
        MockResponse::json(
            200,
            br#"{"ok":true,"result":{"id":123456789,"is_bot":true,"first_name":"Medusa","username":"medusa_bot"}}"#,
        ),
        MockResponse::json(200, br#"{"ok":true,"result":[]}"#),
        MockResponse::json(
            200,
            br#"{"ok":true,"result":{"file_id":"file-1","file_unique_id":"unique-1","file_size":3,"file_path":"voice/file.ogg"}}"#,
        ),
        MockResponse::json(200, br#"{"ok":true,"result":true}"#),
        MockResponse::json(200, br#"{"ok":true,"result":true}"#),
        MockResponse::json(
            200,
            br#"{"ok":true,"result":{"message_id":9,"date":1700000000,"chat":{"id":42,"type":"private"},"text":"hello"}}"#,
        ),
        MockResponse::json(
            400,
            br#"{"ok":false,"error_code":400,"description":"Bad Request: message is not modified"}"#,
        ),
        MockResponse::json(200, br#"{"ok":true,"result":true}"#),
        MockResponse::json(200, br#"{"ok":true,"result":true}"#),
        MockResponse::bytes(200, &[], b"abc"),
        MockResponse::bytes(200, &[], b"abcdef"),
        MockResponse::bytes(429, &[("Retry-After", "7")], b"retry"),
        MockResponse::bytes(500, &[], b"server"),
        MockResponse::json(
            200,
            br#"{"ok":false,"parameters":{"retry_after":4},"description":"retry"}"#,
        ),
        MockResponse::bytes(500, &[], b"not-json"),
        MockResponse::json(
            400,
            format!(
                r#"{{"ok":false,"error_code":400,"description":"rejected credential {TOKEN}"}}"#
            )
            .as_bytes(),
        ),
    ]);
    let (api_base, requests, server_thread) = spawn_mock_http(responses);
    let token = TelegramBotToken::new(TOKEN).expect("valid token");
    let client = TelegramBotApiClient::with_api_base(token, &api_base).expect("loopback client");
    let debug = format!("{client:?}");
    assert!(debug.contains("REDACTED"));
    assert!(!debug.contains(TOKEN));

    let me = client.get_me().expect("getMe");
    assert_eq!(me.username.as_deref(), Some("medusa_bot"));
    assert!(
        client
            .get_updates(Some(4), 1, 10)
            .expect("getUpdates")
            .is_empty()
    );
    let file = client.get_file("file-1").expect("getFile");
    assert_eq!(file.file_path.as_deref(), Some("voice/file.ogg"));
    assert!(
        client
            .send_chat_action(42, Some(3), TelegramChatAction::Typing)
            .expect("send chat action")
    );
    assert!(
        client
            .set_message_reaction(42, 9, Some(TelegramReaction::Success))
            .expect("set reaction")
    );

    let sent = client
        .send_message(&TelegramSendMessage {
            chat_id: 42,
            text: "hello".to_owned(),
            message_thread_id: None,
            parse_mode: Some(TelegramBotParseMode::MarkdownV2),
            reply_parameters: None,
            reply_markup: None,
            link_preview_options: None,
        })
        .expect("send message");
    assert_eq!(sent.message_id, 9);
    assert_eq!(
        client
            .edit_message_text(&TelegramEditMessageText {
                chat_id: 42,
                message_id: 9,
                text: "hello".to_owned(),
                parse_mode: None,
                reply_markup: None,
                link_preview_options: None,
            })
            .expect("unchanged edit"),
        TelegramEditMessageOutcome::Unchanged
    );
    assert!(client.delete_message(42, 9).expect("delete message"));
    assert!(
        client
            .answer_callback_query("callback-1", Some("done"))
            .expect("answer callback")
    );

    assert_eq!(
        client
            .download_file("voice/file.ogg", 3)
            .expect("download file"),
        b"abc"
    );
    assert!(matches!(
        client.download_file("voice/file.ogg", 3),
        Err(TelegramBotApiError::FileTooLarge { bytes: 6, limit: 3 })
    ));
    assert_eq!(
        client
            .download_file("voice/file.ogg", 16)
            .expect_err("retry-after download"),
        TelegramBotApiError::RetryAfter { seconds: 7 }
    );
    assert!(matches!(
        client.download_file("voice/file.ogg", 16),
        Err(TelegramBotApiError::Transport {
            kind: TelegramTransportFailure::Server,
            status: Some(500),
        })
    ));
    assert_eq!(
        client.get_me().expect_err("envelope retry"),
        TelegramBotApiError::RetryAfter { seconds: 4 }
    );
    assert!(matches!(
        client.get_me(),
        Err(TelegramBotApiError::Transport {
            kind: TelegramTransportFailure::Server,
            status: Some(500),
        })
    ));
    let rejected = client.get_me().expect_err("rejected response");
    let TelegramBotApiError::Rejected { description, .. } = rejected else {
        panic!("unexpected error: {rejected:?}");
    };
    assert!(description.contains("[REDACTED]"));
    assert!(!description.contains(TOKEN));

    assert!(client.get_file("").is_err());
    assert!(client.get_updates(Some(-1), 1, 10).is_err());
    assert!(client.get_updates(None, 0, 10).is_err());
    assert!(client.get_updates(None, 1, 0).is_err());
    assert!(client.download_file("../secret", 10).is_err());
    assert!(client.download_file("voice/file.ogg", 0).is_err());
    assert!(
        TelegramBotApiClient::with_api_base(
            TelegramBotToken::new(TOKEN).expect("valid token"),
            "http://example.com"
        )
        .is_err()
    );

    server_thread.join().expect("mock server thread");
    let requests = requests.lock().expect("captured requests");
    assert_eq!(requests.len(), 16);
    assert!(requests[0].starts_with(&format!("POST /bot{TOKEN}/getMe")));
    assert!(requests[1].contains("\"allowed_updates\":[\"message\",\"callback_query\"]"));
    assert!(requests[4].contains("\"emoji\":\"👍\""));
    assert!(requests[6].starts_with(&format!("POST /bot{TOKEN}/editMessageText")));
    assert!(requests[9].starts_with(&format!("GET /file/bot{TOKEN}/voice/file.ogg")));
}

#[derive(Clone)]
struct MockResponse {
    status: u16,
    headers: Vec<(&'static str, &'static str)>,
    body: Vec<u8>,
}

impl MockResponse {
    fn json(status: u16, body: &[u8]) -> Self {
        Self::bytes(status, &[("Content-Type", "application/json")], body)
    }

    fn bytes(status: u16, headers: &[(&'static str, &'static str)], body: &[u8]) -> Self {
        Self {
            status,
            headers: headers.to_vec(),
            body: body.to_vec(),
        }
    }
}

fn spawn_mock_http(
    responses: VecDeque<MockResponse>,
) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock Telegram server");
    let address = listener.local_addr().expect("mock Telegram address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let handle = thread::spawn(move || {
        for response in responses {
            let (mut stream, _) = listener.accept().expect("accept Telegram request");
            let request = read_complete_request(&mut stream);
            captured.lock().expect("capture request").push(request);
            let reason = match response.status {
                200 => "OK",
                400 => "Bad Request",
                429 => "Too Many Requests",
                500 => "Internal Server Error",
                _ => "Error",
            };
            let mut headers = format!(
                "HTTP/1.1 {} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
                response.status,
                response.body.len()
            );
            for (name, value) in response.headers {
                headers.push_str(&format!("{name}: {value}\r\n"));
            }
            headers.push_str("\r\n");
            stream.write_all(headers.as_bytes()).expect("write headers");
            stream.write_all(&response.body).expect("write body");
            stream.flush().expect("flush response");
        }
    });
    (format!("http://{address}"), requests, handle)
}

fn read_complete_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set request timeout");
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).expect("read request");
        assert!(read > 0, "request closed before headers");
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().expect("content length"))
        })
        .unwrap_or_default();
    while bytes.len() < header_end + content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).expect("read request body");
        assert!(read > 0, "request closed before body");
        bytes.extend_from_slice(&chunk[..read]);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}
