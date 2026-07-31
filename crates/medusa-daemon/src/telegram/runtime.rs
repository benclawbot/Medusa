//! Supervised Telegram polling runtime over the authoritative daemon control plane.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use time::OffsetDateTime;

use super::{
    TelegramChatKind, TelegramGatewayError, TelegramIdentity, TelegramInboundMessage,
    TelegramServiceOutcome, TelegramSessionService, TelegramSessionServiceError,
    bot_api::{
        TelegramBotApiClient, TelegramBotApiError, TelegramBotChatKind, TelegramBotMessage,
        TelegramInboundCallback, TelegramTransportUpdate,
    },
};

const DEFAULT_TRANSIENT_BACKOFF: Duration = Duration::from_secs(2);
const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(60);
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelegramPollingConfig {
    pub bot_username: String,
    pub timeout_seconds: u16,
    pub limit: u8,
}

impl TelegramPollingConfig {
    pub fn validate(&self) -> Result<(), TelegramRuntimeError> {
        let username = self.bot_username.trim().trim_start_matches('@');
        if username.is_empty()
            || username.len() > 32
            || !username
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || !(1..=50).contains(&self.timeout_seconds)
            || !(1..=100).contains(&self.limit)
        {
            return Err(TelegramRuntimeError::InvalidConfiguration);
        }
        Ok(())
    }
}

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
        Ok(Self {
            client,
            service,
            config,
        })
    }

    #[must_use]
    pub const fn service(&self) -> &TelegramSessionService {
        &self.service
    }

    #[must_use]
    pub const fn service_mut(&mut self) -> &mut TelegramSessionService {
        &mut self.service
    }

    pub fn poll_once(&mut self) -> Result<Vec<TelegramServiceOutcome>, TelegramRuntimeError> {
        let updates = self.client.get_updates(
            self.service.next_update_offset(),
            self.config.timeout_seconds,
            self.config.limit,
        )?;
        let mut outcomes = Vec::new();
        for raw_update in updates {
            let update_id = raw_update.update_id;
            let update = match TelegramTransportUpdate::try_from(raw_update) {
                Ok(update) => update,
                Err(error) if update_id >= 0 && is_poison_transport_update(&error) => {
                    self.service.acknowledge_transport_update(update_id)?;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            match update {
                TelegramTransportUpdate::Message { update_id, message } => {
                    let inbound = match inbound_message(message, &self.config.bot_username) {
                        Ok(inbound) => inbound,
                        Err(TelegramRuntimeError::InvalidUpdate) => {
                            self.service.acknowledge_transport_update(update_id)?;
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    match self.service.process_message(update_id, inbound) {
                        Ok(outcome) => outcomes.push(outcome),
                        Err(error) if is_rejected_input(&error) => {
                            self.service.acknowledge_transport_update(update_id)?;
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                TelegramTransportUpdate::Callback {
                    update_id,
                    callback,
                } => {
                    let identity = callback_identity(&callback);
                    match self.service.process_callback(
                        update_id,
                        identity,
                        &callback.data,
                        OffsetDateTime::now_utc(),
                    ) {
                        Ok(_) => {
                            self.client
                                .answer_callback_query(&callback.query_id, None)?;
                        }
                        Err(error) if is_rejected_input(&error) => {
                            self.service.acknowledge_transport_update(update_id)?;
                            self.client.answer_callback_query(
                                &callback.query_id,
                                Some("Request rejected"),
                            )?;
                        }
                        Err(error) => return Err(error.into()),
                    }
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
                    if sleep_until_cancelled(cancelled, error.retry_delay(consecutive_failures)) {
                        break;
                    }
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
    if user.is_bot {
        return Err(TelegramRuntimeError::InvalidUpdate);
    }
    let text = message.text.or(message.caption).unwrap_or_default();
    let chat_kind = chat_kind(message.chat.kind);
    let bot_mentioned =
        matches!(chat_kind, TelegramChatKind::Private) || mentions_bot(&text, bot_username);
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
    text.split_whitespace().any(|token| {
        let token = token.trim_matches(|character: char| {
            !character.is_ascii_alphanumeric() && character != '_' && character != '@'
        });
        token
            .strip_prefix('@')
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(username))
    })
}

fn is_poison_transport_update(error: &TelegramBotApiError) -> bool {
    matches!(
        error,
        TelegramBotApiError::InvalidTimestamp | TelegramBotApiError::InvalidUpdate
    )
}

fn is_rejected_input(error: &TelegramSessionServiceError) -> bool {
    matches!(
        error,
        TelegramSessionServiceError::Gateway(
            TelegramGatewayError::Disabled
                | TelegramGatewayError::Unauthorized
                | TelegramGatewayError::MentionRequired
                | TelegramGatewayError::EmptyMessage
                | TelegramGatewayError::UnknownCommand(_)
                | TelegramGatewayError::MissingArgument(_)
                | TelegramGatewayError::InvalidCallbackRequest
                | TelegramGatewayError::InvalidCallback
                | TelegramGatewayError::CallbackIdentityMismatch
                | TelegramGatewayError::CallbackExpired
                | TelegramGatewayError::CallbackAlreadyResolved
                | TelegramGatewayError::Protocol(_)
        )
    )
}

fn sleep_until_cancelled(cancelled: &AtomicBool, duration: Duration) -> bool {
    let mut remaining = duration;
    while !remaining.is_zero() {
        if cancelled.load(Ordering::Acquire) {
            return true;
        }
        let interval = remaining.min(CANCELLATION_POLL_INTERVAL);
        thread::sleep(interval);
        remaining = remaining.saturating_sub(interval);
    }
    cancelled.load(Ordering::Acquire)
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
            || matches!(
                self,
                Self::Service(TelegramSessionServiceError::BotApi(error))
                    if error.is_transient()
            )
    }

    fn retry_delay(&self, consecutive_failures: u32) -> Duration {
        let retry_after = match self {
            Self::BotApi(error) => error.retry_after_seconds(),
            Self::Service(TelegramSessionServiceError::BotApi(error)) => {
                error.retry_after_seconds()
            }
            _ => None,
        };
        if let Some(seconds) = retry_after {
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
                id: if kind == TelegramBotChatKind::Private {
                    42
                } else {
                    -100
                },
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
        let inbound = inbound_message(
            message(TelegramBotChatKind::Private, "status"),
            "medusa_bot",
        )
        .expect("normalize");
        assert!(inbound.identity.bot_mentioned);
        assert_eq!(inbound.identity.topic_id, Some(9));
    }

    #[test]
    fn group_mentions_are_exact_and_case_insensitive() {
        let inbound = inbound_message(
            message(TelegramBotChatKind::Supergroup, "Hello (@Medusa_Bot),"),
            "medusa_bot",
        )
        .expect("normalize");
        assert!(inbound.identity.bot_mentioned);
        assert!(!mentions_bot("hello @medusa_bot_extra", "medusa_bot"));
    }

    #[test]
    fn bot_messages_are_rejected_to_prevent_feedback_loops() {
        let mut source = message(TelegramBotChatKind::Private, "status");
        source.from.as_mut().expect("sender").is_bot = true;
        assert!(matches!(
            inbound_message(source, "medusa_bot"),
            Err(TelegramRuntimeError::InvalidUpdate)
        ));
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

    #[test]
    fn cancellation_interrupts_retry_sleep() {
        let cancelled = AtomicBool::new(true);
        assert!(sleep_until_cancelled(&cancelled, MAX_RETRY_BACKOFF));
    }
}
