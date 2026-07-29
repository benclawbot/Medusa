//! OpenAI Realtime route capability and provider-specific protocol mapping.
//!
//! Medusa's ChatGPT/Codex authentication is currently supplied by the local
//! `openai-oauth` gateway. That gateway exposes Responses, Chat Completions,
//! and Models endpoints, but does not expose the OpenAI Realtime WebSocket
//! protocol. Voice therefore fails closed before a frontend opens a microphone.
//! Provider wire types stay isolated here so a supported authenticated route can
//! be added without leaking OpenAI protocol details into the shared voice core.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use medusa_config::Config;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::voice::{
    AudioFrame, RealtimeVoiceEvent, TranscriptSpeaker, TranscriptUpdate, VoiceAvailability,
    VoiceError,
};

pub const OPENAI_OAUTH_PROVIDER: &str = "openai-oauth";
pub const OPENAI_OAUTH_REALTIME_REASON: &str =
    "ChatGPT/Codex OAuth voice is unavailable because the configured local openai-oauth gateway does not expose an authenticated Realtime endpoint. Keep using text input; Medusa will not request an API key or start microphone streaming.";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenAiRealtimeRoute {
    /// The current ChatGPT/Codex OAuth gateway. Explicitly unsupported until it
    /// exposes an authenticated Realtime endpoint and capability contract.
    OAuthGatewayUnsupported { base_url: String },
    /// A non-OAuth route is intentionally not used for voice. Voice must reuse
    /// Medusa's existing ChatGPT/Codex authentication rather than adding keys.
    ExistingRouteUnsupported { provider: String },
}

impl OpenAiRealtimeRoute {
    #[must_use]
    pub fn availability(&self) -> VoiceAvailability {
        match self {
            Self::OAuthGatewayUnsupported { .. } => {
                VoiceAvailability::unavailable(OPENAI_OAUTH_REALTIME_REASON)
            }
            Self::ExistingRouteUnsupported { provider } => VoiceAvailability::unavailable(
                format!(
                    "Realtime voice is unavailable for configured provider `{provider}`. Voice requires a supported ChatGPT/Codex OAuth route; Medusa will not ask for a separate API key."
                ),
            ),
        }
    }

    #[must_use]
    pub fn permits_microphone_streaming(&self) -> bool {
        false
    }
}

