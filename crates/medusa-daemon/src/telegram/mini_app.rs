//! Authenticated Telegram Mini App bridge for the existing Medusa Realtime voice surface.
//!
//! The bridge validates Telegram `initData`, binds an expiring launch ticket to one Telegram
//! identity and one authoritative Medusa session, mints the same short-lived WebRTC credential used
//! by the desktop, and submits final user transcripts through the shared frontend control plane.

use std::collections::BTreeMap;

use medusa_config::Config;
use medusa_protocol::frontend::{
    FRONTEND_PROTOCOL_VERSION, FrontendCommand, FrontendCommandEnvelope, FrontendKind,
};
use medusa_runtime::openai_realtime::{
    OpenAiRealtimeSessionCredential, resolve_openai_realtime_route,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use ulid::Ulid;

use super::{TelegramChatKind, TelegramIdentity};
use crate::FrontendControlPlane;

const INIT_DATA_MAX_AGE: Duration = Duration::minutes(10);
const LAUNCH_TICKET_LIFETIME: Duration = Duration::minutes(5);
const MAX_INIT_DATA_BYTES: usize = 16 * 1024;
const MAX_SESSION_ID_CHARS: usize = 160;
const MAX_TRANSCRIPT_CHARS: usize = 32_000;
const MAX_FIELD_CHARS: usize = 8_192;

#[derive(Clone, Eq, PartialEq)]
pub struct TelegramMiniAppSecret(Vec<u8>);

impl TelegramMiniAppSecret {
    pub fn from_bot_token(token: &str) -> Result<Self, TelegramMiniAppError> {
        if token.len() < 16
            || token.len() > 256
            || token.bytes().any(|byte| byte.is_ascii_control())
            || !token.contains(':')
        {
            return Err(TelegramMiniAppError::InvalidSecret);
        }
        Ok(Self(token.as_bytes().to_vec()))
    }
}

impl std::fmt::Debug for TelegramMiniAppSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TelegramMiniAppSecret([REDACTED])")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramMiniAppUser {
    pub id: i64,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedMiniAppIdentity {
    pub identity: TelegramIdentity,
    pub auth_date: OffsetDateTime,
    pub query_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelegramMiniAppBinding {
    pub identity: TelegramIdentity,
    pub session_id: String,
    pub expires_at: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramMiniAppLaunchTicket {
    pub token: String,
    pub expires_at: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct LaunchClaims {
    version: u8,
    nonce: String,
    chat_id: i64,
    topic_id: Option<i64>,
    user_id: i64,
    chat_kind: TelegramChatKind,
    session_id: String,
    expires_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramMiniAppRealtimeSession {
    pub authorization_token: String,
    pub expires_at: u64,
    pub model: String,
    pub webrtc_call_url: String,
    pub session_id: String,
}

#[derive(Clone)]
pub struct TelegramMiniAppBridge {
    secret: TelegramMiniAppSecret,
}

impl TelegramMiniAppBridge {
    #[must_use]
    pub fn new(secret: TelegramMiniAppSecret) -> Self {
        Self { secret }
    }

    pub fn verify_init_data(
        &self,
        init_data: &str,
        expected: &TelegramIdentity,
        now: OffsetDateTime,
    ) -> Result<VerifiedMiniAppIdentity, TelegramMiniAppError> {
        if init_data.is_empty() || init_data.len() > MAX_INIT_DATA_BYTES {
            return Err(TelegramMiniAppError::InvalidInitData);
        }
        let mut fields = parse_query(init_data)?;
        let supplied_hash = fields
            .remove("hash")
            .ok_or(TelegramMiniAppError::InvalidInitData)?;
        if supplied_hash.len() != 64 || !supplied_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(TelegramMiniAppError::InvalidInitData);
        }
        let data_check_string = fields
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("\n");
        let secret_key = hmac_sha256(b"WebAppData", &self.secret.0);
        let expected_hash = hex::encode(hmac_sha256(&secret_key, data_check_string.as_bytes()));
        if !constant_time_eq(
            expected_hash.as_bytes(),
            supplied_hash.to_ascii_lowercase().as_bytes(),
        ) {
            return Err(TelegramMiniAppError::InvalidSignature);
        }

        let auth_date = fields
            .get("auth_date")
            .ok_or(TelegramMiniAppError::InvalidInitData)?
            .parse::<i64>()
            .map_err(|_| TelegramMiniAppError::InvalidInitData)
            .and_then(|timestamp| {
                OffsetDateTime::from_unix_timestamp(timestamp)
                    .map_err(|_| TelegramMiniAppError::InvalidInitData)
            })?;
        let age = now - auth_date;
        if age.is_negative() || age > INIT_DATA_MAX_AGE {
            return Err(TelegramMiniAppError::ExpiredInitData);
        }
        let user: TelegramMiniAppUser = serde_json::from_str(
            fields
                .get("user")
                .ok_or(TelegramMiniAppError::InvalidInitData)?,
        )
        .map_err(|_| TelegramMiniAppError::InvalidInitData)?;
        if user.id != expected.user_id {
            return Err(TelegramMiniAppError::IdentityMismatch);
        }
        if let Some(chat_json) = fields.get("chat") {
            let chat: TelegramMiniAppChat = serde_json::from_str(chat_json)
                .map_err(|_| TelegramMiniAppError::InvalidInitData)?;
            if chat.id != expected.chat_id {
                return Err(TelegramMiniAppError::IdentityMismatch);
            }
        }
        Ok(VerifiedMiniAppIdentity {
            identity: expected.clone(),
            auth_date,
            query_id: fields.get("query_id").cloned(),
        })
    }

    pub fn issue_launch_ticket(
        &self,
        identity: &TelegramIdentity,
        session_id: &str,
        now: OffsetDateTime,
    ) -> Result<TelegramMiniAppLaunchTicket, TelegramMiniAppError> {
        validate_session_id(session_id)?;
        let claims = LaunchClaims {
            version: 1,
            nonce: Ulid::new().to_string(),
            chat_id: identity.chat_id,
            topic_id: identity.topic_id,
            user_id: identity.user_id,
            chat_kind: identity.chat_kind,
            session_id: session_id.to_owned(),
            expires_at: (now + LAUNCH_TICKET_LIFETIME).unix_timestamp(),
        };
        let payload =
            serde_json::to_vec(&claims).map_err(|_| TelegramMiniAppError::InvalidTicket)?;
        let signature = hmac_sha256(&self.secret.0, &payload);
        Ok(TelegramMiniAppLaunchTicket {
            token: format!("{}.{}", hex::encode(payload), hex::encode(signature)),
            expires_at: claims.expires_at,
        })
    }

    pub fn inspect_launch_ticket(
        &self,
        token: &str,
        now: OffsetDateTime,
    ) -> Result<TelegramMiniAppBinding, TelegramMiniAppError> {
        let (payload_hex, signature_hex) = token
            .split_once('.')
            .ok_or(TelegramMiniAppError::InvalidTicket)?;
        if payload_hex.len() > MAX_INIT_DATA_BYTES * 2
            || signature_hex.len() != 64
            || !payload_hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !signature_hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(TelegramMiniAppError::InvalidTicket);
        }
        let payload = hex::decode(payload_hex).map_err(|_| TelegramMiniAppError::InvalidTicket)?;
        let supplied =
            hex::decode(signature_hex).map_err(|_| TelegramMiniAppError::InvalidTicket)?;
        let expected_signature = hmac_sha256(&self.secret.0, &payload);
        if !constant_time_eq(&expected_signature, &supplied) {
            return Err(TelegramMiniAppError::InvalidSignature);
        }
        let claims: LaunchClaims =
            serde_json::from_slice(&payload).map_err(|_| TelegramMiniAppError::InvalidTicket)?;
        if claims.version != 1 || claims.expires_at < now.unix_timestamp() {
            return Err(TelegramMiniAppError::InvalidTicket);
        }
        validate_session_id(&claims.session_id)?;
        Ok(TelegramMiniAppBinding {
            identity: TelegramIdentity {
                chat_id: claims.chat_id,
                topic_id: claims.topic_id,
                user_id: claims.user_id,
                chat_kind: claims.chat_kind,
                bot_mentioned: false,
            },
            session_id: claims.session_id,
            expires_at: claims.expires_at,
        })
    }

    pub fn verify_launch_ticket(
        &self,
        token: &str,
        expected: &TelegramIdentity,
        now: OffsetDateTime,
    ) -> Result<String, TelegramMiniAppError> {
        let (payload_hex, signature_hex) = token
            .split_once('.')
            .ok_or(TelegramMiniAppError::InvalidTicket)?;
        if payload_hex.len() > MAX_INIT_DATA_BYTES * 2
            || signature_hex.len() != 64
            || !payload_hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !signature_hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(TelegramMiniAppError::InvalidTicket);
        }
        let payload = hex::decode(payload_hex).map_err(|_| TelegramMiniAppError::InvalidTicket)?;
        let supplied =
            hex::decode(signature_hex).map_err(|_| TelegramMiniAppError::InvalidTicket)?;
        let expected_signature = hmac_sha256(&self.secret.0, &payload);
        if !constant_time_eq(&expected_signature, &supplied) {
            return Err(TelegramMiniAppError::InvalidSignature);
        }
        let claims: LaunchClaims =
            serde_json::from_slice(&payload).map_err(|_| TelegramMiniAppError::InvalidTicket)?;
        if claims.version != 1
            || claims.chat_id != expected.chat_id
            || claims.topic_id != expected.topic_id
            || claims.user_id != expected.user_id
            || claims.chat_kind != expected.chat_kind
            || claims.expires_at < now.unix_timestamp()
        {
            return Err(TelegramMiniAppError::IdentityMismatch);
        }
        validate_session_id(&claims.session_id)?;
        Ok(claims.session_id)
    }

    pub fn establish_realtime_session(
        &self,
        ticket: &str,
        identity: &TelegramIdentity,
        config: &Config,
        now: OffsetDateTime,
    ) -> Result<TelegramMiniAppRealtimeSession, TelegramMiniAppError> {
        let session_id = self.verify_launch_ticket(ticket, identity, now)?;
        let credential = resolve_openai_realtime_route(config)
            .establish_session()
            .map_err(|error| TelegramMiniAppError::Realtime(error.to_string()))?;
        Ok(realtime_session(session_id, credential))
    }

    pub fn submit_transcript(
        &self,
        ticket: &str,
        identity: &TelegramIdentity,
        transcript: &str,
        control_plane: &mut FrontendControlPlane,
        now: OffsetDateTime,
    ) -> Result<(), TelegramMiniAppError> {
        let session_id = self.verify_launch_ticket(ticket, identity, now)?;
        let transcript = transcript.trim();
        if transcript.is_empty() || transcript.chars().count() > MAX_TRANSCRIPT_CHARS {
            return Err(TelegramMiniAppError::InvalidTranscript);
        }
        let command = FrontendCommandEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            command_id: Ulid::new().to_string(),
            idempotency_key: format!("telegram-mini-app:{}", Ulid::new()),
            frontend: FrontendKind::Telegram,
            client_id: identity.user_id.to_string(),
            session_id: Some(session_id),
            turn_id: None,
            timestamp: now,
            command: FrontendCommand::Submit {
                text: transcript.to_owned(),
                attachment_ids: Vec::new(),
            },
        };
        command
            .validate()
            .map_err(|error| TelegramMiniAppError::Protocol(error.to_owned()))?;
        control_plane
            .dispatch(command)
            .map_err(|error| TelegramMiniAppError::ControlPlane(error.to_string()))?;
        Ok(())
    }

    #[must_use]
    pub fn client_html() -> &'static str {
        MINI_APP_HTML
    }
}

