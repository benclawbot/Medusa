//! Telegram transport policy, command mapping, callback safety, and deterministic rendering.
//!
//! This module deliberately does not own an agent or repository policy. It converts authenticated
//! Telegram input into the shared frontend protocol and converts typed presentation events into
//! transport actions. The live-session broker remains the only execution authority.

use std::collections::{BTreeMap, BTreeSet};

use medusa_protocol::frontend::{
    ApprovalDecision, AttachmentMode, FrontendCommand, FrontendCommandEnvelope, FrontendEvent,
    FrontendEventEnvelope, FrontendKind, PresentationActivityKind, PresentationLifecycle,
    FRONTEND_PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use ulid::Ulid;

const TELEGRAM_TEXT_LIMIT_UTF16: usize = 4_000;
const CALLBACK_PREFIX: &str = "m1:";
const MAX_CALLBACK_RECORDS: usize = 4_096;
const MAX_REPLAY_RECORDS: usize = 4_096;

/// Telegram gateway configuration with conservative, default-deny behavior.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub transport: TelegramTransport,
    pub repository_profile: String,
    pub allowed_users: BTreeSet<i64>,
    pub allowed_group_users: BTreeSet<i64>,
    pub allowed_chats: BTreeSet<i64>,
    pub require_mention: bool,
    pub home_chat_id: Option<i64>,
    pub home_topic_id: Option<i64>,
    pub webhook_secret_configured: bool,
    pub display: TelegramDisplayConfig,
    pub voice: TelegramVoiceConfig,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            transport: TelegramTransport::Polling,
            repository_profile: "default".to_owned(),
            allowed_users: BTreeSet::new(),
            allowed_group_users: BTreeSet::new(),
            allowed_chats: BTreeSet::new(),
            require_mention: true,
            home_chat_id: None,
            home_topic_id: None,
            webhook_secret_configured: false,
            display: TelegramDisplayConfig::default(),
            voice: TelegramVoiceConfig::default(),
        }
    }
}

impl TelegramConfig {
    /// Validates security-sensitive settings before networking starts.
    pub fn validate(&self) -> Result<(), TelegramGatewayError> {
        if !self.enabled {
            return Ok(());
        }
        if self.repository_profile.trim().is_empty() {
            return Err(TelegramGatewayError::InvalidConfiguration(
                "repository profile cannot be empty".to_owned(),
            ));
        }
        if self.allowed_users.is_empty() && self.allowed_group_users.is_empty() {
            return Err(TelegramGatewayError::InvalidConfiguration(
                "Telegram is enabled but no numeric users are allowlisted".to_owned(),
            ));
        }
        if self.transport == TelegramTransport::Webhook && !self.webhook_secret_configured {
            return Err(TelegramGatewayError::InvalidConfiguration(
                "webhook mode requires a configured webhook secret".to_owned(),
            ));
        }
        if self.home_topic_id.is_some() && self.home_chat_id.is_none() {
            return Err(TelegramGatewayError::InvalidConfiguration(
                "a home topic requires a home chat".to_owned(),
            ));
        }
        self.display.validate()?;
        self.voice.validate()
    }
}

/// Bot API delivery mode.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelegramTransport {
    Polling,
    Webhook,
}

/// User-selectable action verbosity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolProgressMode {
    Off,
    New,
    All,
    Verbose,
}

/// Rendering behavior, matching the Hermes-style mobile defaults from issue #568.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct TelegramDisplayConfig {
    pub streaming: bool,
    pub edit_interval_ms: u64,
    pub buffer_threshold_chars: usize,
    pub cursor: String,
    pub fresh_final_after_seconds: u64,
    pub tool_progress: ToolProgressMode,
    pub interim_assistant_messages: bool,
    pub long_running_notifications: bool,
    pub busy_detail: bool,
    pub cleanup_progress: bool,
    pub disable_link_previews: bool,
}

impl Default for TelegramDisplayConfig {
    fn default() -> Self {
        Self {
            streaming: true,
            edit_interval_ms: 300,
            buffer_threshold_chars: 40,
            cursor: " ▉".to_owned(),
            fresh_final_after_seconds: 0,
            tool_progress: ToolProgressMode::New,
            interim_assistant_messages: true,
            long_running_notifications: true,
            busy_detail: false,
            cleanup_progress: false,
            disable_link_previews: false,
        }
    }
}

