//! OAuth-safe OpenAI Realtime transport for Medusa.
//!
//! OAuth credentials remain inside the configured loopback gateway. Medusa
//! probes an explicit capability endpoint and refuses microphone audio until
//! the authenticated route confirms full-duplex Realtime support.

use std::collections::VecDeque;
use std::time::Duration;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const CAPABILITY_PATH: &str = "realtime/capabilities";
const DEFAULT_AUDIO_QUEUE_CAPACITY: usize = 64;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WireKind {
    WebSocket,
    WebRtc,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GatewayCapability {
    pub available: bool,
    pub reason: Option<String>,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub wire: Option<WireKind>,
    #[serde(default)]
    pub supports_input_audio: bool,
    #[serde(default)]
    pub supports_output_audio: bool,
    #[serde(default)]
    pub supports_barge_in: bool,
}

impl GatewayCapability {
    #[must_use]
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            endpoint: None,
            model: None,
            wire: None,
            supports_input_audio: false,
            supports_output_audio: false,
            supports_barge_in: false,
        }
    }

    pub fn validate(&self) -> Result<(), TransportError> {
        if !self.available {
            return Err(TransportError::Unavailable(
                self.reason
                    .clone()
                    .unwrap_or_else(|| "authenticated route does not expose Realtime".to_owned()),
            ));
        }
        if self.endpoint.as_deref().is_none_or(str::is_empty) {
            return Err(TransportError::IncompatibleGateway(
                "capability response omitted a Realtime endpoint".to_owned(),
            ));
        }
        if self.wire.is_none() {
            return Err(TransportError::IncompatibleGateway(
                "capability response omitted a wire protocol".to_owned(),
            ));
        }
        if !self.supports_input_audio || !self.supports_output_audio {
            return Err(TransportError::Unavailable(
                "authenticated route does not support full-duplex audio".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportError {
    Unavailable(String),
    GatewayUnreachable(String),
    IncompatibleGateway(String),
    Protocol(String),
    Wire(String),
    Closed,
    Muted,
    InvalidAudioQueueCapacity,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(message) => write!(formatter, "Realtime unavailable: {message}"),
            Self::GatewayUnreachable(message) => write!(formatter, "OAuth gateway unavailable: {message}"),
            Self::IncompatibleGateway(message) => write!(formatter, "OAuth gateway is incompatible: {message}"),
            Self::Protocol(message) => write!(formatter, "Realtime protocol error: {message}"),
            Self::Wire(message) => write!(formatter, "Realtime wire error: {message}"),
            Self::Closed => write!(formatter, "Realtime transport is closed"),
            Self::Muted => write!(formatter, "microphone audio is muted"),
            Self::InvalidAudioQueueCapacity => write!(formatter, "audio queue capacity must be greater than zero"),
        }
    }
}

impl std::error::Error for TransportError {}

pub fn probe_gateway(base_url: &str) -> Result<GatewayCapability, TransportError> {
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| TransportError::GatewayUnreachable(error.to_string()))?;
    probe_gateway_with(&client, base_url)
}

pub fn probe_gateway_with(
    client: &Client,
    base_url: &str,
) -> Result<GatewayCapability, TransportError> {
    let endpoint = format!("{}/{CAPABILITY_PATH}", base_url.trim_end_matches('/'));
    let response = client
        .get(endpoint)
        .send()
        .map_err(|error| TransportError::GatewayUnreachable(error.to_string()))?;

    match response.status().as_u16() {
        404 => {
            return Ok(GatewayCapability::unavailable(
                "installed openai-oauth gateway does not advertise Realtime support; update the gateway or select a supported authenticated route",
            ));
        }
        401 | 403 => {
            return Ok(GatewayCapability::unavailable(
                "existing ChatGPT OAuth session is not authorized for Realtime; sign in again or use an account with Realtime access",
            ));
        }
        _ => {}
    }
    if !response.status().is_success() {
        return Err(TransportError::GatewayUnreachable(format!(
            "capability probe returned HTTP {}",
            response.status()
        )));
    }
    let capability = response
        .json::<GatewayCapability>()
        .map_err(|error| TransportError::IncompatibleGateway(error.to_string()))?;
    capability.validate()?;
    Ok(capability)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioFormat {
    Pcm16,
    G711Ulaw,
    G711Alaw,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionConfig {
    pub instructions: String,
    pub voice: String,
    pub input_audio_format: AudioFormat,
    pub output_audio_format: AudioFormat,
    pub transcription_model: String,
    pub server_vad: bool,
    pub interrupt_response: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            instructions: "Act as Medusa's realtime coding supervisor. Keep spoken updates concise and preserve tool and approval policy.".to_owned(),
            voice: "alloy".to_owned(),
            input_audio_format: AudioFormat::Pcm16,
            output_audio_format: AudioFormat::Pcm16,
            transcription_model: "gpt-4o-mini-transcribe".to_owned(),
            server_vad: true,
            interrupt_response: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    Connected,
    UserSpeechStarted,
    UserSpeechStopped,
    UserTranscriptDelta { item_id: String, delta: String },
    UserTranscriptFinal { item_id: String, transcript: String },
    AssistantTranscriptDelta { item_id: String, delta: String },
    AssistantTranscriptFinal { item_id: String, transcript: String },
    AssistantAudioDelta { item_id: String, audio_base64: String },
    AssistantAudioDone { item_id: String },
    ResponseStarted { response_id: String },
    ResponseDone { response_id: String },
    RateLimited { retry_after_ms: Option<u64> },
    Error { code: String, message: String, retryable: bool },
}

pub trait Wire: Send {
    fn send_json(&mut self, payload: Value) -> Result<(), String>;
    fn receive_json(&mut self) -> Result<Option<Value>, String>;
    fn reconnect(&mut self) -> Result<(), String>;
    fn close(&mut self) -> Result<(), String>;
}

pub struct Transport<W: Wire> {
    wire: W,
    capability: GatewayCapability,
    config: SessionConfig,
    pending_audio: VecDeque<String>,
    audio_capacity: usize,
    active_response_id: Option<String>,
    active_item_id: Option<String>,
    muted: bool,
    activated: bool,
    closed: bool,
}

impl<W: Wire> Transport<W> {
    pub fn new(
        wire: W,
        capability: GatewayCapability,
        config: SessionConfig,
    ) -> Result<Self, TransportError> {
        Self::with_audio_capacity(wire, capability, config, DEFAULT_AUDIO_QUEUE_CAPACITY)
    }

    pub fn with_audio_capacity(
        wire: W,
        capability: GatewayCapability,
        config: SessionConfig,
        audio_capacity: usize,
    ) -> Result<Self, TransportError> {
        capability.validate()?;
        if audio_capacity == 0 {
            return Err(TransportError::InvalidAudioQueueCapacity);
        }
        Ok(Self {
            wire,
            capability,
            config,
            pending_audio: VecDeque::with_capacity(audio_capacity),
            audio_capacity,
            active_response_id: None,
            active_item_id: None,
            muted: true,
            activated: false,
            closed: false,
        })
    }

    #[must_use]
    pub fn capability(&self) -> &GatewayCapability {
        &self.capability
    }

    pub fn activate(&mut self) -> Result<(), TransportError> {
        self.ensure_open()?;
        self.send(session_update(&self.config))?;
        self.activated = true;
        self.muted = false;
        self.flush_audio()
    }

    pub fn set_muted(&mut self, muted: bool) -> Result<(), TransportError> {
        self.ensure_open()?;
        self.muted = muted;
        if muted {
            self.pending_audio.clear();
            self.send(json!({ "type": "input_audio_buffer.clear" }))?;
        }
        Ok(())
    }

    pub fn queue_input_audio(&mut self, audio_base64: String) -> Result<(), TransportError> {
        self.ensure_open()?;
        if self.muted || !self.activated {
            return Err(TransportError::Muted);
        }
        if self.pending_audio.len() == self.audio_capacity {
            self.pending_audio.pop_front();
        }
        self.pending_audio.push_back(audio_base64);
        self.flush_audio()
    }

    pub fn commit_input_audio(&mut self) -> Result<(), TransportError> {
        self.ensure_open()?;
        self.flush_audio()?;
        self.send(json!({ "type": "input_audio_buffer.commit" }))
    }

    pub fn request_response(&mut self) -> Result<(), TransportError> {
        self.ensure_open()?;
        self.send(json!({ "type": "response.create" }))
    }

    pub fn barge_in(&mut self, played_audio_ms: u64) -> Result<(), TransportError> {
        self.ensure_open()?;
        if !self.capability.supports_barge_in {
            return Err(TransportError::Unavailable(
                "authenticated route does not support barge-in".to_owned(),
            ));
        }
        if self.active_response_id.is_some() {
            self.send(json!({ "type": "response.cancel" }))?;
        }
        if let Some(item_id) = self.active_item_id.clone() {
            self.send(json!({
                "type": "conversation.item.truncate",
                "item_id": item_id,
                "content_index": 0,
                "audio_end_ms": played_audio_ms,
            }))?;
        }
        self.active_response_id = None;
        self.active_item_id = None;
        Ok(())
    }

    pub fn next_event(&mut self) -> Result<Option<Event>, TransportError> {
        self.ensure_open()?;
        let Some(payload) = self
            .wire
            .receive_json()
            .map_err(TransportError::Wire)?
        else {
            return Ok(None);
        };
        let event = translate_event(&payload)?;
        match &event {
            Event::ResponseStarted { response_id } => {
                self.active_response_id = Some(response_id.clone());
            }
            Event::AssistantAudioDelta { item_id, .. } => {
                self.active_item_id = Some(item_id.clone());
            }
            Event::ResponseDone { .. } => {
                self.active_response_id = None;
                self.active_item_id = None;
            }
            _ => {}
        }
        Ok(Some(event))
    }

    pub fn reconnect(&mut self) -> Result<(), TransportError> {
        self.ensure_open()?;
        self.wire.reconnect().map_err(TransportError::Wire)?;
        if self.activated {
            self.send(session_update(&self.config))?;
        }
        Ok(())
    }

    pub fn close(&mut self) -> Result<(), TransportError> {
        if self.closed {
            return Ok(());
        }
        self.muted = true;
        self.pending_audio.clear();
        self.active_response_id = None;
        self.active_item_id = None;
        self.closed = true;
        self.wire.close().map_err(TransportError::Wire)
    }

    fn flush_audio(&mut self) -> Result<(), TransportError> {
        while let Some(audio) = self.pending_audio.pop_front() {
            self.send(json!({
                "type": "input_audio_buffer.append",
                "audio": audio,
            }))?;
        }
        Ok(())
    }

    fn send(&mut self, payload: Value) -> Result<(), TransportError> {
        self.wire.send_json(payload).map_err(TransportError::Wire)
    }

    fn ensure_open(&self) -> Result<(), TransportError> {
        if self.closed {
            Err(TransportError::Closed)
        } else {
            Ok(())
        }
    }
}

fn session_update(config: &SessionConfig) -> Value {
    json!({
        "type": "session.update",
        "session": {
            "instructions": config.instructions,
            "voice": config.voice,
            "input_audio_format": audio_format(&config.input_audio_format),
            "output_audio_format": audio_format(&config.output_audio_format),
            "input_audio_transcription": { "model": config.transcription_model },
            "turn_detection": if config.server_vad {
                json!({
                    "type": "server_vad",
                    "create_response": true,
                    "interrupt_response": config.interrupt_response,
                })
            } else {
                Value::Null
            },
        }
    })
}

fn audio_format(format: &AudioFormat) -> &'static str {
    match format {
        AudioFormat::Pcm16 => "pcm16",
        AudioFormat::G711Ulaw => "g711_ulaw",
        AudioFormat::G711Alaw => "g711_alaw",
    }
}

pub fn translate_event(payload: &Value) -> Result<Event, TransportError> {
    let kind = required_string(payload, "type")?;
    match kind {
        "session.created" | "session.updated" => Ok(Event::Connected),
        "input_audio_buffer.speech_started" => Ok(Event::UserSpeechStarted),
        "input_audio_buffer.speech_stopped" => Ok(Event::UserSpeechStopped),
        "conversation.item.input_audio_transcription.delta" => Ok(Event::UserTranscriptDelta {
            item_id: required_string(payload, "item_id")?.to_owned(),
            delta: required_string(payload, "delta")?.to_owned(),
        }),
        "conversation.item.input_audio_transcription.completed" => Ok(Event::UserTranscriptFinal {
            item_id: required_string(payload, "item_id")?.to_owned(),
            transcript: required_string(payload, "transcript")?.to_owned(),
        }),
        "response.audio_transcript.delta" => Ok(Event::AssistantTranscriptDelta {
            item_id: required_string(payload, "item_id")?.to_owned(),
            delta: required_string(payload, "delta")?.to_owned(),
        }),
        "response.audio_transcript.done" => Ok(Event::AssistantTranscriptFinal {
            item_id: required_string(payload, "item_id")?.to_owned(),
            transcript: required_string(payload, "transcript")?.to_owned(),
        }),
        "response.audio.delta" => Ok(Event::AssistantAudioDelta {
            item_id: required_string(payload, "item_id")?.to_owned(),
            audio_base64: required_string(payload, "delta")?.to_owned(),
        }),
        "response.audio.done" => Ok(Event::AssistantAudioDone {
            item_id: required_string(payload, "item_id")?.to_owned(),
        }),
        "response.created" => Ok(Event::ResponseStarted {
            response_id: nested_string(payload, &["response", "id"])?.to_owned(),
        }),
        "response.done" => Ok(Event::ResponseDone {
            response_id: nested_string(payload, &["response", "id"])?.to_owned(),
        }),
        "rate_limits.updated" => Ok(Event::RateLimited {
            retry_after_ms: payload.get("retry_after_ms").and_then(Value::as_u64),
        }),
        "error" => {
            let error = payload.get("error").unwrap_or(payload);
            let code = error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("realtime_error");
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Realtime service error");
            let retryable = matches!(
                error.get("type").and_then(Value::as_str),
                Some("server_error" | "rate_limit_error")
            );
            Ok(Event::Error {
                code: code.to_owned(),
                message: message.to_owned(),
                retryable,
            })
        }
        other => Err(TransportError::Protocol(format!(
            "unsupported server event `{other}`"
        ))),
    }
}

fn required_string<'a>(payload: &'a Value, field: &str) -> Result<&'a str, TransportError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| TransportError::Protocol(format!("event omitted string field `{field}`")))
}

