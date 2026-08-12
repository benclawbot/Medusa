//! OpenAI Realtime route discovery, session credential minting, and protocol mapping.
//!
//! Medusa reuses the local Codex/ChatGPT authentication state. A ChatGPT login may
//! contain an automatically provisioned API credential; when present, that
//! credential is read only by the trusted runtime and exchanged for a short-lived
//! Realtime client secret. Long-lived credentials are never serialized, returned
//! to frontends, or included in debug output. Accounts without an API credential
//! remain fail-closed before microphone activation.

use std::{
    env, fmt, fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use medusa_config::Config;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::voice::{
    AudioFrame, RealtimeVoiceEvent, TranscriptSpeaker, TranscriptUpdate, VoiceAvailability,
    VoiceError,
};

pub const OPENAI_OAUTH_PROVIDER: &str = "openai-oauth";
pub const OPENAI_REALTIME_MODEL: &str = "gpt-realtime";
pub const OPENAI_API_BASE_URL: &str = "https://api.openai.com/v1";
const CLIENT_SECRET_PATH: &str = "realtime/client_secrets";
const CLIENT_SECRET_TTL_SECONDS: u64 = 60;
const MIN_CLIENT_SECRET_REMAINING_SECONDS: u64 = 10;
const MAX_CLIENT_SECRET_TTL_SECONDS: u64 = 7_200;
const DEFAULT_AUTH_FILE: &str = "auth.json";

pub const OPENAI_OAUTH_REALTIME_REASON: &str = "ChatGPT/Codex authentication is configured, but this login did not provision an OpenAI API credential that can mint a Realtime session. Voice remains unavailable before microphone capture; Medusa will not request or store a separate voice API key.";

#[derive(Clone, Eq, PartialEq)]
pub enum OpenAiRealtimeRoute {
    /// A ChatGPT login whose trusted Codex auth file contains an automatically
    /// provisioned API credential. The credential remains in that file and is
    /// read again only while minting a short-lived client secret.
    OAuthApi {
        api_base_url: String,
        model: String,
        auth_file: PathBuf,
    },
    OAuthUnavailable {
        reason: String,
    },
    ExistingRouteUnsupported {
        provider: String,
    },
}

impl fmt::Debug for OpenAiRealtimeRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OAuthApi {
                api_base_url,
                model,
                auth_file,
            } => formatter
                .debug_struct("OAuthApi")
                .field("api_base_url", api_base_url)
                .field("model", model)
                .field("auth_file", auth_file)
                .finish(),
            Self::OAuthUnavailable { reason } => formatter
                .debug_struct("OAuthUnavailable")
                .field("reason", reason)
                .finish(),
            Self::ExistingRouteUnsupported { provider } => formatter
                .debug_struct("ExistingRouteUnsupported")
                .field("provider", provider)
                .finish(),
        }
    }
}

impl OpenAiRealtimeRoute {
    #[must_use]
    pub fn availability(&self) -> VoiceAvailability {
        match self {
            Self::OAuthApi { .. } => VoiceAvailability {
                available: true,
                reason: None,
                supports_input_audio: true,
                supports_output_audio: true,
                supports_barge_in: true,
            },
            Self::OAuthUnavailable { reason } => VoiceAvailability::unavailable(reason.clone()),
            Self::ExistingRouteUnsupported { provider } => VoiceAvailability::unavailable(format!(
                "Realtime voice is unavailable for configured provider `{provider}`. Voice requires the existing ChatGPT/Codex authentication route; Medusa will not ask for a separate voice API key."
            )),
        }
    }

    /// A discovered route alone never authorizes microphone capture. The caller
    /// must first mint and validate a short-lived session credential.
    #[must_use]
    pub fn permits_microphone_streaming(&self) -> bool {
        false
    }