impl TelegramDisplayConfig {
    fn validate(&self) -> Result<(), TelegramGatewayError> {
        if self.edit_interval_ms < 100 || self.edit_interval_ms > 10_000 {
            return Err(TelegramGatewayError::InvalidConfiguration(
                "Telegram edit interval must be between 100 and 10000 milliseconds".to_owned(),
            ));
        }
        if self.buffer_threshold_chars == 0 || self.buffer_threshold_chars > 4_000 {
            return Err(TelegramGatewayError::InvalidConfiguration(
                "Telegram buffer threshold must be between 1 and 4000 characters".to_owned(),
            ));
        }
        if utf16_len(&self.cursor) > 16 {
            return Err(TelegramGatewayError::InvalidConfiguration(
                "Telegram streaming cursor is too long".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Native Telegram voice-note output policy.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelegramVoiceMode {
    Off,
    VoiceOnly,
    All,
}

/// Voice-note and Mini App settings. Credentials are intentionally absent.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct TelegramVoiceConfig {
    pub mode: TelegramVoiceMode,
    pub transcription_model: String,
    pub voice: String,
    pub mini_app_enabled: bool,
    pub mini_app_public_url: Option<String>,
}

impl Default for TelegramVoiceConfig {
    fn default() -> Self {
        Self {
            mode: TelegramVoiceMode::Off,
            transcription_model: "gpt-4o-mini-transcribe".to_owned(),
            voice: "alloy".to_owned(),
            mini_app_enabled: false,
            mini_app_public_url: None,
        }
    }
}

impl TelegramVoiceConfig {
    fn validate(&self) -> Result<(), TelegramGatewayError> {
        if self.transcription_model.trim().is_empty() || self.voice.trim().is_empty() {
            return Err(TelegramGatewayError::InvalidConfiguration(
                "Telegram voice model and voice cannot be empty".to_owned(),
            ));
        }
        if self.mini_app_enabled {
            let Some(url) = self.mini_app_public_url.as_deref() else {
                return Err(TelegramGatewayError::InvalidConfiguration(
                    "Telegram Mini App requires a public HTTPS URL".to_owned(),
                ));
            };
            if !url.starts_with("https://") {
                return Err(TelegramGatewayError::InvalidConfiguration(
                    "Telegram Mini App URL must use HTTPS".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

/// Telegram chat classification used for authorization and binding.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelegramChatKind {
    Private,
    Group,
    Supergroup,
    Channel,
}

/// Numeric Telegram identity. Usernames are never authorization inputs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramIdentity {
    pub user_id: i64,
    pub chat_id: i64,
    pub topic_id: Option<i64>,
    pub chat_kind: TelegramChatKind,
    pub bot_mentioned: bool,
}

/// An authenticated Telegram message submitted to the command mapper.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramInboundMessage {
    pub identity: TelegramIdentity,
    pub message_id: i64,
    pub text: String,
    pub attached_session_id: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub received_at: OffsetDateTime,
}

/// Telegram-only behavior or a command forwarded to the live-session broker.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum TelegramInboundAction {
    Forward(FrontendCommandEnvelope),
    SetToolProgress(ToolProgressMode),
    SetVoiceMode(TelegramVoiceMode),
    VoiceStatus,
    StartLiveVoice,
    Help,
}

/// Stateful gateway core owned by the daemon process.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TelegramGateway {
    config: TelegramConfig,
    callbacks: CallbackStore,
}

impl TelegramGateway {
    pub fn new(config: TelegramConfig) -> Result<Self, TelegramGatewayError> {
        config.validate()?;
        Ok(Self {
            config,
            callbacks: CallbackStore::default(),
        })
    }

    #[must_use]
    pub fn config(&self) -> &TelegramConfig {
        &self.config
    }

    /// Applies default-deny numeric identity and chat authorization.
    pub fn authorize(&self, identity: &TelegramIdentity) -> Result<(), TelegramGatewayError> {
        if !self.config.enabled {
            return Err(TelegramGatewayError::Disabled);
        }
        match identity.chat_kind {
            TelegramChatKind::Private => {
                if !self.config.allowed_users.contains(&identity.user_id) {
                    return Err(TelegramGatewayError::Unauthorized);
                }
            }
            TelegramChatKind::Group | TelegramChatKind::Supergroup => {
                if !self.config.allowed_chats.contains(&identity.chat_id)
                    || !self.config.allowed_group_users.contains(&identity.user_id)
                {
                    return Err(TelegramGatewayError::Unauthorized);
                }
                if self.config.require_mention && !identity.bot_mentioned {
                    return Err(TelegramGatewayError::MentionRequired);
                }
            }
            TelegramChatKind::Channel => return Err(TelegramGatewayError::Unauthorized),
        }
        Ok(())
    }

    /// Maps a Telegram command or ordinary text to the shared frontend protocol.
    pub fn map_message(
        &self,
        message: &TelegramInboundMessage,
    ) -> Result<TelegramInboundAction, TelegramGatewayError> {
        self.authorize(&message.identity)?;
        let trimmed = message.text.trim();
        if trimmed.is_empty() {
            return Err(TelegramGatewayError::EmptyMessage);
        }
        let (command, args) = split_command(trimmed);
        let action = match command {
            None => TelegramInboundAction::Forward(self.envelope(
                message,
                FrontendCommand::Submit {
                    text: trimmed.to_owned(),
                    attachment_ids: Vec::new(),
                },
            )?),
            Some("/new") => TelegramInboundAction::Forward(self.envelope(
                message,
                FrontendCommand::CreateSession {
                    repository_profile: self.config.repository_profile.clone(),
                    objective: non_empty(args).map(str::to_owned),
                },
            )?),
            Some("/sessions") => TelegramInboundAction::Forward(
                self.envelope(message, FrontendCommand::ListSessions)?,
            ),
            Some("/attach") => TelegramInboundAction::Forward(self.envelope(
                message,
                FrontendCommand::Attach {
                    session_id: required(args, "usage: /attach <session>")?.to_owned(),
                    mode: AttachmentMode::Owner,
                    after_cursor: None,
                },
            )?),
            Some("/detach") => {
                TelegramInboundAction::Forward(self.envelope(message, FrontendCommand::Detach)?)
            }
            Some("/resume") => TelegramInboundAction::Forward(self.envelope(
                message,
                FrontendCommand::ResumeSession {
                    session_id: required(args, "usage: /resume <session>")?.to_owned(),
                },
            )?),
            Some("/status") => TelegramInboundAction::Forward(
                self.envelope(message, FrontendCommand::ShowStatus)?,
            ),
            Some("/stop") => TelegramInboundAction::Forward(
                self.envelope(message, FrontendCommand::CancelTurn)?,
            ),
            Some("/model") => TelegramInboundAction::Forward(self.envelope(
                message,
                FrontendCommand::ConfigureModel {
                    provider: None,
                    model: required(args, "usage: /model <model>")?.to_owned(),
                },
            )?),
            Some("/effort") => TelegramInboundAction::Forward(self.envelope(
                message,
                FrontendCommand::SetEffort {
                    effort: required(args, "usage: /effort <low|medium|high>")?.to_owned(),
                },
            )?),
            Some("/plan") => TelegramInboundAction::Forward(self.envelope(
                message,
                FrontendCommand::SetPlanMode {
                    enabled: parse_on_off(args, "usage: /plan <on|off>")?,
                },
            )?),
            Some("/verbose") => TelegramInboundAction::SetToolProgress(parse_progress(args)?),
            Some("/voice") => parse_voice(args)?,
            Some("/help") => TelegramInboundAction::Help,
            Some(other) => return Err(TelegramGatewayError::UnknownCommand(other.to_owned())),
        };
        Ok(action)
    }

    /// Issues opaque, one-shot callbacks for one approval request.
    pub fn issue_approval_callbacks(
        &mut self,
        identity: &TelegramIdentity,
        session_id: &str,
        turn_id: Option<&str>,
        approval_id: &str,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<Vec<TelegramInlineButton>, TelegramGatewayError> {
        self.authorize(identity)?;
        if session_id.trim().is_empty() || approval_id.trim().is_empty() || expires_at <= now {
            return Err(TelegramGatewayError::InvalidCallbackRequest);
        }
        let approve = self.callbacks.issue(
            identity,
            session_id,
            turn_id,
            approval_id,
            ApprovalDecision::ApproveOnce,
            expires_at,
            now,
        );
        let deny = self.callbacks.issue(
            identity,
            session_id,
            turn_id,
            approval_id,
            ApprovalDecision::Deny,
            expires_at,
            now,
        );
        Ok(vec![
            TelegramInlineButton {
                label: "Approve once".to_owned(),
                callback_data: approve,
            },
            TelegramInlineButton {
                label: "Deny".to_owned(),
                callback_data: deny,
            },
        ])
    }

    /// Resolves an opaque callback into one idempotent authoritative command.
    pub fn resolve_callback(
        &mut self,
        identity: &TelegramIdentity,
        callback_data: &str,
        now: OffsetDateTime,
    ) -> Result<FrontendCommandEnvelope, TelegramGatewayError> {
        self.authorize(identity)?;
        let resolved = self.callbacks.resolve(identity, callback_data, now)?;
        let command = FrontendCommandEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            command_id: format!("telegram-callback-{}", resolved.nonce),
            idempotency_key: format!("telegram-callback:{}", resolved.nonce),
            frontend: FrontendKind::Telegram,
            client_id: telegram_client_id(identity),
            session_id: Some(resolved.session_id),
            turn_id: resolved.turn_id,
            timestamp: now,
            command: FrontendCommand::ResolveApproval {
                approval_id: resolved.approval_id,
                decision: resolved.decision,
            },
        };
        command
            .validate()
            .map_err(|error| TelegramGatewayError::Protocol(error.to_owned()))?;
        Ok(command)
    }

    fn envelope(
        &self,
        message: &TelegramInboundMessage,
        command: FrontendCommand,
    ) -> Result<FrontendCommandEnvelope, TelegramGatewayError> {
        let identity = &message.identity;
        let stable = format!(
            "{}:{}:{}:{}",
            identity.chat_id,
            identity.topic_id.unwrap_or_default(),
            identity.user_id,
            message.message_id
        );
        let digest = hex::encode(Sha256::digest(stable.as_bytes()));
        let envelope = FrontendCommandEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            command_id: format!("telegram-{}", &digest[..24]),
            idempotency_key: format!("telegram:{stable}"),
            frontend: FrontendKind::Telegram,
            client_id: telegram_client_id(identity),
            session_id: message.attached_session_id.clone(),
            turn_id: None,
            timestamp: message.received_at,
            command,
        };
        envelope
            .validate()
            .map_err(|error| TelegramGatewayError::Protocol(error.to_owned()))?;
        Ok(envelope)
    }
}

fn telegram_client_id(identity: &TelegramIdentity) -> String {
    format!(
        "telegram:{}:{}:{}",
        identity.chat_id,
        identity.topic_id.unwrap_or_default(),
        identity.user_id
    )
}

fn split_command(input: &str) -> (Option<&str>, &str) {
    if !input.starts_with('/') {
        return (None, input);
    }
    let (command, args) = input.split_once(char::is_whitespace).unwrap_or((input, ""));
    let command = command.split_once('@').map_or(command, |(name, _)| name);
    (Some(command), args.trim())
}

fn non_empty(value: &str) -> Option<&str> {
    (!value.trim().is_empty()).then_some(value.trim())
}

fn required<'a>(value: &'a str, usage: &'static str) -> Result<&'a str, TelegramGatewayError> {
    non_empty(value).ok_or(TelegramGatewayError::MissingArgument(usage))
}

fn parse_on_off(value: &str, usage: &'static str) -> Result<bool, TelegramGatewayError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err(TelegramGatewayError::MissingArgument(usage)),
    }
}

fn parse_progress(value: &str) -> Result<ToolProgressMode, TelegramGatewayError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "off" => Ok(ToolProgressMode::Off),
        "new" => Ok(ToolProgressMode::New),
        "all" => Ok(ToolProgressMode::All),
        "verbose" => Ok(ToolProgressMode::Verbose),
        _ => Err(TelegramGatewayError::MissingArgument(
            "usage: /verbose <off|new|all|verbose>",
        )),
    }
}

fn parse_voice(value: &str) -> Result<TelegramInboundAction, TelegramGatewayError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "off" => Ok(TelegramInboundAction::SetVoiceMode(TelegramVoiceMode::Off)),
        "on" => Ok(TelegramInboundAction::SetVoiceMode(
            TelegramVoiceMode::VoiceOnly,
        )),
        "tts" => Ok(TelegramInboundAction::SetVoiceMode(TelegramVoiceMode::All)),
        "status" => Ok(TelegramInboundAction::VoiceStatus),
        "live" => Ok(TelegramInboundAction::StartLiveVoice),
        _ => Err(TelegramGatewayError::MissingArgument(
            "usage: /voice <off|on|tts|status|live>",
        )),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CallbackStore {
    records: BTreeMap<String, CallbackRecord>,
}

impl Default for CallbackStore {
    fn default() -> Self {
        Self {
            records: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CallbackRecord {
    nonce: String,
    user_id: i64,
    chat_id: i64,
    topic_id: Option<i64>,
    session_id: String,
    turn_id: Option<String>,
    approval_id: String,
    decision: ApprovalDecision,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    issued_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    consumed_at: Option<OffsetDateTime>,
}

struct ResolvedCallback {
    nonce: String,
    session_id: String,
    turn_id: Option<String>,
    approval_id: String,
    decision: ApprovalDecision,
}

impl CallbackStore {
    #[allow(clippy::too_many_arguments)]
    fn issue(
        &mut self,
        identity: &TelegramIdentity,
        session_id: &str,
        turn_id: Option<&str>,
        approval_id: &str,
        decision: ApprovalDecision,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> String {
        self.prune(now);
        let nonce = Ulid::new().to_string();
        self.records.insert(
            nonce.clone(),
            CallbackRecord {
                nonce: nonce.clone(),
                user_id: identity.user_id,
                chat_id: identity.chat_id,
                topic_id: identity.topic_id,
                session_id: session_id.to_owned(),
                turn_id: turn_id.map(str::to_owned),
                approval_id: approval_id.to_owned(),
                decision,
                expires_at,
                issued_at: now,
                consumed_at: None,
            },
        );
        format!("{CALLBACK_PREFIX}{nonce}")
    }

    fn resolve(
        &mut self,
        identity: &TelegramIdentity,
        callback_data: &str,
        now: OffsetDateTime,
    ) -> Result<ResolvedCallback, TelegramGatewayError> {
        let nonce = callback_data
            .strip_prefix(CALLBACK_PREFIX)
            .ok_or(TelegramGatewayError::InvalidCallback)?;
        if nonce.is_empty() || callback_data.len() > 64 {
            return Err(TelegramGatewayError::InvalidCallback);
        }
        let record = self
            .records
            .get_mut(nonce)
            .ok_or(TelegramGatewayError::InvalidCallback)?;
        if record.user_id != identity.user_id
            || record.chat_id != identity.chat_id
            || record.topic_id != identity.topic_id
        {
            return Err(TelegramGatewayError::CallbackIdentityMismatch);
        }
        if record.consumed_at.is_some() {
            return Err(TelegramGatewayError::CallbackAlreadyResolved);
        }
        if record.expires_at < now {
            return Err(TelegramGatewayError::CallbackExpired);
        }
        record.consumed_at = Some(now);
        Ok(ResolvedCallback {
            nonce: record.nonce.clone(),
            session_id: record.session_id.clone(),
            turn_id: record.turn_id.clone(),
            approval_id: record.approval_id.clone(),
            decision: record.decision,
        })
    }

    fn prune(&mut self, now: OffsetDateTime) {
        self.records.retain(|_, record| {
            record.expires_at >= now
                || record.consumed_at.is_some_and(|consumed| consumed + Duration::days(1) >= now)
        });
        while self.records.len() >= MAX_CALLBACK_RECORDS {
            let Some(oldest) = self
                .records
                .iter()
                .min_by_key(|(_, record)| record.issued_at)
                .map(|(nonce, _)| nonce.clone())
            else {
                break;
            };
            self.records.remove(&oldest);
        }
    }
}

/// Telegram reaction applied to the source user message.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelegramReaction {
    Processing,
    Success,
    Failure,
}

/// Telegram parse mode used by a transport action.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelegramParseMode {
    Plain,
    MarkdownV2,
}

/// Stable edit-in-place message slots. Transport state maps these to Telegram message IDs.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum TelegramMessageSlot {
    Preview(u16),
    Progress,
    Plan,
    Team,
    Question(String),
    Approval(String),
    Interim(String),
    Notice(String),
}

/// Inline callback data sent through the Telegram Bot API.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramInlineButton {
    pub label: String,
    pub callback_data: String,
}

/// Semantic button intent produced by the renderer before opaque callback encoding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum TelegramButtonIntent {
    AnswerQuestion {
        question_id: String,
        value: String,
    },
    Approval {
        approval_id: String,
        decision: ApprovalDecision,
    },
    Details {
        reference: String,
    },
    CancelQueued,
    StartLiveVoice,
}

