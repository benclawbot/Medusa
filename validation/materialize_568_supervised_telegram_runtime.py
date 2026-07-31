from pathlib import Path

root = Path(__file__).resolve().parents[1]
telegram = root / "crates/medusa-daemon/src/telegram"

runtime = r'''//! Supervised Telegram polling runtime over the authoritative daemon control plane.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use time::OffsetDateTime;

use super::{
    TelegramChatKind, TelegramIdentity, TelegramInboundMessage, TelegramServiceOutcome,
    TelegramSessionService, TelegramSessionServiceError,
    bot_api::{
        TelegramBotApiClient, TelegramBotApiError, TelegramBotChatKind, TelegramBotMessage,
        TelegramInboundCallback, TelegramTransportUpdate,
    },
};

const DEFAULT_TRANSIENT_BACKOFF: Duration = Duration::from_secs(2);
const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelegramPollingConfig {
    pub bot_username: String,
    pub timeout_seconds: u16,
    pub limit: u8,
}

impl TelegramPollingConfig {
    pub fn validate(&self) -> Result<(), TelegramRuntimeError> {
        if self.bot_username.trim().is_empty()
            || !(1..=50).contains(&self.timeout_seconds)
            || !(1..=100).contains(&self.limit)
        {
            return Err(TelegramRuntimeError::InvalidConfiguration);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct TelegramPollingRuntime {
    client: TelegramBotApiClient,
    service: TelegramSessionService,
    config: TelegramPollingConfig,
}

impl TelegramPollingRuntime {
    pub fn new(
        client: TelegramBotApiClient,
        service: TelegramSessionService,
        config: TelegramPollingConfig,
    ) -> Result<Self, TelegramRuntimeError> {
        config.validate()?;
        Ok(Self { client, service, config })
    }

    #[must_use]
    pub const fn service(&self) -> &TelegramSessionService {
        &self.service
    }

    pub fn poll_once(&mut self) -> Result<Vec<TelegramServiceOutcome>, TelegramRuntimeError> {
        let updates = self.client.get_updates(
            self.service.next_update_offset(),
            self.config.timeout_seconds,
            self.config.limit,
        )?;
        let mut outcomes = Vec::new();
        for update in updates {
            match TelegramTransportUpdate::try_from(update)? {
                TelegramTransportUpdate::Message { update_id, message } => {
                    let inbound = inbound_message(message, &self.config.bot_username)?;
                    outcomes.push(self.service.process_message(update_id, inbound)?);
                }
                TelegramTransportUpdate::Callback { update_id, callback } => {
                    let identity = callback_identity(&callback);
                    self.service.process_callback(
                        update_id,
                        identity,
                        &callback.data,
                        OffsetDateTime::now_utc(),
                    )?;
                    self.client
                        .answer_callback_query(&callback.query_id, None)?;
                }
                TelegramTransportUpdate::Unsupported { update_id } => {
                    self.service.acknowledge_transport_update(update_id)?;
                }
            }
        }
        Ok(outcomes)
    }

    pub fn run_until_cancelled(
        &mut self,
        cancelled: &AtomicBool,
    ) -> Result<(), TelegramRuntimeError> {
        let mut consecutive_failures = 0_u32;
        while !cancelled.load(Ordering::Acquire) {
            match self.poll_once() {
                Ok(_) => consecutive_failures = 0,
                Err(error) if error.is_transient() => {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    thread::sleep(error.retry_delay(consecutive_failures));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

fn inbound_message(
    message: TelegramBotMessage,
    bot_username: &str,
) -> Result<TelegramInboundMessage, TelegramRuntimeError> {
    let user = message.from.ok_or(TelegramRuntimeError::InvalidUpdate)?;
    let text = message.text.or(message.caption).unwrap_or_default();
    let chat_kind = chat_kind(message.chat.kind);
    let bot_mentioned = matches!(chat_kind, TelegramChatKind::Private)
        || mentions_bot(&text, bot_username);
    let received_at = OffsetDateTime::from_unix_timestamp(message.date)
        .map_err(|_| TelegramRuntimeError::InvalidUpdate)?;
    Ok(TelegramInboundMessage {
        identity: TelegramIdentity {
            user_id: user.id,
            chat_id: message.chat.id,
            topic_id: message.message_thread_id,
            chat_kind,
            bot_mentioned,
        },
        message_id: message.message_id,
        text,
        attached_session_id: None,
        received_at,
    })
}

fn callback_identity(callback: &TelegramInboundCallback) -> TelegramIdentity {
    TelegramIdentity {
        user_id: callback.user.id,
        chat_id: callback.chat.id,
        topic_id: callback.message_thread_id,
        chat_kind: chat_kind(callback.chat.kind),
        bot_mentioned: true,
    }
}

const fn chat_kind(kind: TelegramBotChatKind) -> TelegramChatKind {
    match kind {
        TelegramBotChatKind::Private => TelegramChatKind::Private,
        TelegramBotChatKind::Group => TelegramChatKind::Group,
        TelegramBotChatKind::Supergroup => TelegramChatKind::Supergroup,
        TelegramBotChatKind::Channel => TelegramChatKind::Channel,
    }
}

fn mentions_bot(text: &str, bot_username: &str) -> bool {
    let username = bot_username.trim().trim_start_matches('@');
    let mention = format!("@{}", username.to_ascii_lowercase());
    text.to_ascii_lowercase()
        .split(|character: char| character.is_whitespace() || character.is_ascii_punctuation())
        .any(|part| part == mention.trim_start_matches('@'))
        || text.to_ascii_lowercase().contains(&mention)
}

#[derive(Debug, thiserror::Error)]
pub enum TelegramRuntimeError {
    #[error("invalid Telegram polling configuration")]
    InvalidConfiguration,
    #[error("Telegram transport update is invalid")]
    InvalidUpdate,
    #[error(transparent)]
    BotApi(#[from] TelegramBotApiError),
    #[error(transparent)]
    Service(#[from] TelegramSessionServiceError),
}

impl TelegramRuntimeError {
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::BotApi(error) if error.is_transient())
    }

    fn retry_delay(&self, consecutive_failures: u32) -> Duration {
        if let Self::BotApi(error) = self
            && let Some(seconds) = error.retry_after_seconds()
        {
            return Duration::from_secs(seconds.min(MAX_RETRY_BACKOFF.as_secs()));
        }
        let exponent = consecutive_failures.saturating_sub(1).min(5);
        DEFAULT_TRANSIENT_BACKOFF
            .saturating_mul(2_u32.saturating_pow(exponent))
            .min(MAX_RETRY_BACKOFF)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::bot_api::{TelegramBotChat, TelegramBotUser};

    fn message(kind: TelegramBotChatKind, text: &str) -> TelegramBotMessage {
        TelegramBotMessage {
            message_id: 7,
            date: 1_700_000_000,
            chat: TelegramBotChat {
                id: if kind == TelegramBotChatKind::Private { 42 } else { -100 },
                kind,
                title: None,
                username: None,
                is_forum: false,
            },
            from: Some(TelegramBotUser {
                id: 42,
                is_bot: false,
                first_name: "Ada".to_owned(),
                username: None,
            }),
            message_thread_id: Some(9),
            text: Some(text.to_owned()),
            caption: None,
        }
    }

    #[test]
    fn private_messages_are_normalized_without_requiring_a_mention() {
        let inbound = inbound_message(message(TelegramBotChatKind::Private, "status"), "medusa_bot")
            .expect("normalize");
        assert!(inbound.identity.bot_mentioned);
        assert_eq!(inbound.identity.topic_id, Some(9));
    }

    #[test]
    fn group_mentions_are_detected_case_insensitively() {
        let inbound = inbound_message(
            message(TelegramBotChatKind::Supergroup, "Hello @Medusa_Bot"),
            "medusa_bot",
        )
        .expect("normalize");
        assert!(inbound.identity.bot_mentioned);
    }

    #[test]
    fn retry_backoff_is_bounded() {
        let error = TelegramRuntimeError::BotApi(TelegramBotApiError::Transport {
            kind: super::super::bot_api::TelegramTransportFailure::Timeout,
            status: None,
        });
        assert!(error.is_transient());
        assert_eq!(error.retry_delay(100), MAX_RETRY_BACKOFF);
    }
}
'''
(telegram / "runtime.rs").write_text(runtime)