    pub fn establish_session(
        &self,
    ) -> Result<OpenAiRealtimeSessionCredential, OpenAiRealtimeEstablishError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| OpenAiRealtimeEstablishError::Transport(error.to_string()))?;
        self.establish_session_with(&client, unix_time_seconds()?)
    }

    pub fn establish_session_with(
        &self,
        client: &Client,
        now_seconds: u64,
    ) -> Result<OpenAiRealtimeSessionCredential, OpenAiRealtimeEstablishError> {
        let Self::OAuthApi {
            api_base_url,
            model,
            auth_file,
        } = self
        else {
            return Err(OpenAiRealtimeEstablishError::Unavailable(
                self.availability()
                    .reason
                    .unwrap_or_else(|| "Realtime route is unavailable".to_owned()),
            ));
        };

        let api_key = load_chatgpt_api_key(auth_file)?;
        let endpoint = format!(
            "{}/{CLIENT_SECRET_PATH}",
            api_base_url.trim_end_matches('/')
        );
        let response = client
            .post(endpoint)
            .bearer_auth(&api_key)
            .json(&client_secret_request(model))
            .send()
            .map_err(|error| OpenAiRealtimeEstablishError::Transport(error.to_string()))?;
        drop(api_key);

        let status = response.status();
        let request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        if !status.is_success() {
            let detail = request_id
                .as_deref()
                .map_or_else(String::new, |id| format!(" (request {id})"));
            return match status.as_u16() {
                401 | 403 => Err(OpenAiRealtimeEstablishError::Authentication(format!(
                    "the API credential associated with the ChatGPT login was rejected{detail}; sign in again and verify API organization access"
                ))),
                404 => Err(OpenAiRealtimeEstablishError::Unavailable(format!(
                    "the authenticated OpenAI route does not expose Realtime client-secret minting{detail}"
                ))),
                429 => Err(OpenAiRealtimeEstablishError::RateLimited(format!(
                    "Realtime session minting was rate limited{detail}"
                ))),
                _ => Err(OpenAiRealtimeEstablishError::Service(format!(
                    "Realtime session minting returned HTTP {status}{detail}"
                ))),
            };
        }

        let payload = response
            .json::<ClientSecretResponse>()
            .map_err(|error| OpenAiRealtimeEstablishError::Protocol(error.to_string()))?;
        let (client_secret, expires_at) = payload.into_secret()?;
        if client_secret.trim().len() < 16 {
            return Err(OpenAiRealtimeEstablishError::Protocol(
                "Realtime client-secret response contained an invalid credential".to_owned(),
            ));
        }
        if expires_at <= now_seconds.saturating_add(MIN_CLIENT_SECRET_REMAINING_SECONDS) {
            return Err(OpenAiRealtimeEstablishError::Protocol(
                "Realtime client secret was already expired or too close to expiry".to_owned(),
            ));
        }
        if expires_at > now_seconds.saturating_add(MAX_CLIENT_SECRET_TTL_SECONDS + 60) {
            return Err(OpenAiRealtimeEstablishError::Protocol(
                "Realtime client secret exceeded the bounded lifetime".to_owned(),
            ));
        }

        Ok(OpenAiRealtimeSessionCredential {
            client_secret,
            expires_at,
            model: model.clone(),
            websocket_url: realtime_websocket_url(api_base_url, model),
            webrtc_call_url: format!("{}/realtime/calls", api_base_url.trim_end_matches('/')),
        })
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum OpenAiRealtimeEstablishError {
    #[error("Realtime unavailable: {0}")]
    Unavailable(String),
    #[error("Realtime credential unavailable: {0}")]
    Credential(String),
    #[error("Realtime authentication failed: {0}")]
    Authentication(String),
    #[error("Realtime request failed: {0}")]
    Transport(String),
    #[error("Realtime service failed: {0}")]
    Service(String),
    #[error("Realtime rate limited: {0}")]
    RateLimited(String),
    #[error("Realtime protocol error: {0}")]
    Protocol(String),
    #[error("system clock unavailable: {0}")]
    Clock(String),
}

pub struct OpenAiRealtimeSessionCredential {
    client_secret: String,
    expires_at: u64,
    model: String,
    websocket_url: String,
    webrtc_call_url: String,
}

impl fmt::Debug for OpenAiRealtimeSessionCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiRealtimeSessionCredential")
            .field("client_secret", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .field("model", &self.model)
            .field("websocket_url", &self.websocket_url)
            .field("webrtc_call_url", &self.webrtc_call_url)
            .finish()
    }
}