/// Button shown by a deterministic render action.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramRenderButton {
    pub label: String,
    pub intent: TelegramButtonIntent,
}

/// Side-effect requested from the Telegram transport adapter.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum TelegramAction {
    SetReaction {
        reaction: Option<TelegramReaction>,
    },
    SetTyping {
        active: bool,
    },
    UpsertText {
        slot: TelegramMessageSlot,
        text: String,
        parse_mode: TelegramParseMode,
        buttons: Vec<TelegramRenderButton>,
        disable_link_preview: bool,
    },
    DeleteSlot {
        slot: TelegramMessageSlot,
    },
    SendArtifact {
        artifact_id: String,
        evidence_ref: String,
        caption: Option<String>,
    },
}

/// Replay-safe deterministic renderer over shared presentation events.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TelegramRenderer {
    config: TelegramDisplayConfig,
    source_message_id: i64,
    preview: String,
    preview_chunks: Vec<String>,
    last_edit_at: Option<OffsetDateTime>,
    last_flushed_chars: usize,
    cursor_events: BTreeMap<u64, String>,
    active: bool,
}

impl TelegramRenderer {
    pub fn new(config: TelegramDisplayConfig, source_message_id: i64) -> Result<Self, TelegramGatewayError> {
        config.validate()?;
        Ok(Self {
            config,
            source_message_id,
            preview: String::new(),
            preview_chunks: Vec::new(),
            last_edit_at: None,
            last_flushed_chars: 0,
            cursor_events: BTreeMap::new(),
            active: false,
        })
    }