fn realtime_session(
    session_id: String,
    credential: OpenAiRealtimeSessionCredential,
) -> TelegramMiniAppRealtimeSession {
    TelegramMiniAppRealtimeSession {
        authorization_token: credential.authorization_token().to_owned(),
        expires_at: credential.expires_at(),
        model: credential.model().to_owned(),
        webrtc_call_url: credential.webrtc_call_url().to_owned(),
        session_id,
    }
}

fn parse_query(input: &str) -> Result<BTreeMap<String, String>, TelegramMiniAppError> {
    let mut fields = BTreeMap::new();
    for pair in input.split('&') {
        let (key, value) = pair
            .split_once('=')
            .ok_or(TelegramMiniAppError::InvalidInitData)?;
        let key = percent_decode(key)?;
        let value = percent_decode(value)?;
        if key.is_empty()
            || key.chars().count() > 128
            || value.chars().count() > MAX_FIELD_CHARS
            || fields.insert(key, value).is_some()
        {
            return Err(TelegramMiniAppError::InvalidInitData);
        }
    }
    Ok(fields)
}

fn percent_decode(input: &str) -> Result<String, TelegramMiniAppError> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let high = hex_value(bytes[index + 1])?;
                let low = hex_value(bytes[index + 2])?;
                output.push((high << 4) | low);
                index += 3;
            }
            b'%' => return Err(TelegramMiniAppError::InvalidInitData),
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(output).map_err(|_| TelegramMiniAppError::InvalidInitData)
}

