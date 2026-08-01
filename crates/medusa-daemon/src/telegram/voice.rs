//! Telegram voice-note transcription and native OGG/Opus reply synthesis.

use std::{fmt, io::Read, time::Duration};

use reqwest::{
    StatusCode,
    blocking::{Client, Response},
    header::{AUTHORIZATION, CONTENT_TYPE},
};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_INPUT_AUDIO_BYTES: usize = 20 * 1024 * 1024;
const MAX_OUTPUT_AUDIO_BYTES: u64 = 48 * 1024 * 1024;
const MAX_TRANSCRIPT_CHARS: usize = 32_000;
const MAX_SPEECH_INPUT_CHARS: usize = 32_000;
const MAX_MODEL_CHARS: usize = 160;
const MAX_VOICE_CHARS: usize = 80;
const MAX_FILE_NAME_CHARS: usize = 240;

#[derive(Clone, Eq, PartialEq)]
pub struct OpenAiAudioToken(String);

impl OpenAiAudioToken {
    pub fn new(value: impl Into<String>) -> Result<Self, TelegramVoiceError> {
        let value = value.into();
        if value.trim() != value
            || value.len() < 16
            || value.len() > 4_096
            || value.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(TelegramVoiceError::InvalidCredential);
        }
        Ok(Self(value))
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OpenAiAudioToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpenAiAudioToken([REDACTED])")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelegramVoiceInput {
    pub file_name: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelegramSynthesizedVoice {
    pub file_name: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone)]
pub struct TelegramVoicePipeline {
    client: Client,
    token: OpenAiAudioToken,
    api_base: String,
    transcription_model: String,
    tts_model: String,
    voice: String,
}

impl fmt::Debug for TelegramVoicePipeline {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TelegramVoicePipeline")
            .field("token", &self.token)
            .field("api_base", &self.api_base)
            .field("transcription_model", &self.transcription_model)
            .field("tts_model", &self.tts_model)
            .field("voice", &self.voice)
            .finish_non_exhaustive()
    }
}

impl TelegramVoicePipeline {
    pub fn new(
        token: OpenAiAudioToken,
        api_base: impl Into<String>,
        transcription_model: impl Into<String>,
        tts_model: impl Into<String>,
        voice: impl Into<String>,
    ) -> Result<Self, TelegramVoiceError> {
        let api_base = api_base.into();
        validate_api_base(&api_base)?;
        let transcription_model = validate_identifier(transcription_model.into())?;
        let tts_model = validate_identifier(tts_model.into())?;
        let voice = validate_voice(voice.into())?;
        let client = Client::builder()
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .user_agent(concat!(
                "medusa/",
                env!("CARGO_PKG_VERSION"),
                " telegram-voice"
            ))
            .build()
            .map_err(|_| TelegramVoiceError::ClientSetup)?;
        Ok(Self {
            client,
            token,
            api_base: api_base.trim_end_matches('/').to_owned(),
            transcription_model,
            tts_model,
            voice,
        })
    }

    pub fn transcribe(&self, input: &TelegramVoiceInput) -> Result<String, TelegramVoiceError> {
        validate_input(input)?;
        let boundary = format!("medusa-voice-{}", Ulid::new());
        let mut body = Vec::with_capacity(input.bytes.len().saturating_add(1_024));
        push_text_part(&mut body, &boundary, "model", &self.transcription_model);
        push_file_part(&mut body, &boundary, "file", input);
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        let response = self
            .client
            .post(format!("{}/v1/audio/transcriptions", self.api_base))
            .header(AUTHORIZATION, format!("Bearer {}", self.token.expose()))
            .header(
                CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .map_err(classify_transport)?;
        let status = response.status();
        if !status.is_success() {
            return Err(classify_status(status));
        }
        let payload: TranscriptionResponse = response
            .json()
            .map_err(|_| TelegramVoiceError::MalformedResponse)?;
        let transcript = payload.text.trim();
        if transcript.is_empty() || transcript.chars().count() > MAX_TRANSCRIPT_CHARS {
            return Err(TelegramVoiceError::InvalidTranscript);
        }
        Ok(transcript.to_owned())
    }

    pub fn synthesize(&self, text: &str) -> Result<TelegramSynthesizedVoice, TelegramVoiceError> {
        let text = text.trim();
        if text.is_empty() || text.chars().count() > MAX_SPEECH_INPUT_CHARS {
            return Err(TelegramVoiceError::InvalidSpeechInput);
        }
        let response = self
            .client
            .post(format!("{}/v1/audio/speech", self.api_base))
            .header(AUTHORIZATION, format!("Bearer {}", self.token.expose()))
            .json(&SpeechRequest {
                model: &self.tts_model,
                voice: &self.voice,
                input: text,
                response_format: "opus",
            })
            .send()
            .map_err(classify_transport)?;
        let status = response.status();
        if !status.is_success() {
            return Err(classify_status(status));
        }
        let bytes = read_bounded(response, MAX_OUTPUT_AUDIO_BYTES)?;
        if !bytes.starts_with(b"OggS") {
            return Err(TelegramVoiceError::InvalidOggOpus);
        }
        Ok(TelegramSynthesizedVoice {
            file_name: format!("medusa-{}.ogg", Ulid::new()),
            mime_type: "audio/ogg".to_owned(),
            bytes,
        })
    }
}

fn validate_api_base(value: &str) -> Result<(), TelegramVoiceError> {
    let parsed = reqwest::Url::parse(value).map_err(|_| TelegramVoiceError::InvalidApiBase)?;
    let loopback = parsed.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if parsed.host_str().is_none()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || (!loopback && parsed.scheme() != "https")
        || (loopback && !matches!(parsed.scheme(), "http" | "https"))
    {
        return Err(TelegramVoiceError::InvalidApiBase);
    }
    Ok(())
}

fn validate_identifier(value: String) -> Result<String, TelegramVoiceError> {
    let trimmed = value.trim();
    if trimmed != value
        || trimmed.is_empty()
        || trimmed.len() > MAX_MODEL_CHARS
        || !trimmed.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
    {
        return Err(TelegramVoiceError::InvalidModel);
    }
    Ok(value)
}

fn validate_voice(value: String) -> Result<String, TelegramVoiceError> {
    let trimmed = value.trim();
    if trimmed != value
        || trimmed.is_empty()
        || trimmed.len() > MAX_VOICE_CHARS
        || !trimmed
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(TelegramVoiceError::InvalidVoice);
    }
    Ok(value)
}