    #[must_use]
    pub const fn source_message_id(&self) -> i64 {
        self.source_message_id
    }

    pub fn render(
        &mut self,
        envelope: &FrontendEventEnvelope,
        now: OffsetDateTime,
    ) -> Result<Vec<TelegramAction>, TelegramGatewayError> {
        envelope
            .validate()
            .map_err(|error| TelegramGatewayError::Protocol(error.to_owned()))?;
        if let Some(existing) = self.cursor_events.get(&envelope.cursor) {
            if existing == &envelope.event_id {
                return Ok(Vec::new());
            }
            return Err(TelegramGatewayError::CursorConflict(envelope.cursor));
        }
        if self
            .cursor_events
            .last_key_value()
            .is_some_and(|(cursor, _)| envelope.cursor < *cursor)
        {
            return Err(TelegramGatewayError::StaleCursor(envelope.cursor));
        }
        self.cursor_events
            .insert(envelope.cursor, envelope.event_id.clone());
        while self.cursor_events.len() > MAX_REPLAY_RECORDS {
            let Some(first) = self.cursor_events.first_key_value().map(|(cursor, _)| *cursor) else {
                break;
            };
            self.cursor_events.remove(&first);
        }

        let mut actions = Vec::new();
        match &envelope.event {
            FrontendEvent::SubmissionAccepted | FrontendEvent::Started => {
                self.active = true;
                actions.push(TelegramAction::SetReaction {
                    reaction: Some(TelegramReaction::Processing),
                });
                actions.push(TelegramAction::SetTyping { active: true });
            }
            FrontendEvent::SubmissionQueued { position } => {
                actions.push(TelegramAction::UpsertText {
                    slot: TelegramMessageSlot::Progress,
                    text: format!("Queued — position {position}"),
                    parse_mode: TelegramParseMode::Plain,
                    buttons: vec![TelegramRenderButton {
                        label: "Cancel queued".to_owned(),
                        intent: TelegramButtonIntent::CancelQueued,
                    }],
                    disable_link_preview: self.config.disable_link_previews,
                });
            }
            FrontendEvent::AssistantTextDelta { text } => {
                self.preview.push_str(text);
                if self.should_flush(now) {
                    actions.extend(self.flush_preview(true, now));
                }
            }
            FrontendEvent::AssistantInterim { text }
                if self.config.interim_assistant_messages =>
            {
                actions.push(TelegramAction::UpsertText {
                    slot: TelegramMessageSlot::Interim(envelope.event_id.clone()),
                    text: telegram_markdown_v2(text),
                    parse_mode: TelegramParseMode::MarkdownV2,
                    buttons: Vec::new(),
                    disable_link_preview: self.config.disable_link_previews,
                });
            }
            FrontendEvent::Activity(activity) => {
                if let Some(text) = render_activity(
                    activity.kind,
                    activity.lifecycle,
                    &activity.title,
                    &activity.details,
                    self.config.tool_progress,
                ) {
                    actions.push(TelegramAction::UpsertText {
                        slot: TelegramMessageSlot::Progress,
                        text,
                        parse_mode: TelegramParseMode::Plain,
                        buttons: activity
                            .evidence_ref
                            .as_ref()
                            .map(|reference| {
                                vec![TelegramRenderButton {
                                    label: "Details".to_owned(),
                                    intent: TelegramButtonIntent::Details {
                                        reference: reference.clone(),
                                    },
                                }]
                            })
                            .unwrap_or_default(),
                        disable_link_preview: self.config.disable_link_previews,
                    });
                }
            }
            FrontendEvent::Plan { steps, current } => {
                let mut text = String::from("Plan\n\n");
                for step in steps {
                    text.push_str(lifecycle_icon(step.lifecycle));
                    text.push(' ');
                    text.push_str(&step.title);
                    text.push('\n');
                }
                if let Some(current) = current {
                    text.push_str("\nCurrent: ");
                    text.push_str(current);
                }
                actions.push(TelegramAction::UpsertText {
                    slot: TelegramMessageSlot::Plan,
                    text,
                    parse_mode: TelegramParseMode::Plain,
                    buttons: Vec::new(),
                    disable_link_preview: self.config.disable_link_previews,
                });
            }
            FrontendEvent::Team {
                workers,
                verification,
            } => {
                let mut text = String::from("Team\n\n");
                for worker in workers {
                    text.push_str(lifecycle_icon(worker.lifecycle));
                    text.push(' ');
                    text.push_str(&worker.role);
                    text.push_str(" — ");
                    text.push_str(&worker.task);
                    if self.config.busy_detail {
                        if let Some(action) = &worker.current_action {
                            text.push_str(" — ");
                            text.push_str(action);
                        }
                    }
                    text.push('\n');
                }
                if let Some(verification) = verification {
                    text.push_str("\nVerification: ");
                    text.push_str(verification);
                }
                actions.push(TelegramAction::UpsertText {
                    slot: TelegramMessageSlot::Team,
                    text,
                    parse_mode: TelegramParseMode::Plain,
                    buttons: Vec::new(),
                    disable_link_preview: self.config.disable_link_previews,
                });
            }
            FrontendEvent::Question(question) => {
                let buttons = question
                    .options
                    .iter()
                    .map(|option| TelegramRenderButton {
                        label: option.label.clone(),
                        intent: TelegramButtonIntent::AnswerQuestion {
                            question_id: question.question_id.clone(),
                            value: option.value.clone(),
                        },
                    })
                    .collect();
                actions.push(TelegramAction::UpsertText {
                    slot: TelegramMessageSlot::Question(question.question_id.clone()),
                    text: question.prompt.clone(),
                    parse_mode: TelegramParseMode::Plain,
                    buttons,
                    disable_link_preview: self.config.disable_link_previews,
                });
            }
            FrontendEvent::ApprovalRequired(approval) => {
                let text = format!(
                    "Approval required\n\nAction: {}\nScope: {}\nReason: {}\nRisk: {}",
                    approval.action, approval.scope, approval.reason, approval.risk
                );
                actions.push(TelegramAction::UpsertText {
                    slot: TelegramMessageSlot::Approval(approval.approval_id.clone()),
                    text,
                    parse_mode: TelegramParseMode::Plain,
                    buttons: vec![
                        TelegramRenderButton {
                            label: "Approve once".to_owned(),
                            intent: TelegramButtonIntent::Approval {
                                approval_id: approval.approval_id.clone(),
                                decision: ApprovalDecision::ApproveOnce,
                            },
                        },
                        TelegramRenderButton {
                            label: "Deny".to_owned(),
                            intent: TelegramButtonIntent::Approval {
                                approval_id: approval.approval_id.clone(),
                                decision: ApprovalDecision::Deny,
                            },
                        },
                        TelegramRenderButton {
                            label: "Details".to_owned(),
                            intent: TelegramButtonIntent::Details {
                                reference: approval.approval_id.clone(),
                            },
                        },
                    ],
                    disable_link_preview: self.config.disable_link_previews,
                });
            }
            FrontendEvent::Progress { turn, phase } if self.config.long_running_notifications => {
                let suffix = phase
                    .as_ref()
                    .map(|phase| format!(" — {phase}"))
                    .unwrap_or_default();
                actions.push(TelegramAction::UpsertText {
                    slot: TelegramMessageSlot::Progress,
                    text: format!("⏳ Working — turn {turn}{suffix}"),
                    parse_mode: TelegramParseMode::Plain,
                    buttons: Vec::new(),
                    disable_link_preview: self.config.disable_link_previews,
                });
            }
            FrontendEvent::SettingsChanged {
                model,
                effort,
                plan_mode,
            } => {
                actions.push(TelegramAction::UpsertText {
                    slot: TelegramMessageSlot::Notice(envelope.event_id.clone()),
                    text: format!(
                        "Settings updated — model {model}, effort {effort}, plan {}",
                        if *plan_mode { "on" } else { "off" }
                    ),
                    parse_mode: TelegramParseMode::Plain,
                    buttons: Vec::new(),
                    disable_link_preview: self.config.disable_link_previews,
                });
            }
            FrontendEvent::Notice {
                severity,
                title,
                details,
            } => {
                let mut text = format!("{}: {}", severity.to_uppercase(), title);
                if !details.is_empty() {
                    text.push_str("\n");
                    text.push_str(&details.join("\n"));
                }
                actions.push(TelegramAction::UpsertText {
                    slot: TelegramMessageSlot::Notice(envelope.event_id.clone()),
                    text,
                    parse_mode: TelegramParseMode::Plain,
                    buttons: Vec::new(),
                    disable_link_preview: self.config.disable_link_previews,
                });
            }
            FrontendEvent::Artifact(artifact) => actions.push(TelegramAction::SendArtifact {
                artifact_id: artifact.artifact_id.clone(),
                evidence_ref: artifact.evidence_ref.clone(),
                caption: artifact.caption.clone(),
            }),
            FrontendEvent::TurnFinished => {
                actions.extend(self.flush_preview(false, now));
                actions.push(TelegramAction::SetTyping { active: false });
            }
            FrontendEvent::Completed { summary } => {
                actions.extend(self.flush_preview(false, now));
                if let Some(summary) = summary {
                    if self.preview.trim().is_empty() {
                        actions.push(TelegramAction::UpsertText {
                            slot: TelegramMessageSlot::Preview(0),
                            text: telegram_markdown_v2(summary),
                            parse_mode: TelegramParseMode::MarkdownV2,
                            buttons: Vec::new(),
                            disable_link_preview: self.config.disable_link_previews,
                        });
                    }
                }
                actions.push(TelegramAction::SetTyping { active: false });
                actions.push(TelegramAction::SetReaction {
                    reaction: Some(TelegramReaction::Success),
                });
                if self.config.cleanup_progress {
                    actions.push(TelegramAction::DeleteSlot {
                        slot: TelegramMessageSlot::Progress,
                    });
                }
                self.active = false;
            }
            FrontendEvent::Cancelled { reason } => {
                actions.push(TelegramAction::SetTyping { active: false });
                actions.push(TelegramAction::SetReaction { reaction: None });
                actions.push(TelegramAction::UpsertText {
                    slot: TelegramMessageSlot::Progress,
                    text: reason
                        .as_ref()
                        .map(|reason| format!("Cancelled — {reason}"))
                        .unwrap_or_else(|| "Cancelled".to_owned()),
                    parse_mode: TelegramParseMode::Plain,
                    buttons: Vec::new(),
                    disable_link_preview: self.config.disable_link_previews,
                });
                self.active = false;
            }
            FrontendEvent::Failed { message, recovery } => {
                actions.extend(self.flush_preview(false, now));
                actions.push(TelegramAction::SetTyping { active: false });
                actions.push(TelegramAction::SetReaction {
                    reaction: Some(TelegramReaction::Failure),
                });
                let mut text = format!("Failed — {message}");
                if !recovery.is_empty() {
                    text.push_str("\n\nRecovery:\n");
                    for item in recovery {
                        text.push_str("• ");
                        text.push_str(item);
                        text.push('\n');
                    }
                }
                actions.push(TelegramAction::UpsertText {
                    slot: TelegramMessageSlot::Progress,
                    text,
                    parse_mode: TelegramParseMode::Plain,
                    buttons: Vec::new(),
                    disable_link_preview: self.config.disable_link_previews,
                });
                self.active = false;
            }
            FrontendEvent::Usage { .. }
            | FrontendEvent::AssistantInterim { .. }
            | FrontendEvent::Progress { .. } => {}
        }
        Ok(actions)
    }