fn hex_value(byte: u8) -> Result<u8, TelegramMiniAppError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(TelegramMiniAppError::InvalidInitData),
    }
}

fn validate_session_id(session_id: &str) -> Result<(), TelegramMiniAppError> {
    if session_id.is_empty()
        || session_id.len() > MAX_SESSION_ID_CHARS
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(TelegramMiniAppError::InvalidSession);
    }
    Ok(())
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    let mut normalized = [0_u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        normalized[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut inner_key = normalized;
    let mut outer_key = normalized;
    for byte in &mut inner_key {
        *byte ^= 0x36;
    }
    for byte in &mut outer_key {
        *byte ^= 0x5c;
    }
    let mut inner = Sha256::new();
    inner.update(inner_key);
    inner.update(message);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_key);
    outer.update(inner_hash);
    outer.finalize().into()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        difference |= usize::from(
            left.get(index).copied().unwrap_or_default()
                ^ right.get(index).copied().unwrap_or_default(),
        );
    }
    difference == 0
}

#[derive(Deserialize)]
struct TelegramMiniAppChat {
    id: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum TelegramMiniAppError {
    #[error("Telegram Mini App secret is invalid")]
    InvalidSecret,
    #[error("Telegram Mini App initData is invalid")]
    InvalidInitData,
    #[error("Telegram Mini App signature is invalid")]
    InvalidSignature,
    #[error("Telegram Mini App initData has expired")]
    ExpiredInitData,
    #[error("Telegram Mini App identity does not match the bound chat")]
    IdentityMismatch,
    #[error("Telegram Mini App launch ticket is invalid")]
    InvalidTicket,
    #[error("Telegram Mini App session identifier is invalid")]
    InvalidSession,
    #[error("Telegram Mini App transcript is invalid")]
    InvalidTranscript,
    #[error("Telegram Mini App frontend protocol failed: {0}")]
    Protocol(String),
    #[error("Telegram Mini App control-plane request failed: {0}")]
    ControlPlane(String),
    #[error("Telegram Mini App Realtime session failed: {0}")]
    Realtime(String),
}

const MINI_APP_HTML: &str = r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Medusa Voice</title></head>
<body>
<main><h1>Medusa Voice</h1><p id="status">Authenticating…</p><button id="start" disabled>Start voice</button><button id="mute" disabled>Mute</button><pre id="transcript"></pre></main>
<script src="https://telegram.org/js/telegram-web-app.js"></script>
<script>
const tg = window.Telegram.WebApp; tg.ready();
const launchTicket = new URLSearchParams(window.location.search).get('ticket');
let pc, stream, muted = false, ticket;
const status = document.getElementById('status'), start = document.getElementById('start'), mute = document.getElementById('mute'), transcript = document.getElementById('transcript');
(async () => {
  if (!launchTicket) throw new Error('Missing signed launch ticket');
  const auth = await fetch('/telegram/mini-app/auth', {method:'POST', headers:{'content-type':'application/json'}, body:JSON.stringify({ticket:launchTicket, initData:tg.initData})});
  if (!auth.ok) throw new Error('Telegram authentication failed');
  ticket = (await auth.json()).token; status.textContent = 'Ready'; start.disabled = false;
})().catch(error => status.textContent = error.message);
start.onclick = async () => {
  start.disabled = true; status.textContent = 'Establishing authenticated Realtime session…';
  const response = await fetch('/telegram/mini-app/realtime', {method:'POST', headers:{'content-type':'application/json','authorization':`Bearer ${ticket}`}});
  if (!response.ok) throw new Error('Realtime unavailable');
  const session = await response.json();
  stream = await navigator.mediaDevices.getUserMedia({audio:true});
  pc = new RTCPeerConnection(); pc.addTrack(stream.getAudioTracks()[0], stream); pc.ontrack = event => { const audio = new Audio(); audio.autoplay = true; audio.srcObject = event.streams[0]; };
  const channel = pc.createDataChannel('oai-events'); channel.onmessage = async event => { const data = JSON.parse(event.data); if (data.type && data.type.includes('transcript')) transcript.textContent += data.delta || data.transcript || ''; if (data.type === 'conversation.item.input_audio_transcription.completed' && data.transcript) { await fetch('/telegram/mini-app/transcript', {method:'POST', headers:{'content-type':'application/json','authorization':`Bearer ${ticket}`}, body:JSON.stringify({transcript:data.transcript})}); } };
  const offer = await pc.createOffer(); await pc.setLocalDescription(offer);
  const answer = await fetch(`${session.webrtcCallUrl}?model=${encodeURIComponent(session.model)}`, {method:'POST', headers:{'authorization':`Bearer ${session.authorizationToken}`,'content-type':'application/sdp'}, body:offer.sdp});
  await pc.setRemoteDescription({type:'answer', sdp:await answer.text()});
  mute.disabled = false; status.textContent = 'Connected to the current Medusa session';
};
mute.onclick = () => { muted = !muted; stream.getAudioTracks().forEach(track => track.enabled = !muted); mute.textContent = muted ? 'Unmute' : 'Mute'; };
</script></body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_known_vector() {
        assert_eq!(
            hex::encode(hmac_sha256(
                b"key",
                b"The quick brown fox jumps over the lazy dog"
            )),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn tickets_are_bound_to_identity_and_expiry() {
        let bridge = TelegramMiniAppBridge::new(
            TelegramMiniAppSecret::from_bot_token("123456:abcdefghijklmnopqrstuvwxyz")
                .expect("secret"),
        );
        let identity = TelegramIdentity {
            chat_id: 7,
            topic_id: Some(9),
            user_id: 11,
            chat_kind: super::super::TelegramChatKind::Private,
            bot_mentioned: false,
        };
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("time");
        let ticket = bridge
            .issue_launch_ticket(&identity, "session-1", now)
            .expect("ticket");
        assert_eq!(
            bridge
                .verify_launch_ticket(&ticket.token, &identity, now)
                .expect("verify"),
            "session-1"
        );
        let other = TelegramIdentity {
            user_id: 12,
            ..identity.clone()
        };
        assert!(
            bridge
                .verify_launch_ticket(&ticket.token, &other, now)
                .is_err()
        );
        assert!(
            bridge
                .verify_launch_ticket(&ticket.token, &identity, now + Duration::minutes(6))
                .is_err()
        );
    }

    #[test]
    fn inspected_ticket_preserves_supergroup_identity() {
        let bridge = TelegramMiniAppBridge::new(
            TelegramMiniAppSecret::from_bot_token("123456:abcdefghijklmnopqrstuvwxyz")
                .expect("secret"),
        );
        let identity = TelegramIdentity {
            chat_id: -100_123,
            topic_id: Some(42),
            user_id: 11,
            chat_kind: TelegramChatKind::Supergroup,
            bot_mentioned: true,
        };
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("time");
        let ticket = bridge
            .issue_launch_ticket(&identity, "session-group", now)
            .expect("ticket");

        let binding = bridge
            .inspect_launch_ticket(&ticket.token, now)
            .expect("inspect");
        assert_eq!(binding.identity.chat_id, identity.chat_id);
        assert_eq!(binding.identity.topic_id, identity.topic_id);
        assert_eq!(binding.identity.user_id, identity.user_id);
        assert_eq!(binding.identity.chat_kind, TelegramChatKind::Supergroup);
        assert_eq!(binding.session_id, "session-group");
    }

    #[test]
    fn percent_decoding_is_strict() {
        assert_eq!(
            percent_decode("Ada+Lovelace").expect("decode"),
            "Ada Lovelace"
        );
        assert_eq!(
            percent_decode("%7B%22id%22%3A1%7D").expect("decode"),
            "{\"id\":1}"
        );
        assert!(percent_decode("%zz").is_err());
    }
}