impl OpenAiRealtimeSessionCredential {
    /// Returns the short-lived bearer credential for the selected transport.
    /// It must never be persisted or logged by the caller.
    #[must_use]
    pub fn authorization_token(&self) -> &str {
        &self.client_secret
    }

    #[must_use]
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    #[must_use]
    pub fn websocket_url(&self) -> &str {
        &self.websocket_url
    }

    #[must_use]
    pub fn webrtc_call_url(&self) -> &str {
        &self.webrtc_call_url
    }

    #[must_use]
    pub fn permits_microphone_streaming_at(&self, now_seconds: u64) -> bool {
        !self.client_secret.is_empty()
            && self.expires_at > now_seconds.saturating_add(MIN_CLIENT_SECRET_REMAINING_SECONDS)
    }
}

/// Decide voice availability from the same account configuration used by normal
/// Medusa requests. An OpenAI-compatible REST shape is never treated as proof of
/// Realtime capability.
#[must_use]
pub fn resolve_openai_realtime_route(config: &Config) -> OpenAiRealtimeRoute {
    if !config
        .model
        .provider
        .trim()
        .eq_ignore_ascii_case(OPENAI_OAUTH_PROVIDER)
    {
        return OpenAiRealtimeRoute::ExistingRouteUnsupported {
            provider: config.model.provider.clone(),
        };
    }

    let auth_file = match codex_auth_path() {
        Ok(path) => path,
        Err(reason) => return OpenAiRealtimeRoute::OAuthUnavailable { reason },
    };
    resolve_openai_realtime_route_with_auth_file(config, &auth_file, OPENAI_API_BASE_URL)
}

#[must_use]
pub fn resolve_openai_realtime_route_with_auth_file(
    config: &Config,
    auth_file: &Path,
    api_base_url: &str,
) -> OpenAiRealtimeRoute {
    if !config
        .model
        .provider
        .trim()
        .eq_ignore_ascii_case(OPENAI_OAUTH_PROVIDER)
    {
        return OpenAiRealtimeRoute::ExistingRouteUnsupported {
            provider: config.model.provider.clone(),
        };
    }
    match inspect_chatgpt_auth(auth_file) {
        Ok(()) => OpenAiRealtimeRoute::OAuthApi {
            api_base_url: api_base_url.trim_end_matches('/').to_owned(),
            model: OPENAI_REALTIME_MODEL.to_owned(),
            auth_file: auth_file.to_path_buf(),
        },
        Err(reason) => OpenAiRealtimeRoute::OAuthUnavailable { reason },
    }
}

pub fn establish_openai_realtime_session(
    config: &Config,
) -> Result<OpenAiRealtimeSessionCredential, OpenAiRealtimeEstablishError> {
    resolve_openai_realtime_route(config).establish_session()
}

#[derive(Deserialize)]
struct CodexAuthFile {
    #[serde(default)]
    auth_mode: Option<String>,
    #[serde(default, rename = "OPENAI_API_KEY")]
    openai_api_key: Option<String>,
}

fn codex_auth_path() -> Result<PathBuf, String> {
    if let Some(home) = env::var_os("CODEX_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home).join(DEFAULT_AUTH_FILE));
    }
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "cannot locate the existing Codex authentication state; set CODEX_HOME or sign in with Codex on this account".to_owned()
        })?;
    Ok(PathBuf::from(home).join(".codex").join(DEFAULT_AUTH_FILE))
}