fn nested_string<'a>(payload: &'a Value, path: &[&str]) -> Result<&'a str, TransportError> {
    let mut current = payload;
    for field in path {
        current = current.get(*field).ok_or_else(|| {
            TransportError::Protocol(format!("event omitted field `{}`", path.join(".")))
        })?;
    }
    current.as_str().ok_or_else(|| {
        TransportError::Protocol(format!("event field `{}` was not a string", path.join(".")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockWire {
        sent: Vec<Value>,
        incoming: VecDeque<Value>,
        reconnects: usize,
        closes: usize,
    }

    impl Wire for MockWire {
        fn send_json(&mut self, payload: Value) -> Result<(), String> {
            self.sent.push(payload);
            Ok(())
        }

        fn receive_json(&mut self) -> Result<Option<Value>, String> {
            Ok(self.incoming.pop_front())
        }

        fn reconnect(&mut self) -> Result<(), String> {
            self.reconnects += 1;
            Ok(())
        }

        fn close(&mut self) -> Result<(), String> {
            self.closes += 1;
            Ok(())
        }
    }

    fn capability() -> GatewayCapability {
        GatewayCapability {
            available: true,
            reason: None,
            endpoint: Some("ws://127.0.0.1:10531/realtime".to_owned()),
            model: Some("gpt-realtime".to_owned()),
            wire: Some(WireKind::WebSocket),
            supports_input_audio: true,
            supports_output_audio: true,
            supports_barge_in: true,
        }
    }

    #[test]
    fn unsupported_route_fails_before_audio_transport_exists() {
        let result = Transport::new(
            MockWire::default(),
            GatewayCapability::unavailable("route unsupported"),
            SessionConfig::default(),
        );
        assert!(matches!(result, Err(TransportError::Unavailable(_))));
    }

    #[test]
    fn audio_is_blocked_until_explicit_activation() {
        let mut transport = Transport::new(
            MockWire::default(),
            capability(),
            SessionConfig::default(),
        )
        .expect("transport");
        assert_eq!(
            transport.queue_input_audio("AAA=".to_owned()),
            Err(TransportError::Muted)
        );
    }

    #[test]
    fn session_creation_and_audio_streaming_are_protocol_correct() {
        let mut transport = Transport::new(
            MockWire::default(),
            capability(),
            SessionConfig::default(),
        )
        .expect("transport");
        transport.activate().expect("activate");
        transport
            .queue_input_audio("AAA=".to_owned())
            .expect("audio");
        assert_eq!(transport.wire.sent[0]["type"], "session.update");
        assert_eq!(transport.wire.sent[1]["type"], "input_audio_buffer.append");
        assert_eq!(transport.wire.sent[1]["audio"], "AAA=");
    }

    #[test]
    fn transcript_audio_and_error_events_translate() {
        assert_eq!(
            translate_event(&json!({
                "type": "response.audio_transcript.delta",
                "item_id": "item-1",
                "delta": "hello",
            }))
            .expect("event"),
            Event::AssistantTranscriptDelta {
                item_id: "item-1".to_owned(),
                delta: "hello".to_owned(),
            }
        );
        assert!(matches!(
            translate_event(&json!({
                "type": "response.audio.delta",
                "item_id": "item-1",
                "delta": "AAA=",
            }))
            .expect("event"),
            Event::AssistantAudioDelta { .. }
        ));
        assert!(matches!(
            translate_event(&json!({
                "type": "error",
                "error": {
                    "type": "rate_limit_error",
                    "code": "rate_limit",
                    "message": "slow down"
                }
            }))
            .expect("event"),
            Event::Error { retryable: true, .. }
        ));
    }

    #[test]
    fn barge_in_cancels_and_truncates_audio() {
        let mut wire = MockWire::default();
        wire.incoming.push_back(json!({
            "type": "response.created",
            "response": { "id": "response-1" }
        }));
        wire.incoming.push_back(json!({
            "type": "response.audio.delta",
            "item_id": "item-1",
            "delta": "AAA="
        }));
        let mut transport = Transport::new(wire, capability(), SessionConfig::default())
            .expect("transport");
        transport.next_event().expect("response");
        transport.next_event().expect("audio");
        transport.barge_in(420).expect("barge in");
        assert_eq!(transport.wire.sent[0]["type"], "response.cancel");
        assert_eq!(transport.wire.sent[1]["type"], "conversation.item.truncate");
        assert_eq!(transport.wire.sent[1]["audio_end_ms"], 420);
    }

    #[test]
    fn reconnect_restores_session_without_credentials() {
        let mut transport = Transport::new(
            MockWire::default(),
            capability(),
            SessionConfig::default(),
        )
        .expect("transport");
        transport.activate().expect("activate");
        transport.reconnect().expect("reconnect");
        assert_eq!(transport.wire.reconnects, 1);
        assert_eq!(transport.wire.sent.len(), 2);
        let payload = serde_json::to_string(&transport.wire.sent).expect("payload");
        assert!(!payload.to_ascii_lowercase().contains("authorization"));
        assert!(!payload.to_ascii_lowercase().contains("token"));
    }

    #[test]
    fn mute_and_close_stop_transmission_deterministically() {
        let mut transport = Transport::new(
            MockWire::default(),
            capability(),
            SessionConfig::default(),
        )
        .expect("transport");
        transport.activate().expect("activate");
        transport.set_muted(true).expect("mute");
        assert_eq!(transport.wire.sent.last().expect("clear")["type"], "input_audio_buffer.clear");
        assert_eq!(transport.queue_input_audio("AAA=".to_owned()), Err(TransportError::Muted));
        transport.close().expect("close");
        transport.close().expect("close again");
        assert_eq!(transport.wire.closes, 1);
        assert_eq!(transport.request_response(), Err(TransportError::Closed));
    }
}