    fn should_flush(&self, now: OffsetDateTime) -> bool {
        if !self.config.streaming {
            return false;
        }
        if self.last_edit_at.is_none() {
            return true;
        }
        let new_chars = self.preview.chars().count().saturating_sub(self.last_flushed_chars);
        let elapsed = now - self.last_edit_at.expect("checked above");
        new_chars >= self.config.buffer_threshold_chars
            || elapsed >= Duration::milliseconds(self.config.edit_interval_ms as i64)
    }

    fn flush_preview(&mut self, streaming: bool, now: OffsetDateTime) -> Vec<TelegramAction> {
        if self.preview.is_empty() {
            return Vec::new();
        }
        let chunks = split_telegram_text(&self.preview, TELEGRAM_TEXT_LIMIT_UTF16);
        let mut actions = Vec::new();
        for (index, chunk) in chunks.iter().enumerate() {
            let final_chunk = index + 1 == chunks.len();
            let text = if streaming && final_chunk {
                format!("{chunk}{}", self.config.cursor)
            } else if streaming {
                chunk.clone()
            } else {
                telegram_markdown_v2(chunk)
            };
            actions.push(TelegramAction::UpsertText {
                slot: TelegramMessageSlot::Preview(index as u16),
                text,
                parse_mode: if streaming {
                    TelegramParseMode::Plain
                } else {
                    TelegramParseMode::MarkdownV2
                },
                buttons: Vec::new(),
                disable_link_preview: self.config.disable_link_previews,
            });
        }
        for index in chunks.len()..self.preview_chunks.len() {
            actions.push(TelegramAction::DeleteSlot {
                slot: TelegramMessageSlot::Preview(index as u16),
            });
        }
        self.preview_chunks = chunks;
        self.last_flushed_chars = self.preview.chars().count();
        self.last_edit_at = Some(now);
        actions
    }
}