fn inspect_chatgpt_auth(path: &Path) -> Result<(), String> {
    load_chatgpt_api_key(path)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn load_chatgpt_api_key(path: &Path) -> Result<String, OpenAiRealtimeEstablishError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        OpenAiRealtimeEstablishError::Credential(format!(
            "cannot read existing Codex authentication at {}: {error}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(OpenAiRealtimeEstablishError::Credential(format!(
            "Codex authentication path is not a regular non-symlink file: {}",
            path.display()
        )));
    }
    validate_auth_file_permissions(path, &metadata)?;
    let text = fs::read_to_string(path).map_err(|error| {
        OpenAiRealtimeEstablishError::Credential(format!(
            "cannot read existing Codex authentication at {}: {error}",
            path.display()
        ))
    })?;
    let auth = serde_json::from_str::<CodexAuthFile>(&text).map_err(|error| {
        OpenAiRealtimeEstablishError::Credential(format!(
            "existing Codex authentication is malformed: {error}"
        ))
    })?;
    if !auth
        .auth_mode
        .as_deref()
        .is_some_and(|mode| mode.eq_ignore_ascii_case("chatgpt"))
    {
        return Err(OpenAiRealtimeEstablishError::Credential(
            "the existing Codex authentication is not a ChatGPT login".to_owned(),
        ));
    }
    let key = auth
        .openai_api_key
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            OpenAiRealtimeEstablishError::Unavailable(OPENAI_OAUTH_REALTIME_REASON.to_owned())
        })?;
    if key.len() > 8_192 {
        return Err(OpenAiRealtimeEstablishError::Credential(
            "the provisioned API credential is invalid".to_owned(),
        ));
    }
    Ok(key)
}

