use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use medusa_openai_realtime::{
    Event, GatewayCapability, SessionConfig, Transport, TransportError, Wire, WireKind,
};
use serde_json::{Value, json};

#[derive(Default)]
struct WireState {
    sent: Vec<Value>,
    incoming: VecDeque<Value>,
    reconnects: usize,
    closes: usize,
}

#[derive(Clone, Default)]
struct RecordingWire(Arc<Mutex<WireState>>);

impl RecordingWire {
    fn with_incoming(events: impl IntoIterator<Item = Value>) -> Self {
        let wire = Self::default();
        wire.0.lock().expect("wire state").incoming.extend(events);
        wire
    }

    fn sent_types(&self) -> Vec<String> {
        self.0
            .lock()
            .expect("wire state")
            .sent
            .iter()
            .filter_map(|value| value["type"].as_str().map(str::to_owned))
            .collect()
    }
}

impl Wire for RecordingWire {
    fn send_json(&mut self, payload: Value) -> Result<(), String> {
        self.0.lock().expect("wire state").sent.push(payload);
        Ok(())
    }

    fn receive_json(&mut self) -> Result<Option<Value>, String> {
        Ok(self.0.lock().expect("wire state").incoming.pop_front())
    }

    fn reconnect(&mut self) -> Result<(), String> {
        self.0.lock().expect("wire state").reconnects += 1;
        Ok(())
    }

    fn close(&mut self) -> Result<(), String> {
        self.0.lock().expect("wire state").closes += 1;
        Ok(())
    }
}

fn capability() -> GatewayCapability {
    GatewayCapability {
        available: true,
        reason: None,
        endpoint: Some("ws://127.0.0.1/realtime".to_owned()),
        model: Some("gpt-realtime".to_owned()),
        wire: Some(WireKind::WebSocket),
        supports_input_audio: true,
        supports_output_audio: true,
        supports_barge_in: true,
    }
}

#[test]
fn golden_protocol_sequence_covers_setup_audio_response_barge_in_and_reconnect() {
    let wire = RecordingWire::with_incoming([
        json!({ "type": "session.created" }),
        json!({ "type": "response.created", "response": { "id": "response-1" } }),
        json!({ "type": "response.audio.delta", "item_id": "item-1", "delta": "AAA=" }),
    ]);
    let probe = wire.clone();
    let mut transport =
        Transport::new(wire, capability(), SessionConfig::default()).expect("transport");

    transport.activate().expect("activate");
    transport
        .queue_input_audio("AQIDBA==".to_owned())
        .expect("input audio");
    transport.commit_input_audio().expect("commit audio");
    transport.request_response().expect("request response");
    assert_eq!(
        transport.next_event().expect("session event"),
        Some(Event::Connected)
    );
    assert!(matches!(
        transport.next_event().expect("response event"),
        Some(Event::ResponseStarted { response_id }) if response_id == "response-1"
    ));
    assert!(matches!(
        transport.next_event().expect("audio event"),
        Some(Event::AssistantAudioDelta { item_id, .. }) if item_id == "item-1"
    ));
    transport.barge_in(125).expect("barge in");
    transport.reconnect().expect("reconnect");
    transport.close().expect("close");

    assert_eq!(
        probe.sent_types(),
        vec![
            "session.update",
            "input_audio_buffer.append",
            "input_audio_buffer.commit",
            "response.create",
            "response.cancel",
            "conversation.item.truncate",
            "session.update",
        ]
    );
    let state = probe.0.lock().expect("wire state");
    assert_eq!(state.reconnects, 1);
    assert_eq!(state.closes, 1);
}

#[test]
fn recorded_events_translate_transcripts_activity_completion_rate_limits_and_errors() {
    let wire = RecordingWire::with_incoming([
        json!({ "type": "input_audio_buffer.speech_started" }),
        json!({ "type": "input_audio_buffer.speech_stopped" }),
        json!({ "type": "conversation.item.input_audio_transcription.delta", "item_id": "u1", "delta": "run " }),
        json!({ "type": "conversation.item.input_audio_transcription.completed", "item_id": "u1", "transcript": "run tests" }),
        json!({ "type": "response.audio_transcript.delta", "item_id": "a1", "delta": "Starting" }),
        json!({ "type": "response.audio_transcript.done", "item_id": "a1", "transcript": "Starting now" }),
        json!({ "type": "response.audio.done", "item_id": "a1" }),
        json!({ "type": "response.done", "response": { "id": "r1" } }),
        json!({ "type": "rate_limits.updated", "retry_after_ms": 500 }),
        json!({ "type": "error", "error": { "code": "expired_session", "message": "sign in again", "retryable": true } }),
    ]);
    let mut transport =
        Transport::new(wire, capability(), SessionConfig::default()).expect("transport");

    let events = std::iter::from_fn(|| transport.next_event().transpose())
        .collect::<Result<Vec<_>, _>>()
        .expect("translate fixtures");

    assert!(matches!(events[0], Event::UserSpeechStarted));
    assert!(matches!(events[1], Event::UserSpeechStopped));
    assert!(matches!(events[2], Event::UserTranscriptDelta { .. }));
    assert!(matches!(events[3], Event::UserTranscriptFinal { .. }));
    assert!(matches!(events[4], Event::AssistantTranscriptDelta { .. }));
    assert!(matches!(events[5], Event::AssistantTranscriptFinal { .. }));
    assert!(matches!(events[6], Event::AssistantAudioDone { .. }));
    assert!(matches!(events[7], Event::ResponseDone { .. }));
    assert!(matches!(
        events[8],
        Event::RateLimited {
            retry_after_ms: Some(500)
        }
    ));
    assert!(matches!(
        events[9],
        Event::Error {
            ref code,
            retryable: true,
            ..
        } if code == "expired_session"
    ));
}

#[test]
fn unavailable_or_expired_oauth_routes_fail_without_api_key_fallback_language() {
    for reason in [
        "existing ChatGPT OAuth session expired; sign in again",
        "authenticated route does not expose Realtime",
    ] {
        let result = Transport::new(
            RecordingWire::default(),
            GatewayCapability::unavailable(reason),
            SessionConfig::default(),
        );
        let error = match result {
            Err(TransportError::Unavailable(message)) => message,
            _ => panic!("expected unavailable route"),
        };
        assert!(!error.to_ascii_lowercase().contains("api key"));
        assert!(!error.to_ascii_lowercase().contains("token="));
    }
}

#[test]
fn protocol_errors_do_not_echo_sensitive_payload_fields() {
    let secret = "super-secret-transcript";
    let wire = RecordingWire::with_incoming([json!({
        "type": "unsupported.fixture",
        "authorization": "Bearer oauth-secret",
        "audio": "raw-sensitive-audio",
        "transcript": secret,
    })]);
    let mut transport =
        Transport::new(wire, capability(), SessionConfig::default()).expect("transport");

    let error = transport
        .next_event()
        .expect_err("unsupported event")
        .to_string();
    assert!(!error.contains("oauth-secret"));
    assert!(!error.contains("raw-sensitive-audio"));
    assert!(!error.contains(secret));
}