fn render_activity(
    kind: PresentationActivityKind,
    lifecycle: PresentationLifecycle,
    title: &str,
    details: &[String],
    mode: ToolProgressMode,
) -> Option<String> {
    if mode == ToolProgressMode::Off {
        return None;
    }
    if mode == ToolProgressMode::New
        && !matches!(lifecycle, PresentationLifecycle::Active | PresentationLifecycle::Waiting)
    {
        return None;
    }
    let icon = match kind {
        PresentationActivityKind::Assistant => "💬",
        PresentationActivityKind::RepositoryRead => "🔎",
        PresentationActivityKind::Edit => "✏️",
        PresentationActivityKind::Command => "💻",
        PresentationActivityKind::Test | PresentationActivityKind::Verification => "🧪",
        PresentationActivityKind::Approval => "🔐",
        PresentationActivityKind::Worker => "👥",
        PresentationActivityKind::Integration => "🔀",
        PresentationActivityKind::Recovery => "♻️",
        PresentationActivityKind::Progress => "⏳",
        PresentationActivityKind::Done => "✅",
        PresentationActivityKind::Error => "❌",
    };
    let mut text = format!("{icon} {title}");
    if mode == ToolProgressMode::Verbose && !details.is_empty() {
        text.push_str("\n");
        text.push_str(&details.join("\n"));
    } else if matches!(mode, ToolProgressMode::All | ToolProgressMode::Verbose)
        && lifecycle == PresentationLifecycle::Failed
        && !details.is_empty()
    {
        text.push_str(" — ");
        text.push_str(&details.join("; "));
    }
    Some(text)
}

fn lifecycle_icon(lifecycle: PresentationLifecycle) -> &'static str {
    match lifecycle {
        PresentationLifecycle::Active => "◉",
        PresentationLifecycle::Waiting | PresentationLifecycle::Informational => "○",
        PresentationLifecycle::Succeeded => "✓",
        PresentationLifecycle::Failed => "✕",
        PresentationLifecycle::Cancelled => "–",
    }
}

/// Telegram counts message limits in UTF-16 code units.
#[must_use]
pub fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