#[cfg(unix)]
fn validate_auth_file_permissions(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), OpenAiRealtimeEstablishError> {
    use std::os::unix::fs::PermissionsExt as _;

    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(OpenAiRealtimeEstablishError::Credential(format!(
            "refusing insecure Codex authentication permissions on {}; restrict the file to the current user",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_auth_file_permissions(
    _path: &Path,
    _metadata: &fs::Metadata,
) -> Result<(), OpenAiRealtimeEstablishError> {
    Ok(())
}

fn unix_time_seconds() -> Result<u64, OpenAiRealtimeEstablishError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| OpenAiRealtimeEstablishError::Clock(error.to_string()))
}

fn client_secret_request(model: &str) -> Value {
    json!({
        "expires_after": {
            "anchor": "created_at",
            "seconds": CLIENT_SECRET_TTL_SECONDS
        },
        "session": OpenAiRealtimeSessionConfig::for_model(
            model,
            "Act as Medusa's realtime coding supervisor. Keep spoken updates concise and preserve Medusa's tool, approval, containment, cancellation, and verification policies."
        )
    })
}

fn realtime_websocket_url(api_base_url: &str, model: &str) -> String {
    let base = api_base_url.trim_end_matches('/');
    let websocket_base = base
        .strip_prefix("https://")
        .map(|rest| format!("wss://{rest}"))
        .or_else(|| {
            base.strip_prefix("http://")
                .map(|rest| format!("ws://{rest}"))
        })
        .unwrap_or_else(|| base.to_owned());
    format!("{websocket_base}/realtime?model={model}")
}

#[derive(Deserialize)]
struct ClientSecretResponse {
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    expires_at: Option<u64>,
    #[serde(default)]
    client_secret: Option<LegacyClientSecret>,
    #[serde(default)]
    session: Option<Value>,
}

impl ClientSecretResponse {
    fn into_secret(self) -> Result<(String, u64), OpenAiRealtimeEstablishError> {
        let Self {
            value,
            expires_at,
            client_secret,
            session,
        } = self;
        if let (Some(value), Some(expires_at)) = (value, expires_at) {
            if session
                .as_ref()
                .and_then(|session| session.get("type"))
                .and_then(Value::as_str)
                .is_some_and(|session_type| session_type != "realtime")
            {
                return Err(OpenAiRealtimeEstablishError::Protocol(
                    "client-secret response was not for a Realtime session".to_owned(),
                ));
            }
            return Ok((value, expires_at));
        }
        client_secret
            .map(|secret| (secret.value, secret.expires_at))
            .ok_or_else(|| {
                OpenAiRealtimeEstablishError::Protocol(
                    "client-secret response omitted the short-lived credential".to_owned(),
                )
            })
    }
}

#[derive(Deserialize)]
struct LegacyClientSecret {
    value: String,
    expires_at: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum OpenAiRealtimeClientEvent {
    #[serde(rename = "session.update")]
    SessionUpdate {
        session: Box<OpenAiRealtimeSessionConfig>,
    },
    #[serde(rename = "input_audio_buffer.append")]
    InputAudioAppend { audio: String },
    #[serde(rename = "input_audio_buffer.commit")]
    InputAudioCommit,
    #[serde(rename = "response.create")]
    ResponseCreate,
    #[serde(rename = "response.cancel")]
    ResponseCancel,
    #[serde(rename = "conversation.item.truncate")]
    ConversationItemTruncate {
        item_id: String,
        content_index: u32,
        audio_end_ms: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OpenAiRealtimeSessionConfig {
    #[serde(rename = "type")]
    pub session_type: String,
    pub model: String,
    pub instructions: String,
    pub output_modalities: Vec<String>,
    pub max_output_tokens: u32,
    pub audio: OpenAiRealtimeAudioConfig,
}

impl OpenAiRealtimeSessionConfig {
    #[must_use]
    pub fn medusa_default(instructions: impl Into<String>) -> Self {
        Self::for_model(OPENAI_REALTIME_MODEL, instructions)
    }

    #[must_use]
    pub fn for_model(model: impl Into<String>, instructions: impl Into<String>) -> Self {
        Self {
            session_type: "realtime".to_owned(),
            model: model.into(),
            instructions: instructions.into(),
            output_modalities: vec!["audio".to_owned()],
            max_output_tokens: 4_096,
            audio: OpenAiRealtimeAudioConfig {
                input: OpenAiRealtimeInputAudioConfig {
                    format: OpenAiRealtimePcmFormat::default(),
                    transcription: OpenAiRealtimeTranscriptionConfig {
                        model: "gpt-4o-mini-transcribe".to_owned(),
                    },
                    noise_reduction: OpenAiRealtimeNoiseReduction {
                        reduction_type: "near_field".to_owned(),
                    },
                    turn_detection: OpenAiRealtimeTurnDetection {
                        detection_type: "server_vad".to_owned(),
                        threshold: 0.5,
                        prefix_padding_ms: 300,
                        silence_duration_ms: 500,
                        create_response: true,
                        interrupt_response: true,
                    },
                },
                output: OpenAiRealtimeOutputAudioConfig {
                    format: OpenAiRealtimePcmFormat::default(),
                    voice: "alloy".to_owned(),
                    speed: 1.0,
                },
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OpenAiRealtimeAudioConfig {
    pub input: OpenAiRealtimeInputAudioConfig,
    pub output: OpenAiRealtimeOutputAudioConfig,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OpenAiRealtimeInputAudioConfig {
    pub format: OpenAiRealtimePcmFormat,
    pub transcription: OpenAiRealtimeTranscriptionConfig,
    pub noise_reduction: OpenAiRealtimeNoiseReduction,
    pub turn_detection: OpenAiRealtimeTurnDetection,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OpenAiRealtimeOutputAudioConfig {
    pub format: OpenAiRealtimePcmFormat,
    pub voice: String,
    pub speed: f64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OpenAiRealtimePcmFormat {
    #[serde(rename = "type")]
    pub format_type: String,
    pub rate: u32,
}

impl Default for OpenAiRealtimePcmFormat {
    fn default() -> Self {
        Self {
            format_type: "audio/pcm".to_owned(),
            rate: 24_000,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OpenAiRealtimeTranscriptionConfig {
    pub model: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OpenAiRealtimeNoiseReduction {
    #[serde(rename = "type")]
    pub reduction_type: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OpenAiRealtimeTurnDetection {
    #[serde(rename = "type")]
    pub detection_type: String,
    pub threshold: f64,
    pub prefix_padding_ms: u32,
    pub silence_duration_ms: u32,
    pub create_response: bool,
    pub interrupt_response: bool,
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
    pub response: Option<Value>,
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

/// Convert provider events into the shared voice model. Both current and legacy
/// Realtime event names are accepted during the provider migration window.
pub fn map_server_event(
    event: OpenAiRealtimeServerEvent,
    sequence: u64,
) -> Result<Vec<RealtimeVoiceEvent>, String> {
    let response_id = event.response_id.clone().or_else(|| {
        event
            .response
            .as_ref()
            .and_then(|response| response.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
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
        "conversation.item.input_audio_transcription.delta" => {
            vec![RealtimeVoiceEvent::Transcript(TranscriptUpdate {
                turn_id: event.item_id.unwrap_or_else(|| "user-audio".to_owned()),
                speaker: TranscriptSpeaker::User,
                text: event.delta.unwrap_or_default(),
                is_final: false,
            })]
        }
        "conversation.item.input_audio_transcription.completed" => {
            vec![RealtimeVoiceEvent::Transcript(TranscriptUpdate {
                turn_id: event.item_id.unwrap_or_else(|| "user-audio".to_owned()),
                speaker: TranscriptSpeaker::User,
                text: event.transcript.unwrap_or_default(),
                is_final: true,
            })]
        }
        "response.output_audio_transcript.delta" | "response.audio_transcript.delta" => {
            vec![RealtimeVoiceEvent::Transcript(TranscriptUpdate {
                turn_id: response_id.unwrap_or_else(|| "assistant-audio".to_owned()),
                speaker: TranscriptSpeaker::Assistant,
                text: event.delta.unwrap_or_default(),
                is_final: false,
            })]
        }
        "response.output_audio_transcript.done" | "response.audio_transcript.done" => {
            vec![RealtimeVoiceEvent::Transcript(TranscriptUpdate {
                turn_id: response_id.unwrap_or_else(|| "assistant-audio".to_owned()),
                speaker: TranscriptSpeaker::Assistant,
                text: event.transcript.unwrap_or_default(),
                is_final: true,
            })]
        }
        "response.output_audio.delta" | "response.audio.delta" => {
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
        "response.output_audio.done" | "response.audio.done" => {
            vec![RealtimeVoiceEvent::TransportStatus {
                connected: true,
                detail: Some("assistant audio completed".to_owned()),
            }]
        }
        "response.cancelled" | "response.canceled" => vec![RealtimeVoiceEvent::Interrupted],
        "response.done"
            if event
                .response
                .as_ref()
                .and_then(|response| response.get("status"))
                .and_then(Value::as_str)
                .is_some_and(|status| matches!(status, "cancelled" | "canceled")) =>
        {
            vec![RealtimeVoiceEvent::Interrupted]
        }
        "response.done" => vec![RealtimeVoiceEvent::TransportStatus {
            connected: true,
            detail: Some("assistant response completed".to_owned()),
        }],
        "rate_limits.updated" => vec![RealtimeVoiceEvent::Error(VoiceError {
            code: "rate_limits_updated".to_owned(),
            message: "OpenAI Realtime rate limits were updated".to_owned(),
            retryable: true,
        })],
        "error" => {
            let error = event.error.ok_or_else(|| {
                "OpenAI Realtime error event omitted structured error details".to_owned()
            })?;
            let retryable = error
                .code
                .as_deref()
                .is_some_and(|code| matches!(code, "rate_limit_exceeded" | "server_error"));
            vec![RealtimeVoiceEvent::Error(VoiceError {
                code: error.code.unwrap_or(error.error_type),
                message: error.message,
                retryable,
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
pub fn session_update_event(instructions: impl Into<String>) -> OpenAiRealtimeClientEvent {
    OpenAiRealtimeClientEvent::SessionUpdate {
        session: Box::new(OpenAiRealtimeSessionConfig::medusa_default(instructions)),
    }
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
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use tempfile::TempDir;

    use super::*;

    fn oauth_config() -> Config {
        let mut config = Config::default();
        config.model.provider = OPENAI_OAUTH_PROVIDER.to_owned();
        config.model.auth = "none".to_owned();
        config.model.base_url = Some("http://127.0.0.1:10531/v1".to_owned());
        config
    }

    fn write_auth(directory: &TempDir, key: Option<&str>) -> PathBuf {
        let path = directory.path().join("auth.json");
        let document = json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": key,
            "tokens": {
                "access_token": "not-used-by-realtime",
                "refresh_token": "not-used-by-realtime"
            }
        });
        fs::write(
            &path,
            serde_json::to_vec(&document).expect("serialize auth"),
        )
        .expect("write auth");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .expect("secure auth permissions");
        }
        path
    }

    fn spawn_client_secret_server(
        expected_key: &'static str,
        secret: &'static str,
        expires_at: u64,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let address = listener.local_addr().expect("mock server address");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set timeout");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4_096];
            loop {
                let read = stream.read(&mut buffer).expect("read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&request);
                let Some(header_end) = text.find("\r\n\r\n") else {
                    continue;
                };
                let content_length = text[..header_end]
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("Content-Length: ")
                            .or_else(|| line.strip_prefix("content-length: "))
                    })
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let request = String::from_utf8(request).expect("utf8 request");
            assert!(request.starts_with("POST /v1/realtime/client_secrets HTTP/1.1"));
            assert!(request.to_ascii_lowercase().contains(&format!(
                "authorization: bearer {}",
                expected_key.to_ascii_lowercase()
            )));
            assert!(request.contains("\"type\":\"realtime\""));
            assert!(request.contains("\"model\":\"gpt-realtime\""));
            assert!(request.contains("\"threshold\":0.5"));
            assert!(request.contains("\"speed\":1.0"));
            let body = json!({
                "value": secret,
                "expires_at": expires_at,
                "session": {
                    "type": "realtime",
                    "model": "gpt-realtime"
                }
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });
        (format!("http://{address}/v1"), handle)
    }

    #[test]
    fn chatgpt_auth_mints_short_lived_session_before_microphone_streaming() {
        let directory = TempDir::new().expect("tempdir");
        let auth_file = write_auth(&directory, Some("auto-provisioned-test-key"));
        let now = 10_000;
        let (api_base_url, server) = spawn_client_secret_server(
            "auto-provisioned-test-key",
            "ek_test_short_lived_credential",
            now + 60,
        );
        let route = resolve_openai_realtime_route_with_auth_file(
            &oauth_config(),
            &auth_file,
            &api_base_url,
        );
        assert!(route.availability().available);
        assert!(!route.permits_microphone_streaming());
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client");
        let credential = route
            .establish_session_with(&client, now)
            .expect("mint credential");
        server.join().expect("mock server");
        assert!(credential.permits_microphone_streaming_at(now));
        assert_eq!(credential.model(), OPENAI_REALTIME_MODEL);
        assert_eq!(
            credential.authorization_token(),
            "ek_test_short_lived_credential"
        );
        assert!(credential.websocket_url().starts_with("ws://"));
        assert!(credential.webrtc_call_url().ends_with("/realtime/calls"));
        assert!(!format!("{credential:?}").contains("ek_test_short_lived_credential"));
    }

    #[test]
    fn pure_chatgpt_subscription_without_api_credential_fails_closed() {
        let directory = TempDir::new().expect("tempdir");
        let auth_file = write_auth(&directory, None);
        let route = resolve_openai_realtime_route_with_auth_file(
            &oauth_config(),
            &auth_file,
            OPENAI_API_BASE_URL,
        );
        assert!(!route.availability().available);
        assert!(!route.permits_microphone_streaming());
        assert!(
            route
                .availability()
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("will not request"))
        );
    }

    #[test]
    fn api_key_login_is_not_misclassified_as_chatgpt_oauth() {
        let directory = TempDir::new().expect("tempdir");
        let path = directory.path().join("auth.json");
        fs::write(
            &path,
            br#"{"auth_mode":"apikey","OPENAI_API_KEY":"direct-key"}"#,
        )
        .expect("write auth");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .expect("secure auth permissions");
        }
        let route = resolve_openai_realtime_route_with_auth_file(
            &oauth_config(),
            &path,
            OPENAI_API_BASE_URL,
        );
        assert!(!route.availability().available);
    }

    #[cfg(unix)]
    #[test]
    fn insecure_auth_file_permissions_fail_closed() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = TempDir::new().expect("tempdir");
        let auth_file = write_auth(&directory, Some("auto-provisioned-test-key"));
        fs::set_permissions(&auth_file, fs::Permissions::from_mode(0o644))
            .expect("insecure permissions");
        let route = resolve_openai_realtime_route_with_auth_file(
            &oauth_config(),
            &auth_file,
            OPENAI_API_BASE_URL,
        );
        assert!(!route.availability().available);
        assert!(
            route
                .availability()
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("insecure"))
        );
    }

    #[test]
    fn openai_compatible_rest_shape_is_not_treated_as_realtime_capability() {
        let mut config = Config::default();
        config.model.provider = "openai".to_owned();
        config.model.auth = "api-key".to_owned();

        let route = resolve_openai_realtime_route(&config);
        assert!(!route.permits_microphone_streaming());
        assert!(
            route
                .availability()
                .reason
                .as_deref()
                .is_some_and(|reason| {
                    reason.contains("existing ChatGPT/Codex authentication route")
                        && reason.contains("will not ask for a separate voice API key")
                })
        );
    }

    #[test]
    fn session_configuration_uses_current_audio_shape_and_server_vad() {
        let event = session_update_event("Coordinate coding work.");
        let value = serde_json::to_value(event).expect("serialize session update");
        assert_eq!(value["type"], "session.update");
        assert_eq!(value["session"]["type"], "realtime");
        assert_eq!(value["session"]["model"], OPENAI_REALTIME_MODEL);
        assert_eq!(
            value["session"]["audio"]["input"]["format"]["type"],
            "audio/pcm"
        );
        assert_eq!(value["session"]["audio"]["input"]["format"]["rate"], 24_000);
        assert_eq!(
            value["session"]["audio"]["input"]["turn_detection"]["type"],
            "server_vad"
        );
        assert_eq!(
            value["session"]["audio"]["input"]["turn_detection"]["threshold"],
            0.5
        );
        assert_eq!(value["session"]["audio"]["output"]["speed"], 1.0);
        assert_eq!(
            value["session"]["audio"]["input"]["transcription"]["model"],
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
    fn current_transcript_deltas_map_to_shared_voice_events() {
        let events = map_server_event(
            OpenAiRealtimeServerEvent {
                event_type: "response.output_audio_transcript.delta".to_owned(),
                item_id: None,
                response_id: None,
                response: Some(json!({"id": "response-1"})),
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
                response: None,
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

    #[test]
    #[ignore = "requires a local ChatGPT login that automatically provisioned an OpenAI API credential"]
    fn live_chatgpt_auth_mints_realtime_client_secret() {
        let route = resolve_openai_realtime_route(&oauth_config());
        let credential = route.establish_session().expect("live Realtime credential");
        let now = unix_time_seconds().expect("clock");
        assert!(credential.permits_microphone_streaming_at(now));
        assert!(!credential.authorization_token().is_empty());
    }
}