fn validate_input(input: &TelegramVoiceInput) -> Result<(), TelegramVoiceError> {
    if input.bytes.is_empty() || input.bytes.len() > MAX_INPUT_AUDIO_BYTES {
        return Err(TelegramVoiceError::InvalidInputAudio);
    }
    if input.file_name.trim().is_empty()
        || input.file_name.chars().count() > MAX_FILE_NAME_CHARS
        || input
            .file_name
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\' | '"'))
        || !matches!(
            input.mime_type.as_str(),
            "audio/ogg" | "audio/opus" | "audio/mpeg" | "audio/mp4" | "audio/wav" | "audio/webm"
        )
    {
        return Err(TelegramVoiceError::InvalidInputAudio);
    }
    Ok(())
}

fn push_text_part(body: &mut Vec<u8>, boundary: &str, name: &str, value: &str) {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(value.as_bytes());
    body.extend_from_slice(b"\r\n");
}

fn push_file_part(body: &mut Vec<u8>, boundary: &str, name: &str, input: &TelegramVoiceInput) {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"{name}\"; filename=\"{}\"\r\n",
            input.file_name
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {}\r\n\r\n", input.mime_type).as_bytes());
    body.extend_from_slice(&input.bytes);
    body.extend_from_slice(b"\r\n");
}

fn read_bounded(response: Response, limit: u64) -> Result<Vec<u8>, TelegramVoiceError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(TelegramVoiceError::ResponseTooLarge);
    }
    let mut bytes = Vec::new();
    response
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| TelegramVoiceError::Transport)?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > limit) {
        return Err(TelegramVoiceError::ResponseTooLarge);
    }
    Ok(bytes)
}

fn classify_transport(_: reqwest::Error) -> TelegramVoiceError {
    TelegramVoiceError::Transport
}

fn classify_status(status: StatusCode) -> TelegramVoiceError {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => TelegramVoiceError::Authentication,
        StatusCode::TOO_MANY_REQUESTS => TelegramVoiceError::RateLimited,
        status if status.is_server_error() => TelegramVoiceError::ProviderUnavailable,
        _ => TelegramVoiceError::Rejected,
    }
}

#[derive(Deserialize)]
struct TranscriptionResponse {
    text: String,
}

#[derive(Serialize)]
struct SpeechRequest<'a> {
    model: &'a str,
    voice: &'a str,
    input: &'a str,
    response_format: &'static str,
}

#[derive(Debug, thiserror::Error)]
pub enum TelegramVoiceError {
    #[error("OpenAI audio credential is invalid")]
    InvalidCredential,
    #[error("OpenAI audio API base is invalid")]
    InvalidApiBase,
    #[error("OpenAI audio model is invalid")]
    InvalidModel,
    #[error("OpenAI audio voice is invalid")]
    InvalidVoice,
    #[error("Telegram input audio is invalid or exceeds the supported size")]
    InvalidInputAudio,
    #[error("voice transcript is empty or exceeds the supported size")]
    InvalidTranscript,
    #[error("speech input is empty or exceeds the supported size")]
    InvalidSpeechInput,
    #[error("speech response is not a valid OGG/Opus stream")]
    InvalidOggOpus,
    #[error("OpenAI audio client setup failed")]
    ClientSetup,
    #[error("OpenAI audio request failed")]
    Transport,
    #[error("OpenAI audio authentication failed")]
    Authentication,
    #[error("OpenAI audio request was rate limited")]
    RateLimited,
    #[error("OpenAI audio provider is unavailable")]
    ProviderUnavailable,
    #[error("OpenAI audio request was rejected")]
    Rejected,
    #[error("OpenAI audio response was malformed")]
    MalformedResponse,
    #[error("OpenAI audio response exceeds the supported size")]
    ResponseTooLarge,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_redacted() {
        let token = OpenAiAudioToken::new("sk-test-1234567890abcdef").expect("token");
        assert_eq!(format!("{token:?}"), "OpenAiAudioToken([REDACTED])");
    }

    #[test]
    fn api_base_requires_https_except_loopback() {
        assert!(validate_api_base("https://api.openai.com").is_ok());
        assert!(validate_api_base("http://127.0.0.1:8080").is_ok());
        assert!(validate_api_base("http://example.test").is_err());
    }

    #[test]
    fn voice_input_is_bounded_and_typed() {
        assert!(
            validate_input(&TelegramVoiceInput {
                file_name: "voice.ogg".to_owned(),
                mime_type: "audio/ogg".to_owned(),
                bytes: b"OggS".to_vec(),
            })
            .is_ok()
        );
        assert!(
            validate_input(&TelegramVoiceInput {
                file_name: "voice.exe".to_owned(),
                mime_type: "application/octet-stream".to_owned(),
                bytes: vec![1],
            })
            .is_err()
        );
    }
}