/// Splits text without breaking Unicode scalar values and keeps fenced code blocks valid.
#[must_use]
pub fn split_telegram_text(value: &str, limit_utf16: usize) -> Vec<String> {
    if value.is_empty() || limit_utf16 == 0 {
        return if value.is_empty() {
            Vec::new()
        } else {
            vec![value.to_owned()]
        };
    }
    if utf16_len(value) <= limit_utf16 {
        return vec![value.to_owned()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    let mut fenced = false;
    for line in value.split_inclusive('\n') {
        let line_len = utf16_len(line);
        if current_len + line_len > limit_utf16 && !current.is_empty() {
            if fenced && current_len + 4 <= limit_utf16 {
                current.push_str("```\n");
            }
            chunks.push(current);
            current = if fenced { "```\n".to_owned() } else { String::new() };
            current_len = utf16_len(&current);
        }
        if line_len > limit_utf16 {
            for character in line.chars() {
                let char_len = character.len_utf16();
                if current_len + char_len > limit_utf16 && !current.is_empty() {
                    if fenced && current_len + 4 <= limit_utf16 {
                        current.push_str("```\n");
                    }
                    chunks.push(current);
                    current = if fenced { "```\n".to_owned() } else { String::new() };
                    current_len = utf16_len(&current);
                }
                current.push(character);
                current_len += char_len;
            }
        } else {
            current.push_str(line);
            current_len += line_len;
        }
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
    }
    if !current.is_empty() {
        if fenced && current_len + 4 <= limit_utf16 {
            current.push_str("```\n");
        }
        chunks.push(current);
    }
    chunks
}

/// Converts ordinary Markdown-like text into conservative Telegram MarkdownV2.
#[must_use]
pub fn telegram_markdown_v2(value: &str) -> String {
    let normalized = normalize_markdown_tables(value);
    let mut escaped = String::with_capacity(normalized.len());
    let mut fenced = false;
    let mut inline_code = false;
    let mut chars = normalized.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '`' {
            if chars.peek() == Some(&'`') {
                let mut clone = chars.clone();
                clone.next();
                if clone.peek() == Some(&'`') {
                    chars.next();
                    chars.next();
                    escaped.push_str("```");
                    fenced = !fenced;
                    continue;
                }
            }
            if !fenced {
                inline_code = !inline_code;
            }
            escaped.push('`');
            continue;
        }
        if fenced || inline_code {
            if matches!(character, '\\' | '`') {
                escaped.push('\\');
            }
            escaped.push(character);
            continue;
        }
        if matches!(
            character,
            '_' | '*' | '[' | ']' | '(' | ')' | '~' | '>' | '#' | '+' | '-' | '=' | '|'
                | '{' | '}' | '.' | '!' | '\\'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Rewrites basic Markdown tables into readable row-group bullets for Telegram.
#[must_use]
pub fn normalize_markdown_tables(value: &str) -> String {
    let lines = value.lines().collect::<Vec<_>>();
    let mut output = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        if index + 1 < lines.len()
            && lines[index].contains('|')
            && is_table_separator(lines[index + 1])
        {
            let headers = table_cells(lines[index]);
            index += 2;
            while index < lines.len() && lines[index].contains('|') {
                let cells = table_cells(lines[index]);
                let row = headers
                    .iter()
                    .zip(cells.iter())
                    .map(|(header, cell)| format!("{header}: {cell}"))
                    .collect::<Vec<_>>()
                    .join("; ");
                output.push(format!("• {row}"));
                index += 1;
            }
            continue;
        }
        output.push(lines[index].to_owned());
        index += 1;
    }
    output.join("\n")
}

fn table_cells(line: &str) -> Vec<String> {
    line.trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_owned())
        .filter(|cell| !cell.is_empty())
        .collect()
}

fn is_table_separator(line: &str) -> bool {
    let cells = table_cells(line);
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let trimmed = cell.trim_matches(':').trim();
            trimmed.len() >= 3 && trimmed.bytes().all(|byte| byte == b'-')
        })
}

#[derive(Debug, Error)]
pub enum TelegramGatewayError {
    #[error("Telegram gateway is disabled")]
    Disabled,
    #[error("Telegram identity is not authorized")]
    Unauthorized,
    #[error("Telegram group message must explicitly mention the bot")]
    MentionRequired,
    #[error("Telegram message cannot be empty")]
    EmptyMessage,
    #[error("unknown Telegram command {0}")]
    UnknownCommand(String),
    #[error("{0}")]
    MissingArgument(&'static str),
    #[error("invalid Telegram configuration: {0}")]
    InvalidConfiguration(String),
    #[error("invalid Telegram callback request")]
    InvalidCallbackRequest,
    #[error("invalid or unknown Telegram callback")]
    InvalidCallback,
    #[error("Telegram callback belongs to a different user, chat, or topic")]
    CallbackIdentityMismatch,
    #[error("Telegram callback has expired")]
    CallbackExpired,
    #[error("Telegram callback was already resolved")]
    CallbackAlreadyResolved,
    #[error("frontend protocol rejected Telegram data: {0}")]
    Protocol(String),
    #[error("Telegram event cursor {0} conflicts with an already rendered event")]
    CursorConflict(u64),
    #[error("Telegram event cursor {0} is older than the current renderer cursor")]
    StaleCursor(u64),
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_protocol::frontend::{
        FrontendEvent, PresentationActivity, PresentationActivityKind, PresentationApproval,
        PresentationLifecycle,
    };
    use time::macros::datetime;

    fn config() -> TelegramConfig {
        TelegramConfig {
            enabled: true,
            allowed_users: BTreeSet::from([42]),
            allowed_group_users: BTreeSet::from([42]),
            allowed_chats: BTreeSet::from([-100]),
            ..TelegramConfig::default()
        }
    }

    fn private_identity() -> TelegramIdentity {
        TelegramIdentity {
            user_id: 42,
            chat_id: 42,
            topic_id: None,
            chat_kind: TelegramChatKind::Private,
            bot_mentioned: false,
        }
    }

    fn message(text: &str) -> TelegramInboundMessage {
        TelegramInboundMessage {
            identity: private_identity(),
            message_id: 9,
            text: text.to_owned(),
            attached_session_id: Some("session-1".to_owned()),
            received_at: datetime!(2026-07-30 16:00 UTC),
        }
    }

    fn event(cursor: u64, event: FrontendEvent) -> FrontendEventEnvelope {
        FrontendEventEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            event_id: format!("event-{cursor}"),
            cursor,
            session_id: "session-1".to_owned(),
            turn_id: Some("turn-1".to_owned()),
            parent_event_id: None,
            correlation_id: "correlation-1".to_owned(),
            timestamp: datetime!(2026-07-30 16:00 UTC),
            lifecycle: PresentationLifecycle::Active,
            event,
        }
    }

    #[test]
    fn authorization_is_default_deny_and_numeric() {
        let gateway = TelegramGateway::new(config()).expect("gateway");
        gateway.authorize(&private_identity()).expect("allowed");
        let mut unauthorized = private_identity();
        unauthorized.user_id = 7;
        assert!(matches!(
            gateway.authorize(&unauthorized),
            Err(TelegramGatewayError::Unauthorized)
        ));
    }

    #[test]
    fn group_authorization_requires_chat_user_and_mention() {
        let gateway = TelegramGateway::new(config()).expect("gateway");
        let mut identity = private_identity();
        identity.chat_id = -100;
        identity.chat_kind = TelegramChatKind::Supergroup;
        assert!(matches!(
            gateway.authorize(&identity),
            Err(TelegramGatewayError::MentionRequired)
        ));
        identity.bot_mentioned = true;
        gateway.authorize(&identity).expect("authorized group");
    }