mod_path = telegram / "mod.rs"
source = mod_path.read_text()
source = source.replace("mod render;\nmod service;", "mod render;\nmod runtime;\nmod service;")
source = source.replace(
    "pub use render::{\n",
    "pub use runtime::{TelegramPollingConfig, TelegramPollingRuntime, TelegramRuntimeError};\npub use render::{\n",
)
mod_path.write_text(source)

service_path = telegram / "service.rs"
source = service_path.read_text()
marker = "    fn binding_after_acknowledgement(\n"
insert = r'''    /// Resolves one signed callback through the same frontend control plane and durable binding.
    pub fn process_callback(
        &mut self,
        update_id: i64,
        identity: TelegramIdentity,
        callback_data: &str,
        received_at: time::OffsetDateTime,
    ) -> Result<FrontendCommandAcknowledgement, TelegramSessionServiceError> {
        if update_id < 0 {
            return Err(TelegramSessionServiceError::InvalidUpdateOffset);
        }
        let key = TelegramBindingKey::from_identity(&identity);
        let stable_id = key.stable_id();
        let existing = self.state.bindings.get(&stable_id).cloned();
        let envelope = self
            .gateway
            .resolve_callback(&identity, callback_data, received_at)?;
        let command = envelope.command.clone();
        let acknowledgement = self.control.dispatch(envelope)?;
        if let Some(binding) = self.binding_after_acknowledgement(
            key,
            existing,
            update_id,
            &command,
            &acknowledgement,
        )? {
            self.state.bindings.insert(stable_id, binding);
        }
        self.acknowledge_update(update_id)?;
        self.persist()?;
        Ok(acknowledgement)
    }

    /// Advances the durable Bot API cursor for an unsupported but valid update.
    pub fn acknowledge_transport_update(
        &mut self,
        update_id: i64,
    ) -> Result<(), TelegramSessionServiceError> {
        self.acknowledge_update(update_id)?;
        self.persist()
    }

'''
if marker not in source:
    raise SystemExit("service insertion marker missing")
source = source.replace(marker, insert + marker, 1)
service_path.write_text(source)
