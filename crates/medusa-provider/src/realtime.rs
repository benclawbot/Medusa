//! OpenAI Realtime transport over Medusa's existing authenticated gateway.
//!
//! The transport never reads or stores OAuth credentials. Authentication remains
//! owned by the configured loopback gateway. Medusa first probes an explicit
//! gateway capability endpoint and refuses to send microphone audio until the
//! route has authoritatively declared Realtime support.

use std::collections::VecDeque;
use std::time::Duration;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const DEFAULT_CAPABILITY_PATH: &str = "realtime/capabilities";
const DEFAULT_MAX_PENDING_AUDIO: usize = 64;
const CAPABILITY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeWireKind {
    WebSocket,
    WebRtc,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RealtimeGatewayCapability {
    pub available: bool,
    pub reason: Option<String>,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub wire: Option<RealtimeWireKind>,
    #[serde(default)]
    pub supports_input_audio: bool,
    #[serde(default)]
    pub supports_output_audio: bool,
    #[serde(default)]
    pub supports_barge_in: bool,
}

impl RealtimeGatewayCapability {
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

    pub fn validate(&self) -> Result<(), RealtimeTransportError> {
        if !self.available {
            return Err(RealtimeTransportError::Unavailable(
                self.reason.clone().unwrap_or_else(|| {
                    "the authenticated gateway does not expose Realtime".to_owned()
                }),
            ));
        }
        if self.endpoint.as_deref().is_none_or(str::is_empty) {
            return Err(RealtimeTransportError::IncompatibleGateway(
                "Realtime capability response omitted an endpoint".to_owned(),
            ));
        }
        if self.wire.is_none() {
            return Err(RealtimeTransportError::IncompatibleGateway(
                "Realtime capability response omitted a wire protocol".to_owned(),
            ));
        }
        if !self.supports_input_audio || !self.supports_output_audio {
            return Err(RealtimeTransportError::Unavailable(
                "the authenticated route does not support full-duplex audio".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RealtimeTransportError {
    Unavailable(String),
    GatewayUnreachable(String),
    IncompatibleGateway(String),
    Protocol(String),
    Wire(String),
    Closed,
    Muted,
    AudioQueueCapacity,
}

impl std::fmt::Display for RealtimeTransportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(message) => write!(formatter, "Realtime unavailable: {message}"),
            Self::GatewayUnreachable(message) => write!(formatter, "OAuth gateway unavailable: {message}"),
            Self::IncompatibleGateway(message) => write!(formatter, "OAuth gateway is incompatible: {message}"),
            Self::Protocol(message) => write!(formatter, "Realtime protocol error: {message}"),
            Self::Wire(message) => write!(formatter, "Realtime transport failed: {message}"),
            Self::Closed => write!(formatter, "Realtime transport is closed"),
            Self::Muted => write!(formatter, "microphone audio is muted"),
            Self::AudioQueueCapacity => write!(formatter, "audio queue capacity must be greater than zero"),
        }
    }
}

impl std::error::Error for RealtimeTransportError {}

pub fn probe_realtime_capability(
    base_url: &str,
) -> Result<RealtimeGatewayCapability, RealtimeTransportError> {
    let client = Client::builder()
        .timeout(CAPABILITY_TIMEOUT)
        .build()
        .map_err(|error| RealtimeTransportError::GatewayUnreachable(error.to_string()))?;
    probe_realtime_capability_with(&client, base_url)
}

pub fn probe_realtime_capability_with(
    client: &Client,
    base_url: &str,
) -> Result<RealtimeGatewayCapability, RealtimeTransportError> {
    let endpoint = format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        DEFAULT_CAPABILITY_PATH
    );
    let response = client
        .get(&endpoint)
        .send()
        .map_err(|error| RealtimeTransportError::GatewayUnreachable(error.to_string()))?;
    if response.status().as_u16() == 404 {
        return Ok(RealtimeGatewayCapability::unavailable(
            "the installed openai-oauth gateway does not advertise Realtime support; update the gateway or select a supported authenticated route",
        ));
    }
    if response.status().as_u16() == 401 || response.status().as_u16() == 403 {
        return Ok(RealtimeGatewayCapability::unavailable(
            "the existing ChatGPT OAuth session is not authorized for Realtime; sign in again or use an account with Realtime access",
        ));
    }
    if !response.status().is_success() {
        return Err(RealtimeTransportError::GatewayUnreachable(format!(
            "capability probe returned HTTP {}",
            response.status()
        )));
    }
    let capability = response
        .json::<RealtimeGatewayCapability>()
        .map_err(|error| RealtimeTransportError::IncompatibleGateway(error.to_string()))?;
    capability.validate()?;
    Ok(capability)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeAudioFormat {
    Pcm16,
    G711Ulaw,
    G711Alaw,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RealtimeSessionConfig {
    pub instructions: String,
    pub voice: String,
    pub input_audio_format: RealtimeAudioFormat,
    pub output_audio_format: RealtimeAudioFormat,
    pub input_transcription_model: String,
    pub server_vad: bool,
    pub interrupt_response: bool,
}

impl Default for RealtimeSessionConfig {
    fn default() -> Self {
        Self {
            instructions: "Act as Medusa's realtime coding supervisor. Keep spoken updates concise and preserve approvals and tool policy.".to_owned(),
            voice: "alloy".to_owned(),
            input_audio_format: RealtimeAudioFormat::Pcm16,
            output_audio_format: RealtimeAudioFormat::Pcm16,
            input_transcription_model: "gpt-4o-mini-transcribe".to_owned(),
            server_vad: true,
            interrupt_response: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RealtimeTransportEvent {
    Connected,
    Disconnected { reason: Option<String>, retryable: bool },
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

pub trait RealtimeWire: Send {
    fn send_json(&mut self, payload: Value) -> Result<(), String>;
    fn receive_json(&mut self) -> Result<Option<Value>, String>;
    fn reconnect(&mut self) -> Result<(), String>;
    fn close(&mut self) -> Result<(), String>;
}

pub struct OpenAiRealtimeTransport<W: RealtimeWire> {
    wire: W,
    capability: RealtimeGatewayCapability,
    config: RealtimeSessionConfig,
    pending_audio: VecDeque<String>,
    max_pending_audio: usize,
    active_response_id: Option<String>,
    active_item_id: Option<String>,
    muted: bool,
    activated: bool,
    closed: bool,
}

impl<W: RealtimeWire> OpenAiRealtimeTransport<W> {
    pub fn new(
        wire: W,
        capability: RealtimeGatewayCapability,
        config: RealtimeSessionConfig,
    ) -> Result<Self, RealtimeTransportError> {
        Self::with_audio_capacity(wire, capability, config, DEFAULT_MAX_PENDING_AUDIO)
    }

    pub fn with_audio_capacity(
        wire: W,
        capability: RealtimeGatewayCapability,
        config: RealtimeSessionConfig,
        max_pending_audio: usize,
    ) -> Result<Self, RealtimeTransportError> {
        capability.validate()?;
        if max_pending_audio == 0 {
            return Err(RealtimeTransportError::AudioQueueCapacity);
        }
        Ok(Self {
            wire,
            capability,
            config,
            pending_audio: VecDeque::with_capacity(max_pending_audio),
            max_pending_audio,
            active_response_id: None,
            active_item_id: None,
            muted: true,
            activated: false,
            closed: false,
        })
    }

    #[must_use]
    pub fn capability(&self) -> &RealtimeGatewayCapability {
        &self.capability
    }

    #[must_use]
    pub fn is_muted(&self) -> bool {
        self.muted
    }

    pub fn activate(&mut self) -> Result<(), RealtimeTransportError> {
        self.ensure_open()?;
        self.send(session_update(&self.config))?;
        self.activated = true;
        self.muted = false;
        self.flush_audio()
    }

    pub fn set_muted(&mut self, muted: bool) -> Result<(), RealtimeTransportError> {
        self.ensure_open()?;
        self.muted = muted;
        if muted {
            self.pending_audio.clear();
            self.send(json!({ "type": "input_audio_buffer.clear" }))?;
        }
        Ok(())
    }

    pub fn queue_input_audio(&mut self, audio_base64: String) -> Result<(), RealtimeTransportError> {
        self.ensure_open()?;
        if self.muted || !self.activated {
            return Err(RealtimeTransportError::Muted);
        }
        if self.pending_audio.len() == self.max_pending_audio {
            self.pending_audio.pop_front();
        }
        self.pending_audio.push_back(audio_base64);
        self.flush_audio()
    }

    pub fn commit_input_audio(&mut self) -> Result<(), RealtimeTransportError> {
        self.ensure_open()?;
        self.flush_audio()?;
        self.send(json!({ "type": "input_audio_buffer.commit" }))
    }

    pub fn request_response(&mut self) -> Result<(), RealtimeTransportError> {
        self.ensure_open()?;
        self.send(json!({ "type": "response.create" }))
    }

    pub fn barge_in(&mut self, played_audio_ms: u64) -> Result<(), RealtimeTransportError> {
        self.ensure_open()?;
        if !self.capability.supports_barge_in {
            return Err(RealtimeTransportError::Unavailable(
                "the authenticated Realtime route does not support barge-in".to_owned(),
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

    pub fn next_event(&mut self) -> Result<Option<RealtimeTransportEvent>, RealtimeTransportError> {
        self.ensure_open()?;
        let Some(payload) = self
            .wire
            .receive_json()
            .map_err(RealtimeTransportError::Wire)?
        else {
            return Ok(None);
        };
        let event = translate_server_event(&payload)?;
        match &event {
            RealtimeTransportEvent::ResponseStarted { response_id } => {
                self.active_response_id = Some(response_id.clone());
            }
            RealtimeTransportEvent::AssistantAudioDelta { item_id, .. } => {
                self.active_item_id = Some(item_id.clone());
            }
            RealtimeTransportEvent::ResponseDone { .. } => {
                self.active_response_id = None;
                self.active_item_id = None;
            }
            _ => {}
        }
        Ok(Some(event))
    }

    pub fn reconnect(&mut self) -> Result<(), RealtimeTransportError> {
        self.ensure_open()?;
        self.wire.reconnect().map_err(RealtimeTransportError::Wire)?;
        if self.activated {
            self.send(session_update(&self.config))?;
        }
        Ok(())
    }

    pub fn close(&mut self) -> Result<(), RealtimeTransportError> {
        if self.closed {
            return Ok(());
        }
        self.muted = true;
        self.pending_audio.clear();
        self.active_response_id = None;
        self.active_item_id = None;
        self.closed = true;
        self.wire.close().map_err(RealtimeTransportError::Wire)
    }

    fn flush_audio(&mut self) -> Result<(), RealtimeTransportError> {
        while let Some(audio) = self.pending_audio.pop_front() {
            self.send(json!({
                "type": "input_audio_buffer.append",
                "audio": audio,
            }))?;
        }
        Ok(())
    }

    fn send(&mut self, payload: Value) -> Result<(), RealtimeTransportError> {
        self.wire
            .send_json(payload)
            .map_err(RealtimeTransportError::Wire)
    }

    fn ensure_open(&self) -> Result<(), RealtimeTransportError> {
        if self.closed {
            Err(RealtimeTransportError::Closed)
        } else {
            Ok(())
        }
    }
}

fn session_update(config: &RealtimeSessionConfig) -> Value {
    json!({
        "type": "session.update",
        "session": {
            "instructions": config.instructions,
            "voice": config.voice,
            "input_audio_format": audio_format_name(&config.input_audio_format),
            "output_audio_format": audio_format_name(&config.output_audio_format),
            "input_audio_transcription": { "model": config.input_transcription_model },
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

fn audio_format_name(format: &RealtimeAudioFormat) -> &'static str {
    match format {
        RealtimeAudioFormat::Pcm16 => "pcm16",
        RealtimeAudioFormat::G711Ulaw => "g711_ulaw",
        RealtimeAudioFormat::G711Alaw => "g711_alaw",
    }
}

pub fn translate_server_event(payload: &Value) -> Result<RealtimeTransportEvent, RealtimeTransportError> {
    let event_type = required_str(payload, "type")?;
    match event_type {
        "session.created" | "session.updated" => Ok(RealtimeTransportEvent::Connected),
        "input_audio_buffer.speech_started" => Ok(RealtimeTransportEvent::UserSpeechStarted),
        "input_audio_buffer.speech_stopped" => Ok(RealtimeTransportEvent::UserSpeechStopped),
        "conversation.item.input_audio_transcription.delta" => Ok(
            RealtimeTransportEvent::UserTranscriptDelta {
                item_id: required_str(payload, "item_id")?.to_owned(),
                delta: required_str(payload, "delta")?.to_owned(),
            },
        ),
        "conversation.item.input_audio_transcription.completed" => Ok(
            RealtimeTransportEvent::UserTranscriptFinal {
                item_id: required_str(payload, "item_id")?.to_owned(),
                transcript: required_str(payload, "transcript")?.to_owned(),
            },
        ),
        "response.audio_transcript.delta" => Ok(RealtimeTransportEvent::AssistantTranscriptDelta {
            item_id: required_str(payload, "item_id")?.to_owned(),
            delta: required_str(payload, "delta")?.to_owned(),
        }),
        "response.audio_transcript.done" => Ok(RealtimeTransportEvent::AssistantTranscriptFinal {
            item_id: required_str(payload, "item_id")?.to_owned(),
            transcript: required_str(payload, "transcript")?.to_owned(),
        }),
        "response.audio.delta" => Ok(RealtimeTransportEvent::AssistantAudioDelta {
            item_id: required_str(payload, "item_id")?.to_owned(),
            audio_base64: required_str(payload, "delta")?.to_owned(),
        }),
        "response.audio.done" => Ok(RealtimeTransportEvent::AssistantAudioDone {
            item_id: required_str(payload, "item_id")?.to_owned(),
        }),
        "response.created" => Ok(RealtimeTransportEvent::ResponseStarted {
            response_id: nested_required_str(payload, &["response", "id"])?.to_owned(),
        }),
        "response.done" => Ok(RealtimeTransportEvent::ResponseDone {
            response_id: nested_required_str(payload, &["response", "id"])?.to_owned(),
        }),
        "rate_limits.updated" => Ok(RealtimeTransportEvent::RateLimited {
            retry_after_ms: payload.get("retry_after_ms").and_then(Value::as_u64),
        }),
        "error" => {
            let error = payload.get("error").unwrap_or(payload);
            let code = error.get("code").and_then(Value::as_str).unwrap_or("realtime_error");
            let message = error.get("message").and_then(Value::as_str).unwrap_or("Realtime service error");
            let retryable = matches!(
                error.get("type").and_then(Value::as_str),
                Some("server_error" | "rate_limit_error")
            );
            Ok(RealtimeTransportEvent::Error {
                code: code.to_owned(),
                message: message.to_owned(),
                retryable,
            })
        }
        other => Err(RealtimeTransportError::Protocol(format!(
            "unsupported server event `{other}`"
        ))),
    }
}

fn required_str<'a>(payload: &'a Value, field: &str) -> Result<&'a str, RealtimeTransportError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| RealtimeTransportError::Protocol(format!("event omitted string field `{field}`")))
}

fn nested_required_str<'a>(
    payload: &'a Value,
    path: &[&str],
) -> Result<&'a str, RealtimeTransportError> {
    let mut current = payload;
    for field in path {
        current = current.get(*field).ok_or_else(|| {
            RealtimeTransportError::Protocol(format!("event omitted field `{}`", path.join(".")))
        })?;
    }
    current.as_str().ok_or_else(|| {
        RealtimeTransportError::Protocol(format!("event field `{}` was not a string", path.join(".")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockWire {
        sent: Vec<Value>,
        received: VecDeque<Value>,
        reconnects: usize,
        closes: usize,
    }

    impl RealtimeWire for MockWire {
        fn send_json(&mut self, payload: Value) -> Result<(), String> {
            self.sent.push(payload);
            Ok(())
        }

        fn receive_json(&mut self) -> Result<Option<Value>, String> {
            Ok(self.received.pop_front())
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

    fn capability() -> RealtimeGatewayCapability {
        RealtimeGatewayCapability {
            available: true,
            reason: None,
            endpoint: Some("ws://127.0.0.1:10531/realtime".to_owned()),
            model: Some("gpt-realtime".to_owned()),
            wire: Some(RealtimeWireKind::WebSocket),
            supports_input_audio: true,
            supports_output_audio: true,
            supports_barge_in: true,
        }
    }

    #[test]
    fn refuses_audio_before_explicit_activation() {
        let mut transport = OpenAiRealtimeTransport::new(
            MockWire::default(),
            capability(),
            RealtimeSessionConfig::default(),
        )
        .expect("transport");
        assert_eq!(
            transport.queue_input_audio("audio".to_owned()),
            Err(RealtimeTransportError::Muted)
        );
    }

    #[test]
    fn activation_configures_session_then_streams_audio() {
        let mut transport = OpenAiRealtimeTransport::new(
            MockWire::default(),
            capability(),
            RealtimeSessionConfig::default(),
        )
        .expect("transport");
        transport.activate().expect("activate");
        transport.queue_input_audio("AAA=".to_owned()).expect("audio");
        assert_eq!(transport.wire.sent[0]["type"], "session.update");
        assert_eq!(transport.wire.sent[1]["type"], "input_audio_buffer.append");
        assert_eq!(transport.wire.sent[1]["audio"], "AAA=");
    }

    #[test]
    fn mute_clears_audio_and_blocks_further_transmission() {
        let mut transport = OpenAiRealtimeTransport::new(
            MockWire::default(),
            capability(),
            RealtimeSessionConfig::default(),
        )
        .expect("transport");
        transport.activate().expect("activate");
        transport.set_muted(true).expect("mute");
        assert_eq!(transport.wire.sent.last().expect("event")["type"], "input_audio_buffer.clear");
        assert_eq!(transport.queue_input_audio("AAA=".to_owned()), Err(RealtimeTransportError::Muted));
    }

    #[test]
    fn translates_transcript_and_audio_events() {
        let transcript = translate_server_event(&json!({
            "type": "response.audio_transcript.delta",
            "item_id": "item-1",
            "delta": "hello",
        }))
        .expect("transcript");
        assert_eq!(
            transcript,
            RealtimeTransportEvent::AssistantTranscriptDelta {
                item_id: "item-1".to_owned(),
                delta: "hello".to_owned(),
            }
        );
        let audio = translate_server_event(&json!({
            "type": "response.audio.delta",
            "item_id": "item-1",
            "delta": "AAA=",
        }))
        .expect("audio");
        assert!(matches!(audio, RealtimeTransportEvent::AssistantAudioDelta { .. }));
    }

    #[test]
    fn barge_in_cancels_response_and_truncates_played_audio() {
        let mut wire = MockWire::default();
        wire.received.push_back(json!({
            "type": "response.created",
            "response": { "id": "response-1" },
        }));
        wire.received.push_back(json!({
            "type": "response.audio.delta",
            "item_id": "item-1",
            "delta": "AAA=",
        }));
        let mut transport = OpenAiRealtimeTransport::new(
            wire,
            capability(),
            RealtimeSessionConfig::default(),
        )
        .expect("transport");
        transport.next_event().expect("response");
        transport.next_event().expect("audio");
        transport.barge_in(420).expect("barge in");
        let sent = &transport.wire.sent;
        assert_eq!(sent[0]["type"], "response.cancel");
        assert_eq!(sent[1]["type"], "conversation.item.truncate");
        assert_eq!(sent[1]["audio_end_ms"], 420);
    }

    #[test]
    fn reconnect_reapplies_session_without_credentials() {
        let mut transport = OpenAiRealtimeTransport::new(
            MockWire::default(),
            capability(),
            RealtimeSessionConfig::default(),
        )
        .expect("transport");
        transport.activate().expect("activate");
        transport.reconnect().expect("reconnect");
        assert_eq!(transport.wire.reconnects, 1);
        assert_eq!(transport.wire.sent.len(), 2);
        let serialized = serde_json::to_string(&transport.wire.sent).expect("serialize");
        assert!(!serialized.to_ascii_lowercase().contains("token"));
        assert!(!serialized.to_ascii_lowercase().contains("authorization"));
    }

    #[test]
    fn unsupported_gateway_fails_before_transport_creation() {
        let result = OpenAiRealtimeTransport::new(
            MockWire::default(),
            RealtimeGatewayCapability::unavailable("route unsupported"),
            RealtimeSessionConfig::default(),
        );
        assert!(matches!(result, Err(RealtimeTransportError::Unavailable(_))));
    }

    #[test]
    fn close_is_idempotent_and_stops_audio() {
        let mut transport = OpenAiRealtimeTransport::new(
            MockWire::default(),
            capability(),
            RealtimeSessionConfig::default(),
        )
        .expect("transport");
        transport.activate().expect("activate");
        transport.close().expect("close");
        transport.close().expect("close again");
        assert_eq!(transport.wire.closes, 1);
        assert_eq!(transport.queue_input_audio("AAA=".to_owned()), Err(RealtimeTransportError::Closed));
    }
}
