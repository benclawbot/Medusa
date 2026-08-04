//! Telegram transport policy, command mapping, callback safety, deterministic rendering, and supervised polling.
//!
//! The gateway remains a frontend adapter to the authoritative live-session broker. It does not own
//! an agent, repository policy, or approval execution path.

pub mod bot_api;
mod callback;
mod command;
mod config;
mod control;
mod delivery;
mod format;
mod mini_app;
mod mini_app_http;
mod projection;
mod render;
mod runtime;
mod service;
mod supervisor;
mod text_fragments;
mod voice;
mod webhook;

use medusa_protocol::frontend::FrontendCommandEnvelope;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

pub use callback::TelegramInlineButton;
pub use command::{TelegramInboundAction, TelegramInboundMessage};
pub use config::{
    TelegramChatKind, TelegramConfig, TelegramDisplayConfig, TelegramIdentity, TelegramTransport,
    TelegramVoiceConfig, TelegramVoiceMode, ToolProgressMode,
};
pub use control::{TelegramControl, TelegramControlError};
pub use delivery::TelegramDeliveryState;
pub use format::{normalize_markdown_tables, split_telegram_text, telegram_markdown_v2, utf16_len};
pub use mini_app::{
    TelegramMiniAppAuthToken, TelegramMiniAppBinding, TelegramMiniAppBridge, TelegramMiniAppError,
    TelegramMiniAppLaunchTicket, TelegramMiniAppRealtimeSession, TelegramMiniAppSecret,
    TelegramMiniAppUser, VerifiedMiniAppIdentity,
};
pub use mini_app_http::{
    TelegramMiniAppCommand, TelegramMiniAppHttpConfig, TelegramMiniAppHttpError,
    TelegramMiniAppHttpServer,
};
pub use projection::project_event;
pub use render::{
    TelegramAction, TelegramButtonIntent, TelegramMessageSlot, TelegramParseMode, TelegramReaction,
    TelegramRenderButton, TelegramRenderer,
};
pub use runtime::{TelegramPollingConfig, TelegramPollingRuntime, TelegramRuntimeError};
pub use service::{
    TelegramBindingKey, TelegramServiceOutcome, TelegramSessionBinding, TelegramSessionService,
    TelegramSessionServiceError,
};
pub use supervisor::{TelegramServiceMode, TelegramServiceSupervisor, TelegramSupervisorError};
pub use voice::{
    OpenAiAudioToken, TelegramSynthesizedVoice, TelegramVoiceError, TelegramVoiceInput,
    TelegramVoicePipeline,
};
pub use webhook::{TelegramWebhookConfig, TelegramWebhookError, TelegramWebhookServer};

use callback::CallbackStore;

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

    pub fn authorize(&self, identity: &TelegramIdentity) -> Result<(), TelegramGatewayError> {
        self.config.authorize(identity)
    }

    pub fn map_message(
        &self,
        message: &TelegramInboundMessage,
    ) -> Result<TelegramInboundAction, TelegramGatewayError> {
        command::map_message(&self.config, message)
    }

    pub fn issue_approval_callbacks(
        &mut self,
        identity: &TelegramIdentity,
        session_id: &str,
        turn_id: Option<&str>,
        approval_id: &str,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<Vec<TelegramInlineButton>, TelegramGatewayError> {
        self.config.authorize(identity)?;
        self.callbacks
            .issue_approval(identity, session_id, turn_id, approval_id, expires_at, now)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn issue_command_callback(
        &mut self,
        identity: &TelegramIdentity,
        session_id: &str,
        turn_id: Option<&str>,
        group_id: &str,
        label: &str,
        command: medusa_protocol::frontend::FrontendCommand,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<TelegramInlineButton, TelegramGatewayError> {
        self.config.authorize(identity)?;
        self.callbacks.issue_command(
            identity, session_id, turn_id, group_id, label, command, expires_at, now,
        )
    }

    pub(crate) fn callback_snapshot(&self) -> CallbackStore {
        self.callbacks.clone()
    }

    pub(crate) fn restore_callbacks(&mut self, callbacks: CallbackStore) {
        self.callbacks = callbacks;
    }

    pub fn resolve_callback(
        &mut self,
        identity: &TelegramIdentity,
        callback_data: &str,
        now: OffsetDateTime,
    ) -> Result<FrontendCommandEnvelope, TelegramGatewayError> {
        self.config.authorize(identity)?;
        self.callbacks.resolve(identity, callback_data, now)
    }
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
    #[error("Telegram attachments cannot be combined with a slash command")]
    AttachmentsNotAllowedForCommand,
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
    #[error("assistant output requires too many Telegram message chunks")]
    TooManyMessageChunks,
}