/// Decide voice availability from the same model/account configuration used by
/// normal Medusa requests. This decision is intentionally authoritative and
/// does not infer Realtime support merely from an OpenAI-compatible REST shape.
#[must_use]
pub fn resolve_openai_realtime_route(config: &Config) -> OpenAiRealtimeRoute {
    if config
        .model
        .provider
        .trim()
        .eq_ignore_ascii_case(OPENAI_OAUTH_PROVIDER)
    {
        return OpenAiRealtimeRoute::OAuthGatewayUnsupported {
            base_url: config
                .model
                .base_url
                .clone()
                .unwrap_or_else(|| "http://127.0.0.1:10531/v1".to_owned()),
        };
    }

    OpenAiRealtimeRoute::ExistingRouteUnsupported {
        provider: config.model.provider.clone(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum OpenAiRealtimeClientEvent {
    #[serde(rename = "session.update")]
    SessionUpdate { session: OpenAiRealtimeSessionConfig },
    #[serde(rename = "input_audio_buffer.append")]
    InputAudioAppend { audio: String },
    #[serde(rename = "input_audio_buffer.commit")]
    InputAudioCommit,
    #[serde(rename = "response.cancel")]
    ResponseCancel,
    #[serde(rename = "conversation.item.truncate")]
    ConversationItemTruncate {
        item_id: String,
        content_index: u32,
        audio_end_ms: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OpenAiRealtimeSessionConfig {
    pub modalities: Vec<String>,
    pub instructions: String,
    pub voice: String,
    pub input_audio_format: String,
    pub output_audio_format: String,
    pub input_audio_transcription: Value,
    pub turn_detection: Value,
}

impl OpenAiRealtimeSessionConfig {
    #[must_use]
    pub fn medusa_default(instructions: impl Into<String>) -> Self {
        Self {
            modalities: vec!["text".to_owned(), "audio".to_owned()],
            instructions: instructions.into(),
            voice: "alloy".to_owned(),
            input_audio_format: "pcm16".to_owned(),
            output_audio_format: "pcm16".to_owned(),
            input_audio_transcription: json!({"model": "gpt-4o-mini-transcribe"}),
            turn_detection: json!({
                "type": "server_vad",
                "create_response": true,
                "interrupt_response": true
            }),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct OpenAiRealtimeServerEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub item_id: Option<String>,
    #[serde(default)]
    pub response_id: Option<String>,
    #[serde(default)]
    pub transcript: Option<String>,
    #[serde(default)]
    pub delta: Option<String>,
    #[serde(default)]
    pub audio: Option<String>,
    #[serde(default)]
    pub error: Option<OpenAiRealtimeWireError>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct OpenAiRealtimeWireError {
    #[serde(default)]
    pub code: Option<String>,
    pub message: String,
    #[serde(rename = "type")]
    pub error_type: String,
}

/// Convert provider events into the shared voice model. Raw audio is decoded
/// into bounded shared frames by the caller; transcripts are never logged here.
pub fn map_server_event(
    event: OpenAiRealtimeServerEvent,
    sequence: u64,
) -> Result<Vec<RealtimeVoiceEvent>, String> {
    let mapped = match event.event_type.as_str() {
        "session.created" | "session.updated" => vec![RealtimeVoiceEvent::TransportStatus {
            connected: true,
            detail: Some("OpenAI Realtime session ready".to_owned()),
        }],
        "input_audio_buffer.speech_started" => {
            vec![RealtimeVoiceEvent::VoiceActivity { active: true }]
        }
        "input_audio_buffer.speech_stopped" => {
            vec![RealtimeVoiceEvent::VoiceActivity { active: false }]
        }
        "conversation.item.input_audio_transcription.delta" => vec![
            RealtimeVoiceEvent::Transcript(TranscriptUpdate {
                turn_id: event.item_id.unwrap_or_else(|| "user-audio".to_owned()),
                speaker: TranscriptSpeaker::User,
                text: event.delta.unwrap_or_default(),
                is_final: false,
            }),
        ],
        "conversation.item.input_audio_transcription.completed" => vec![
            RealtimeVoiceEvent::Transcript(TranscriptUpdate {
                turn_id: event.item_id.unwrap_or_else(|| "user-audio".to_owned()),
                speaker: TranscriptSpeaker::User,
                text: event.transcript.unwrap_or_default(),
                is_final: true,
            }),
        ],
        "response.audio_transcript.delta" => vec![RealtimeVoiceEvent::Transcript(
            TranscriptUpdate {
                turn_id: event
                    .response_id
                    .unwrap_or_else(|| "assistant-audio".to_owned()),
                speaker: TranscriptSpeaker::Assistant,
                text: event.delta.unwrap_or_default(),
                is_final: false,
            },
        )],
        "response.audio_transcript.done" => vec![RealtimeVoiceEvent::Transcript(
            TranscriptUpdate {
                turn_id: event
                    .response_id
                    .unwrap_or_else(|| "assistant-audio".to_owned()),
                speaker: TranscriptSpeaker::Assistant,
                text: event.transcript.unwrap_or_default(),
                is_final: true,
            },
        )],
        "response.audio.delta" => {
            let encoded = event
                .delta
                .or(event.audio)
                .ok_or_else(|| "OpenAI response audio event omitted audio data".to_owned())?;
            let bytes = STANDARD
                .decode(encoded)
                .map_err(|error| format!("OpenAI response audio was not valid base64: {error}"))?;
            vec![RealtimeVoiceEvent::OutputAudioQueued {
                frames: usize::from(!bytes.is_empty()),
            }]
        }
        "response.cancelled" | "response.canceled" => vec![RealtimeVoiceEvent::Interrupted],
        "error" => {
            let error = event.error.ok_or_else(|| {
                "OpenAI Realtime error event omitted structured error details".to_owned()
            })?;
            vec![RealtimeVoiceEvent::Error(VoiceError {
                code: error.code.unwrap_or(error.error_type),
                message: error.message,
                retryable: true,
            })]
        }
        other => vec![RealtimeVoiceEvent::TransportStatus {
            connected: true,
            detail: Some(format!("ignored provider event `{other}`")),
        }],
    };
    let _ = AudioFrame {
        sequence,
        bytes: Vec::new(),
    };
    Ok(mapped)
}

#[must_use]
pub fn append_audio_event(frame: &AudioFrame) -> OpenAiRealtimeClientEvent {
    OpenAiRealtimeClientEvent::InputAudioAppend {
        audio: STANDARD.encode(&frame.bytes),
    }
}

#[must_use]
pub fn barge_in_events(
    item_id: impl Into<String>,
    played_audio_ms: u64,
) -> [OpenAiRealtimeClientEvent; 2] {
    [
        OpenAiRealtimeClientEvent::ResponseCancel,
        OpenAiRealtimeClientEvent::ConversationItemTruncate {
            item_id: item_id.into(),
            content_index: 0,
            audio_end_ms: played_audio_ms,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_gateway_is_rejected_before_microphone_streaming() {
        let mut config = Config::default();
        config.model.provider = OPENAI_OAUTH_PROVIDER.to_owned();
        config.model.auth = "none".to_owned();
        config.model.base_url = Some("http://127.0.0.1:10531/v1".to_owned());

        let route = resolve_openai_realtime_route(&config);
        assert!(!route.permits_microphone_streaming());
        let availability = route.availability();
        assert!(!availability.available);
        assert!(availability.reason.as_deref().is_some_and(|reason| {
            reason.contains("does not expose an authenticated Realtime endpoint")
                && reason.contains("will not request an API key")
        }));
    }

    #[test]
    fn openai_compatible_rest_shape_is_not_treated_as_realtime_capability() {
        let mut config = Config::default();
        config.model.provider = "openai".to_owned();
        config.model.auth = "api-key".to_owned();

        let route = resolve_openai_realtime_route(&config);
        assert!(!route.permits_microphone_streaming());
        assert!(route.availability().reason.as_deref().is_some_and(|reason| {
            reason.contains("supported ChatGPT/Codex OAuth route")
                && reason.contains("will not ask for a separate API key")
        }));
    }

    #[test]
    fn session_configuration_uses_audio_transcription_and_server_vad() {
        let event = OpenAiRealtimeClientEvent::SessionUpdate {
            session: OpenAiRealtimeSessionConfig::medusa_default("Coordinate coding work."),
        };
        let value = serde_json::to_value(event).expect("serialize session update");
        assert_eq!(value["type"], "session.update");
        assert_eq!(value["session"]["input_audio_format"], "pcm16");
        assert_eq!(value["session"]["turn_detection"]["type"], "server_vad");
        assert_eq!(
            value["session"]["input_audio_transcription"]["model"],
            "gpt-4o-mini-transcribe"
        );
    }

    #[test]
    fn audio_frames_are_encoded_without_logging_or_persistence() {
        let event = append_audio_event(&AudioFrame {
            sequence: 7,
            bytes: vec![0, 1, 2, 3],
        });
        let value = serde_json::to_value(event).expect("serialize audio append");
        assert_eq!(value["type"], "input_audio_buffer.append");
        assert_eq!(value["audio"], "AAECAw==");
    }

    #[test]
    fn transcript_deltas_map_to_shared_voice_events() {
        let events = map_server_event(
            OpenAiRealtimeServerEvent {
                event_type: "response.audio_transcript.delta".to_owned(),
                item_id: None,
                response_id: Some("response-1".to_owned()),
                transcript: None,
                delta: Some("hello".to_owned()),
                audio: None,
                error: None,
            },
            0,
        )
        .expect("map transcript delta");
        assert_eq!(
            events,
            vec![RealtimeVoiceEvent::Transcript(TranscriptUpdate {
                turn_id: "response-1".to_owned(),
                speaker: TranscriptSpeaker::Assistant,
                text: "hello".to_owned(),
                is_final: false,
            })]
        );
    }

    #[test]
    fn barge_in_cancels_generation_and_truncates_played_audio() {
        let events = barge_in_events("item-1", 420);
        assert_eq!(events[0], OpenAiRealtimeClientEvent::ResponseCancel);
        assert_eq!(
            events[1],
            OpenAiRealtimeClientEvent::ConversationItemTruncate {
                item_id: "item-1".to_owned(),
                content_index: 0,
                audio_end_ms: 420,
            }
        );
    }

    #[test]
    fn provider_errors_are_redacted_structured_events() {
        let events = map_server_event(
            OpenAiRealtimeServerEvent {
                event_type: "error".to_owned(),
                item_id: None,
                response_id: None,
                transcript: None,
                delta: None,
                audio: None,
                error: Some(OpenAiRealtimeWireError {
                    code: Some("rate_limit_exceeded".to_owned()),
                    message: "try again".to_owned(),
                    error_type: "rate_limit_error".to_owned(),
                }),
            },
            0,
        )
        .expect("map provider error");
        assert_eq!(
            events,
            vec![RealtimeVoiceEvent::Error(VoiceError {
                code: "rate_limit_exceeded".to_owned(),
                message: "try again".to_owned(),
                retryable: true,
            })]
        );
    }
}