    #[test]
    fn ordinary_text_and_commands_map_to_shared_protocol() {
        let gateway = TelegramGateway::new(config()).expect("gateway");
        let TelegramInboundAction::Forward(submit) = gateway
            .map_message(&message("inspect the failing test"))
            .expect("submission")
        else {
            panic!("expected forwarded submission");
        };
        assert!(matches!(submit.command, FrontendCommand::Submit { .. }));

        let TelegramInboundAction::Forward(stop) = gateway
            .map_message(&message("/stop"))
            .expect("stop command")
        else {
            panic!("expected forwarded cancellation");
        };
        assert_eq!(stop.command, FrontendCommand::CancelTurn);
    }

    #[test]
    fn command_idempotency_is_stable_for_one_telegram_message() {
        let gateway = TelegramGateway::new(config()).expect("gateway");
        let first = gateway.map_message(&message("/status")).expect("first");
        let second = gateway.map_message(&message("/status")).expect("second");
        assert_eq!(first, second);
    }

    #[test]
    fn approval_callbacks_are_opaque_bound_expiring_and_one_shot() {
        let mut gateway = TelegramGateway::new(config()).expect("gateway");
        let identity = private_identity();
        let now = datetime!(2026-07-30 16:00 UTC);
        let buttons = gateway
            .issue_approval_callbacks(
                &identity,
                "session-1",
                Some("turn-1"),
                "approval-1",
                now + Duration::minutes(5),
                now,
            )
            .expect("callbacks");
        assert_eq!(buttons.len(), 2);
        assert!(buttons[0].callback_data.starts_with(CALLBACK_PREFIX));
        assert!(!buttons[0].callback_data.contains("approval-1"));
        let command = gateway
            .resolve_callback(&identity, &buttons[0].callback_data, now)
            .expect("resolve");
        assert!(matches!(
            command.command,
            FrontendCommand::ResolveApproval {
                decision: ApprovalDecision::ApproveOnce,
                ..
            }
        ));
        assert!(matches!(
            gateway.resolve_callback(&identity, &buttons[0].callback_data, now),
            Err(TelegramGatewayError::CallbackAlreadyResolved)
        ));
    }

    #[test]
    fn renderer_replays_once_and_rejects_cursor_conflicts() {
        let mut renderer = TelegramRenderer::new(TelegramDisplayConfig::default(), 9)
            .expect("renderer");
        let started = event(1, FrontendEvent::Started);
        assert_eq!(renderer.render(&started, started.timestamp).expect("render").len(), 2);
        assert!(renderer
            .render(&started, started.timestamp)
            .expect("idempotent")
            .is_empty());
        let mut conflict = started.clone();
        conflict.event_id = "other-event".to_owned();
        assert!(matches!(
            renderer.render(&conflict, conflict.timestamp),
            Err(TelegramGatewayError::CursorConflict(1))
        ));
    }

    #[test]
    fn renderer_uses_reaction_typing_stream_and_markdown_finalization() {
        let mut renderer = TelegramRenderer::new(TelegramDisplayConfig::default(), 9)
            .expect("renderer");
        renderer
            .render(&event(1, FrontendEvent::Started), datetime!(2026-07-30 16:00 UTC))
            .expect("start");
        let actions = renderer
            .render(
                &event(
                    2,
                    FrontendEvent::AssistantTextDelta {
                        text: "Fixed *two* tests.".to_owned(),
                    },
                ),
                datetime!(2026-07-30 16:00:01 UTC),
            )
            .expect("delta");
        assert!(actions.iter().any(|action| matches!(
            action,
            TelegramAction::UpsertText {
                parse_mode: TelegramParseMode::Plain,
                text,
                ..
            } if text.ends_with(" ▉")
        )));
        let final_actions = renderer
            .render(
                &event(3, FrontendEvent::TurnFinished),
                datetime!(2026-07-30 16:00:02 UTC),
            )
            .expect("finalize");
        assert!(final_actions.iter().any(|action| matches!(
            action,
            TelegramAction::UpsertText {
                parse_mode: TelegramParseMode::MarkdownV2,
                text,
                ..
            } if text.contains("\\*two\\*")
        )));
    }

    #[test]
    fn renderer_maps_typed_activity_and_approval_without_title_heuristics() {
        let mut renderer = TelegramRenderer::new(TelegramDisplayConfig::default(), 9)
            .expect("renderer");
        let actions = renderer
            .render(
                &event(
                    1,
                    FrontendEvent::Activity(PresentationActivity {
                        activity_id: "activity-1".to_owned(),
                        kind: PresentationActivityKind::RepositoryRead,
                        lifecycle: PresentationLifecycle::Active,
                        title: "Reading runtime controller".to_owned(),
                        details: Vec::new(),
                        affected_paths: Vec::new(),
                        evidence_ref: None,
                    }),
                ),
                datetime!(2026-07-30 16:00 UTC),
            )
            .expect("activity");
        assert!(actions.iter().any(|action| matches!(
            action,
            TelegramAction::UpsertText { text, .. } if text.starts_with("🔎")
        )));

        let approval_actions = renderer
            .render(
                &event(
                    2,
                    FrontendEvent::ApprovalRequired(PresentationApproval {
                        approval_id: "approval-1".to_owned(),
                        action: "Run package installation".to_owned(),
                        scope: "repository".to_owned(),
                        reason: "required for tests".to_owned(),
                        risk: "network and dependency mutation".to_owned(),
                        expires_at: datetime!(2026-07-30 16:05 UTC),
                    }),
                ),
                datetime!(2026-07-30 16:00 UTC),
            )
            .expect("approval");
        assert!(approval_actions.iter().any(|action| matches!(
            action,
            TelegramAction::UpsertText { buttons, .. } if buttons.len() == 3
        )));
    }

    #[test]
    fn utf16_splitting_preserves_non_bmp_characters_and_code_fences() {
        assert_eq!(utf16_len("a😀b"), 4);
        let chunks = split_telegram_text("```rust\n😀😀😀😀\n```\nend", 14);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| utf16_len(chunk) <= 14));
        assert!(chunks.concat().replace("```\n```\n", "").contains("😀😀😀😀"));
    }

    #[test]
    fn markdown_specials_and_tables_are_normalized() {
        let rendered = telegram_markdown_v2(
            "| Name | State |\n| --- | --- |\n| Worker | active |\nhello_world!",
        );
        assert!(rendered.contains("• Name: Worker; State: active"));
        assert!(rendered.contains("hello\\_world\\!"));
    }
}
