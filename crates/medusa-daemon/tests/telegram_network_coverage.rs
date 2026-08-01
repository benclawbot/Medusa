use std::{
    collections::{BTreeMap, VecDeque},
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration as StdDuration,
};

use medusa_config::Config;
use medusa_daemon::telegram::{
    OpenAiAudioToken, TelegramChatKind, TelegramIdentity, TelegramMiniAppBridge,
    TelegramMiniAppHttpConfig, TelegramMiniAppHttpServer, TelegramMiniAppSecret,
    TelegramVoiceError, TelegramVoiceInput, TelegramVoicePipeline, TelegramWebhookConfig,
    TelegramWebhookServer,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

const BOT_TOKEN: &str = "123456789:abcdefghijklmnopqrstuvwxyz";

#[test]
fn mini_app_http_exercises_authentication_routes_and_queue_bounds() {
    let now = OffsetDateTime::now_utc();
    let identity = TelegramIdentity {
        user_id: 7,
        chat_id: 7,
        topic_id: None,
        chat_kind: TelegramChatKind::Private,
        bot_mentioned: false,
    };
    let bridge = TelegramMiniAppBridge::new(
        TelegramMiniAppSecret::from_bot_token(BOT_TOKEN).expect("valid secret"),
    );
    let ticket = bridge
        .issue_launch_ticket(&identity, "session-network-coverage", now)
        .expect("launch ticket");
    let init_data = signed_init_data(BOT_TOKEN, identity.user_id, now);
    let (server, receiver) = TelegramMiniAppHttpServer::bind(
        TelegramMiniAppHttpConfig {
            bind: "127.0.0.1:0".parse().expect("loopback address"),
            path_prefix: "/custom/telegram/voice".to_owned(),
        },
        bridge,
        Config::default(),
    )
    .expect("bind mini app server");
    let address = server.local_addr().expect("mini app address");
    let cancelled = Arc::new(AtomicBool::new(false));
    let server_cancelled = Arc::clone(&cancelled);
    let server_thread = thread::spawn(move || server.run_until_cancelled(&server_cancelled));

    let root = http_request(address, "GET", "/custom/telegram/voice", &[], b"");
    assert_status(&root, 200);
    assert!(response_body(&root).contains("const apiBase = \"/custom/telegram/voice\";"));

    let options = http_request(address, "OPTIONS", "/anything", &[], b"");
    assert_status(&options, 204);
    assert!(options.contains("Cache-Control: no-store"));
    assert!(options.contains("Referrer-Policy: no-referrer"));

    let missing = http_request(address, "GET", "/missing", &[], b"");
    assert_status(&missing, 404);

    let malformed_auth =
        http_json_request(address, "/custom/telegram/voice/auth", None, b"not-json");
    assert_status(&malformed_auth, 400);

    let invalid_ticket_body = serde_json::to_vec(&json!({
        "ticket": "invalid",
        "initData": init_data,
    }))
    .expect("serialize invalid ticket request");
    let invalid_ticket = http_json_request(
        address,
        "/custom/telegram/voice/auth",
        None,
        &invalid_ticket_body,
    );
    assert_status(&invalid_ticket, 401);

    let invalid_init_body = serde_json::to_vec(&json!({
        "ticket": ticket.token,
        "initData": "auth_date=0&user=%7B%22id%22%3A7%7D&hash=00",
    }))
    .expect("serialize invalid init request");
    let invalid_init = http_json_request(
        address,
        "/custom/telegram/voice/auth",
        None,
        &invalid_init_body,
    );
    assert_status(&invalid_init, 401);

    let auth_body = serde_json::to_vec(&json!({
        "ticket": ticket.token,
        "initData": signed_init_data(BOT_TOKEN, identity.user_id, now),
    }))
    .expect("serialize auth request");
    let auth = http_json_request(address, "/custom/telegram/voice/auth", None, &auth_body);
    assert_status(&auth, 200);
    let auth_json: Value = serde_json::from_str(response_body(&auth)).expect("auth response JSON");
    let auth_token = auth_json["token"]
        .as_str()
        .expect("authenticated token")
        .to_owned();
    assert_eq!(auth_json["sessionId"], "session-network-coverage");

    let launch_token_realtime = http_json_request(
        address,
        "/custom/telegram/voice/realtime",
        Some(&ticket.token),
        b"{}",
    );
    assert_status(&launch_token_realtime, 401);

    let missing_realtime =
        http_json_request(address, "/custom/telegram/voice/realtime", None, b"{}");
    assert_status(&missing_realtime, 401);

    let unavailable_realtime = http_json_request(
        address,
        "/custom/telegram/voice/realtime",
        Some(&auth_token),
        b"{}",
    );
    assert_status(&unavailable_realtime, 503);

    let missing_transcript = http_json_request(
        address,
        "/custom/telegram/voice/transcript",
        None,
        br#"{"transcript":"hello"}"#,
    );
    assert_status(&missing_transcript, 401);

    let invalid_transcript_json = http_json_request(
        address,
        "/custom/telegram/voice/transcript",
        Some(&auth_token),
        b"not-json",
    );
    assert_status(&invalid_transcript_json, 400);

    let empty_transcript = http_json_request(
        address,
        "/custom/telegram/voice/transcript",
        Some(&auth_token),
        br#"{"transcript":"   "}"#,
    );
    assert_status(&empty_transcript, 400);

    let accepted = http_json_request(
        address,
        "/custom/telegram/voice/transcript",
        Some(&auth_token),
        br#"{"transcript":"  ship the verified change  "}"#,
    );
    assert_status(&accepted, 202);
    let command = receiver
        .recv_timeout(StdDuration::from_secs(2))
        .expect("queued Mini App command");
    assert_eq!(command.identity, identity);
    assert_eq!(command.session_id, "session-network-coverage");
    assert_eq!(command.transcript, "ship the verified change");

    for index in 0..64 {
        let request = serde_json::to_vec(&json!({
            "transcript": format!("queued transcript {index}"),
        }))
        .expect("serialize queued transcript");
        let response = http_json_request(
            address,
            "/custom/telegram/voice/transcript",
            Some(&auth_token),
            &request,
        );
        assert_status(&response, 202);
    }
    let full = http_json_request(
        address,
        "/custom/telegram/voice/transcript",
        Some(&auth_token),
        br#"{"transcript":"queue overflow"}"#,
    );
    assert_status(&full, 429);

    drop(receiver);
    let disconnected = http_json_request(
        address,
        "/custom/telegram/voice/transcript",
        Some(&auth_token),
        br#"{"transcript":"runtime disconnected"}"#,
    );
    assert_status(&disconnected, 503);

    let oversized = raw_http_request(
        address,
        "POST /custom/telegram/voice/auth HTTP/1.1\r\nHost: localhost\r\nContent-Length: 70000\r\n\r\n",
    );
    assert_status(&oversized, 413);
    let chunked = raw_http_request(
        address,
        "POST /custom/telegram/voice/auth HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\nContent-Length: 0\r\n\r\n",
    );
    assert_status(&chunked, 400);

    cancelled.store(true, Ordering::Release);
    server_thread
        .join()
        .expect("mini app thread")
        .expect("mini app server result");
}

#[test]
fn webhook_server_exercises_authorization_parsing_and_handler_failures() {
    let server = TelegramWebhookServer::bind(TelegramWebhookConfig {
        bind: "127.0.0.1:0".parse().expect("loopback address"),
        path: "/telegram/webhook".to_owned(),
        secret_token: "valid_secret-42".to_owned(),
    })
    .expect("bind webhook server");
    let address = server.local_addr().expect("webhook address");
    let cancelled = Arc::new(AtomicBool::new(false));
    let server_cancelled = Arc::clone(&cancelled);
    let (updates_tx, updates_rx) = mpsc::channel();
    let server_thread = thread::spawn(move || {
        server.run_until_cancelled(&server_cancelled, move |update| {
            let update_id = update.update_id;
            updates_tx.send(update).map_err(|_| "receiver closed")?;
            if update_id == 8 {
                Err("deliberate handler failure")
            } else {
                Ok(())
            }
        })
    });

    assert_status(
        &http_request(address, "GET", "/telegram/webhook", &[], b""),
        405,
    );
    assert_status(
        &http_request(address, "POST", "/wrong", &[], br#"{"update_id":1}"#),
        404,
    );
    assert_status(
        &http_request(
            address,
            "POST",
            "/telegram/webhook",
            &[("X-Telegram-Bot-Api-Secret-Token", "wrong")],
            br#"{"update_id":2}"#,
        ),
        401,
    );
    assert_status(
        &raw_http_request(
            address,
            "POST /telegram/webhook HTTP/1.1\r\nHost: localhost\r\nX-Telegram-Bot-Api-Secret-Token: valid_secret-42\r\n\r\n",
        ),
        400,
    );
    assert_status(
        &raw_http_request(
            address,
            "POST /telegram/webhook HTTP/1.1\r\nHost: localhost\r\nX-Telegram-Bot-Api-Secret-Token: valid_secret-42\r\nContent-Length: 0\r\n\r\n",
        ),
        413,
    );
    assert_status(
        &http_request(
            address,
            "POST",
            "/telegram/webhook",
            &[("X-Telegram-Bot-Api-Secret-Token", "valid_secret-42")],
            b"not-json",
        ),
        400,
    );
    assert_status(
        &raw_http_request(
            address,
            "POST /telegram/webhook HTTP/1.1\r\nHost: localhost\r\nX-Telegram-Bot-Api-Secret-Token: valid_secret-42\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n\r\n{}",
        ),
        400,
    );

    let accepted = http_request(
        address,
        "POST",
        "/telegram/webhook",
        &[("X-Telegram-Bot-Api-Secret-Token", "valid_secret-42")],
        br#"{"update_id":7}"#,
    );
    assert_status(&accepted, 200);
    assert_eq!(
        updates_rx
            .recv_timeout(StdDuration::from_secs(2))
            .expect("accepted update")
            .update_id,
        7
    );

    let failed = http_request(
        address,
        "POST",
        "/telegram/webhook",
        &[("X-Telegram-Bot-Api-Secret-Token", "valid_secret-42")],
        br#"{"update_id":8}"#,
    );
    assert_status(&failed, 500);
    assert_eq!(
        updates_rx
            .recv_timeout(StdDuration::from_secs(2))
            .expect("failed update reached handler")
            .update_id,
        8
    );

    let too_large = raw_http_request(
        address,
        "POST /telegram/webhook HTTP/1.1\r\nHost: localhost\r\nX-Telegram-Bot-Api-Secret-Token: valid_secret-42\r\nContent-Length: 1048577\r\n\r\n",
    );
    assert_status(&too_large, 413);

    cancelled.store(true, Ordering::Release);
    server_thread
        .join()
        .expect("webhook thread")
        .expect("webhook server result");
}

#[test]
fn voice_pipeline_exercises_provider_success_validation_and_status_mapping() {
    assert!(OpenAiAudioToken::new("short").is_err());
    let token = OpenAiAudioToken::new("sk-test-token-1234567890").expect("audio token");
    assert!(
        TelegramVoicePipeline::new(
            token.clone(),
            "http://example.com",
            "transcribe",
            "tts",
            "alloy",
        )
        .is_err()
    );
    assert!(
        TelegramVoicePipeline::new(
            token.clone(),
            "http://127.0.0.1:1",
            "bad model",
            "tts",
            "alloy",
        )
        .is_err()
    );
    assert!(
        TelegramVoicePipeline::new(
            token.clone(),
            "http://127.0.0.1:1",
            "transcribe",
            "tts",
            "bad voice!",
        )
        .is_err()
    );

    let responses = VecDeque::from([
        MockResponse::json(200, br#"{"text":"  verified transcript  "}"#),
        MockResponse::json(200, br#"{"text":"   "}"#),
        MockResponse::text(401, "unauthorized"),
        MockResponse::bytes(200, "audio/ogg", b"OggSvalid-opus"),
        MockResponse::bytes(200, "audio/ogg", b"not-an-ogg"),
        MockResponse::text(429, "rate limited"),
        MockResponse::text(503, "unavailable"),
        MockResponse::text(400, "rejected"),
        MockResponse::json(200, b"not-json"),
    ]);
    let (api_base, requests, server_thread) = spawn_mock_http(responses);
    let pipeline = TelegramVoicePipeline::new(
        token,
        api_base,
        "gpt-4o-mini-transcribe",
        "gpt-4o-mini-tts",
        "alloy",
    )
    .expect("voice pipeline");
    let input = TelegramVoiceInput {
        file_name: "voice.ogg".to_owned(),
        mime_type: "audio/ogg".to_owned(),
        bytes: b"OggSinput-audio".to_vec(),
    };

    assert_eq!(
        pipeline.transcribe(&input).expect("transcript"),
        "verified transcript"
    );
    assert!(matches!(
        pipeline.transcribe(&input),
        Err(TelegramVoiceError::InvalidTranscript)
    ));
    assert!(matches!(
        pipeline.transcribe(&input),
        Err(TelegramVoiceError::Authentication)
    ));

    let voice = pipeline
        .synthesize("speak the result")
        .expect("synthesized voice");
    assert_eq!(voice.mime_type, "audio/ogg");
    assert!(voice.file_name.starts_with("medusa-"));
    assert!(voice.bytes.starts_with(b"OggS"));
    assert!(matches!(
        pipeline.synthesize("invalid stream"),
        Err(TelegramVoiceError::InvalidOggOpus)
    ));
    assert!(matches!(
        pipeline.synthesize("rate limited"),
        Err(TelegramVoiceError::RateLimited)
    ));
    assert!(matches!(
        pipeline.synthesize("provider unavailable"),
        Err(TelegramVoiceError::ProviderUnavailable)
    ));
    assert!(matches!(
        pipeline.synthesize("provider rejected"),
        Err(TelegramVoiceError::Rejected)
    ));
    assert!(matches!(
        pipeline.transcribe(&input),
        Err(TelegramVoiceError::MalformedResponse)
    ));

    assert!(matches!(
        pipeline.transcribe(&TelegramVoiceInput {
            file_name: "../voice.ogg".to_owned(),
            mime_type: "audio/ogg".to_owned(),
            bytes: vec![1],
        }),
        Err(TelegramVoiceError::InvalidInputAudio)
    ));
    assert!(matches!(
        pipeline.transcribe(&TelegramVoiceInput {
            file_name: "voice.ogg".to_owned(),
            mime_type: "application/octet-stream".to_owned(),
            bytes: vec![1],
        }),
        Err(TelegramVoiceError::InvalidInputAudio)
    ));
    assert!(matches!(
        pipeline.synthesize("   "),
        Err(TelegramVoiceError::InvalidSpeechInput)
    ));

    server_thread.join().expect("voice mock server");
    let requests = requests.lock().expect("captured requests");
    assert_eq!(requests.len(), 9);
    assert!(requests[0].starts_with("POST /v1/audio/transcriptions"));
    assert!(requests[0].lines().any(|line| {
        line.to_ascii_lowercase()
            .starts_with("authorization: bearer ")
    }));
    assert!(requests[0].contains("multipart/form-data; boundary=medusa-voice-"));
    assert!(requests[3].starts_with("POST /v1/audio/speech"));
    assert!(requests[3].contains("\"response_format\":\"opus\""));
}

fn signed_init_data(bot_token: &str, user_id: i64, now: OffsetDateTime) -> String {
    let mut fields = BTreeMap::new();
    fields.insert("auth_date", now.unix_timestamp().to_string());
    fields.insert("query_id", "coverage-query".to_owned());
    fields.insert("user", format!(r#"{{"id":{user_id}}}"#));
    let check = fields
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\n");
    let secret = hmac_sha256(b"WebAppData", bot_token.as_bytes());
    let signature = hex::encode(hmac_sha256(&secret, check.as_bytes()));
    fields
        .into_iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(&value)))
        .chain(std::iter::once(format!("hash={signature}")))
        .collect::<Vec<_>>()
        .join("&")
}

fn hmac_sha256(key: &[u8], value: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut normalized = [0_u8; BLOCK];
    if key.len() > BLOCK {
        normalized[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36_u8; BLOCK];
    let mut outer_pad = [0x5c_u8; BLOCK];
    for index in 0..BLOCK {
        inner_pad[index] ^= normalized[index];
        outer_pad[index] ^= normalized[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(value);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_hash);
    outer.finalize().into()
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn http_json_request(address: SocketAddr, path: &str, bearer: Option<&str>, body: &[u8]) -> String {
    let mut headers = vec![("Content-Type", "application/json")];
    let authorization;
    if let Some(token) = bearer {
        authorization = format!("Bearer {token}");
        headers.push(("Authorization", authorization.as_str()));
    }
    http_request(address, "POST", path, &headers, body)
}

fn http_request(
    address: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> String {
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    let mut stream = TcpStream::connect(address).expect("connect HTTP server");
    stream
        .set_read_timeout(Some(StdDuration::from_secs(5)))
        .expect("set read timeout");
    stream.write_all(request.as_bytes()).expect("write headers");
    stream.write_all(body).expect("write body");
    read_http_response(&mut stream)
}

fn raw_http_request(address: SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(address).expect("connect raw HTTP server");
    stream
        .set_read_timeout(Some(StdDuration::from_secs(5)))
        .expect("set raw read timeout");
    stream
        .write_all(request.as_bytes())
        .expect("write raw request");
    read_http_response(&mut stream)
}

fn read_http_response(stream: &mut TcpStream) -> String {
    let mut response = Vec::new();
    let mut chunk = [0_u8; 4_096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => response.extend_from_slice(&chunk[..read]),
            Err(error)
                if error.kind() == std::io::ErrorKind::ConnectionReset && !response.is_empty() =>
            {
                break;
            }
            Err(error) => panic!("read HTTP response: {error}"),
        }
    }
    String::from_utf8(response).expect("UTF-8 HTTP response")
}

fn assert_status(response: &str, status: u16) {
    assert!(
        response.starts_with(&format!("HTTP/1.1 {status} ")),
        "unexpected response: {response}"
    );
}

fn response_body(response: &str) -> &str {
    response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .expect("HTTP response body")
}

#[derive(Clone)]
struct MockResponse {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl MockResponse {
    fn json(status: u16, body: &[u8]) -> Self {
        Self::bytes(status, "application/json", body)
    }

    fn text(status: u16, body: &str) -> Self {
        Self::bytes(status, "text/plain", body.as_bytes())
    }

    fn bytes(status: u16, content_type: &'static str, body: &[u8]) -> Self {
        Self {
            status,
            content_type,
            body: body.to_vec(),
        }
    }
}

fn spawn_mock_http(
    responses: VecDeque<MockResponse>,
) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let address = listener.local_addr().expect("mock server address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let handle = thread::spawn(move || {
        for response in responses {
            let (mut stream, _) = listener.accept().expect("accept mock request");
            let request = read_complete_request(&mut stream);
            captured.lock().expect("capture request").push(request);
            let reason = match response.status {
                200 => "OK",
                400 => "Bad Request",
                401 => "Unauthorized",
                429 => "Too Many Requests",
                503 => "Service Unavailable",
                _ => "Error",
            };
            let headers = format!(
                "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.status,
                response.content_type,
                response.body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .expect("write mock headers");
            stream.write_all(&response.body).expect("write mock body");
            stream.flush().expect("flush mock response");
        }
    });
    (format!("http://{address}"), requests, handle)
}

fn read_complete_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(StdDuration::from_secs(5)))
        .expect("mock read timeout");
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).expect("read mock request");
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
        let read = stream.read(&mut chunk).expect("read mock request body");
        assert!(read > 0, "request closed before body");
        bytes.extend_from_slice(&chunk[..read]);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}
