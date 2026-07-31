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
        TelegramDocument, TelegramInboundCallback, TelegramPhotoSize, TelegramTransportUpdate,
    },
};

const DEFAULT_TRANSIENT_BACKOFF: Duration = Duration::from_secs(2);
const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(60);
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(200);
const MAX_IMAGE_DOWNLOAD_BYTES: u64 = 20 * 1024 * 1024;
const MAX_TEXT_DOWNLOAD_BYTES: u64 = 4 * 1024 * 1024;

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
        let mut normalized = Vec::with_capacity(updates.len());
        for raw_update in updates {
            let update_id = raw_update.update_id;
            match TelegramTransportUpdate::try_from(raw_update) {
                Ok(update) => normalized.push(update),
                Err(error) if update_id >= 0 && is_poison_transport_update(&error) => {
                    self.service.acknowledge_transport_update(update_id)?;
                }
                Err(error) => return Err(error.into()),
            }
        }

        let mut outcomes = Vec::new();
        for unit in group_transport_updates(normalized) {
            match unit {
                TelegramPollingUnit::Messages {
                    update_id,
                    messages,
                } => {
                    let inbound = match self.inbound_batch(&messages) {
                        Ok(inbound) => inbound,
                        Err(error) if error.is_rejected_media() => {
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
                TelegramPollingUnit::Callback {
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
                TelegramPollingUnit::Unsupported { update_id } => {
                    self.service.acknowledge_transport_update(update_id)?;
                }
            }
        }
        self.service
            .deliver_pending(&self.client, OffsetDateTime::now_utc())?;
        Ok(outcomes)
    }

    fn inbound_batch(
        &self,
        messages: &[TelegramBotMessage],
    ) -> Result<TelegramInboundMessage, TelegramRuntimeError> {
        let mut attachment_ids = Vec::new();
        for message in messages {
            if let Some(photo) = largest_photo(&message.photo) {
                attachment_ids.push(self.stage_file(
                    &photo.file_id,
                    photo.file_size,
                    format!(
                        "telegram-photo-{}.jpg",
                        safe_identifier(&photo.file_unique_id)
                    ),
                    Some("image/jpeg".to_owned()),
                    MAX_IMAGE_DOWNLOAD_BYTES,
                )?);
            }
            if let Some(document) = &message.document {
                let (display_name, mime_type, limit) = document_metadata(document)?;
                attachment_ids.push(self.stage_file(
                    &document.file_id,
                    document.file_size,
                    display_name,
                    mime_type,
                    limit,
                )?);
            }
        }
        normalize_message_batch(messages, &self.config.bot_username, attachment_ids)
    }

    fn stage_file(
        &self,
        file_id: &str,
        declared_size: Option<u64>,
        display_name: String,
        mime_type: Option<String>,
        limit: u64,
    ) -> Result<String, TelegramRuntimeError> {
        if declared_size.is_some_and(|size| size > limit) {
            return Err(TelegramRuntimeError::RejectedMedia(format!(
                "Telegram attachment exceeds the {limit}-byte limit"
            )));
        }
        let file = self.client.get_file(file_id).map_err(media_api_error)?;
        if file.file_size.is_some_and(|size| size > limit) {
            return Err(TelegramRuntimeError::RejectedMedia(format!(
                "Telegram attachment exceeds the {limit}-byte limit"
            )));
        }
        let file_path = file.file_path.ok_or_else(|| {
            TelegramRuntimeError::RejectedMedia(
                "Telegram did not provide a downloadable file path".to_owned(),
            )
        })?;
        let bytes = self
            .client
            .download_file(&file_path, limit)
            .map_err(media_api_error)?;
        if !mime_type
            .as_deref()
            .is_some_and(|value| matches!(value, "image/jpeg" | "image/png" | "image/webp"))
            && std::str::from_utf8(&bytes).is_err()
        {
            return Err(TelegramRuntimeError::RejectedMedia(
                "Telegram document is not UTF-8 text".to_owned(),
            ));
        }
        self.service
            .ingest_attachment(display_name, mime_type, bytes)
            .map_err(Into::into)
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

#[derive(Clone, Debug, Eq, PartialEq)]
enum TelegramPollingUnit {
    Messages {
        update_id: i64,
        messages: Vec<TelegramBotMessage>,
    },
    Callback {
        update_id: i64,
        callback: TelegramInboundCallback,
    },
    Unsupported {
        update_id: i64,
    },
}

fn group_transport_updates(updates: Vec<TelegramTransportUpdate>) -> Vec<TelegramPollingUnit> {
    let mut units = Vec::with_capacity(updates.len());
    for update in updates {
        match update {
            TelegramTransportUpdate::Message { update_id, message } => {
                if let Some(TelegramPollingUnit::Messages {
                    update_id: current_update_id,
                    messages,
                }) = units.last_mut()
                    && same_media_group(messages.last(), &message)
                {
                    *current_update_id = (*current_update_id).max(update_id);
                    messages.push(message);
                    continue;
                }
                units.push(TelegramPollingUnit::Messages {
                    update_id,
                    messages: vec![message],
                });
            }
            TelegramTransportUpdate::Callback {
                update_id,
                callback,
            } => units.push(TelegramPollingUnit::Callback {
                update_id,
                callback,
            }),
            TelegramTransportUpdate::Unsupported { update_id } => {
                units.push(TelegramPollingUnit::Unsupported { update_id });
            }
        }
    }
    units
}

fn same_media_group(current: Option<&TelegramBotMessage>, next: &TelegramBotMessage) -> bool {
    let Some(current) = current else {
        return false;
    };
    current.media_group_id.is_some()
        && current.media_group_id == next.media_group_id
        && current.chat.id == next.chat.id
        && current.message_thread_id == next.message_thread_id
        && current.from.as_ref().map(|user| user.id) == next.from.as_ref().map(|user| user.id)
}

fn normalize_message_batch(
    messages: &[TelegramBotMessage],
    bot_username: &str,
    attachment_ids: Vec<String>,
) -> Result<TelegramInboundMessage, TelegramRuntimeError> {
    let first = messages
        .first()
        .ok_or(TelegramRuntimeError::InvalidUpdate)?;
    let user = first
        .from
        .as_ref()
        .ok_or(TelegramRuntimeError::InvalidUpdate)?;
    if user.is_bot
        || messages.iter().any(|message| {
            message.chat.id != first.chat.id
                || message.chat.kind != first.chat.kind
                || message.message_thread_id != first.message_thread_id
                || message.from.as_ref().map(|candidate| candidate.id) != Some(user.id)
                || (messages.len() > 1 && message.media_group_id != first.media_group_id)
        })
    {
        return Err(TelegramRuntimeError::InvalidUpdate);
    }
    let mut parts = Vec::new();
    for message in messages {
        if let Some(text) = message.text.as_deref().or(message.caption.as_deref()) {
            let trimmed = text.trim();
            if !trimmed.is_empty() && !parts.iter().any(|part| part == trimmed) {
                parts.push(trimmed.to_owned());
            }
        }
    }
    let text = parts.join("\n\n");
    let chat_kind = chat_kind(first.chat.kind);
    let bot_mentioned =
        matches!(chat_kind, TelegramChatKind::Private) || mentions_bot(&text, bot_username);
    let received_at = OffsetDateTime::from_unix_timestamp(first.date)
        .map_err(|_| TelegramRuntimeError::InvalidUpdate)?;
    Ok(TelegramInboundMessage {
        identity: TelegramIdentity {
            user_id: user.id,
            chat_id: first.chat.id,
            topic_id: first.message_thread_id,
            chat_kind,
            bot_mentioned,
        },
        message_id: first.message_id,
        text,
        attachment_ids,
        attached_session_id: None,
        received_at,
    })
}

fn largest_photo(photos: &[TelegramPhotoSize]) -> Option<&TelegramPhotoSize> {
    photos.iter().max_by_key(|photo| {
        (
            u64::from(photo.width).saturating_mul(u64::from(photo.height)),
            photo.file_size.unwrap_or_default(),
        )
    })
}

fn document_metadata(
    document: &TelegramDocument,
) -> Result<(String, Option<String>, u64), TelegramRuntimeError> {
    let display_name = sanitize_display_name(
        document.file_name.as_deref(),
        &format!(
            "telegram-document-{}",
            safe_identifier(&document.file_unique_id)
        ),
    );
    let mime_type = document
        .mime_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .filter(|value| value != "application/octet-stream")
        .or_else(|| mime_from_file_name(&display_name).map(str::to_owned));
    let limit = match mime_type.as_deref() {
        Some("image/jpeg" | "image/png" | "image/webp") => MAX_IMAGE_DOWNLOAD_BYTES,
        Some(value) if is_text_mime(value) => MAX_TEXT_DOWNLOAD_BYTES,
        None => MAX_TEXT_DOWNLOAD_BYTES,
        Some(value) => {
            return Err(TelegramRuntimeError::RejectedMedia(format!(
                "Telegram document type {value} is not supported"
            )));
        }
    };
    Ok((display_name, mime_type, limit))
}

fn mime_from_file_name(file_name: &str) -> Option<&'static str> {
    let extension = file_name.rsplit_once('.')?.1.to_ascii_lowercase();
    match extension.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "webp" => Some("image/webp"),
        "txt" | "md" | "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "css" | "html" | "toml"
        | "yaml" | "yml" | "json" | "xml" | "csv" | "log" => Some("text/plain"),
        _ => None,
    }
}

fn is_text_mime(value: &str) -> bool {
    value.starts_with("text/")
        || matches!(
            value,
            "application/json"
                | "application/xml"
                | "application/toml"
                | "application/yaml"
                | "application/x-yaml"
                | "application/javascript"
        )
}

fn sanitize_display_name(value: Option<&str>, fallback: &str) -> String {
    let candidate = value.unwrap_or(fallback).trim();
    let sanitized = candidate
        .chars()
        .map(|character| {
            if character.is_control() || matches!(character, '/' | '\\') {
                '_'
            } else {
                character
            }
        })
        .take(200)
        .collect::<String>();
    if sanitized.trim().is_empty() {
        fallback.to_owned()
    } else {
        sanitized
    }
}

fn safe_identifier(value: &str) -> String {
    let value = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .take(80)
        .collect::<String>();
    if value.is_empty() {
        "file".to_owned()
    } else {
        value
    }
}

fn media_api_error(error: TelegramBotApiError) -> TelegramRuntimeError {
    match error {
        TelegramBotApiError::FileTooLarge { .. } | TelegramBotApiError::InvalidRequest(_) => {
            TelegramRuntimeError::RejectedMedia(error.to_string())
        }
        error => TelegramRuntimeError::BotApi(error),
    }
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
                | TelegramGatewayError::AttachmentsNotAllowedForCommand
                | TelegramGatewayError::InvalidCallbackRequest
                | TelegramGatewayError::InvalidCallback
                | TelegramGatewayError::CallbackIdentityMismatch
                | TelegramGatewayError::CallbackExpired
                | TelegramGatewayError::CallbackAlreadyResolved
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
    #[error("Telegram media was rejected: {0}")]
    RejectedMedia(String),
    #[error(transparent)]
    BotApi(#[from] TelegramBotApiError),
    #[error(transparent)]
    Service(#[from] TelegramSessionServiceError),
}

impl TelegramRuntimeError {
    fn is_rejected_media(&self) -> bool {
        matches!(self, Self::InvalidUpdate | Self::RejectedMedia(_))
    }

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
            media_group_id: None,
            photo: Vec::new(),
            document: None,
            text: Some(text.to_owned()),
            caption: None,
        }
    }

    #[test]
    fn private_messages_are_normalized_without_requiring_a_mention() {
        let source = message(TelegramBotChatKind::Private, "status");
        let inbound =
            normalize_message_batch(&[source], "medusa_bot", Vec::new()).expect("normalize");
        assert!(inbound.identity.bot_mentioned);
        assert_eq!(inbound.identity.topic_id, Some(9));
    }

    #[test]
    fn group_mentions_are_exact_and_case_insensitive() {
        let source = message(TelegramBotChatKind::Supergroup, "Hello (@Medusa_Bot),");
        let inbound =
            normalize_message_batch(&[source], "medusa_bot", Vec::new()).expect("normalize");
        assert!(inbound.identity.bot_mentioned);
        assert!(!mentions_bot("hello @medusa_bot_extra", "medusa_bot"));
    }

    #[test]
    fn bot_messages_are_rejected_to_prevent_feedback_loops() {
        let mut source = message(TelegramBotChatKind::Private, "status");
        source.from.as_mut().expect("sender").is_bot = true;
        assert!(matches!(
            normalize_message_batch(&[source], "medusa_bot", Vec::new()),
            Err(TelegramRuntimeError::InvalidUpdate)
        ));
    }

    #[test]
    fn protocol_failures_are_not_acknowledged_as_rejected_input() {
        let unauthorized = TelegramSessionServiceError::Gateway(TelegramGatewayError::Unauthorized);
        assert!(is_rejected_input(&unauthorized));

        let protocol = TelegramSessionServiceError::Gateway(TelegramGatewayError::Protocol(
            "invalid envelope".to_owned(),
        ));
        assert!(!is_rejected_input(&protocol));
    }

    #[test]
    fn contiguous_album_updates_are_batched_once() {
        let mut first = message(TelegramBotChatKind::Private, "album caption");
        first.media_group_id = Some("album-1".to_owned());
        first.photo.push(TelegramPhotoSize {
            file_id: "file-1".to_owned(),
            file_unique_id: "unique-1".to_owned(),
            width: 10,
            height: 10,
            file_size: Some(100),
        });
        let mut second = message(TelegramBotChatKind::Private, "");
        second.message_id = 8;
        second.media_group_id = Some("album-1".to_owned());
        second.photo.push(TelegramPhotoSize {
            file_id: "file-2".to_owned(),
            file_unique_id: "unique-2".to_owned(),
            width: 20,
            height: 20,
            file_size: Some(200),
        });
        let grouped = group_transport_updates(vec![
            TelegramTransportUpdate::Message {
                update_id: 11,
                message: first,
            },
            TelegramTransportUpdate::Message {
                update_id: 12,
                message: second,
            },
        ]);
        assert!(matches!(
            grouped.as_slice(),
            [TelegramPollingUnit::Messages { update_id: 12, messages }] if messages.len() == 2
        ));
    }

    #[test]
    fn attachment_batch_preserves_caption_and_ids() {
        let mut first = message(TelegramBotChatKind::Private, "inspect these");
        first.media_group_id = Some("album-1".to_owned());
        let mut second = message(TelegramBotChatKind::Private, "");
        second.media_group_id = Some("album-1".to_owned());
        let inbound = normalize_message_batch(
            &[first, second],
            "medusa_bot",
            vec!["artifact-1".to_owned(), "artifact-2".to_owned()],
        )
        .expect("normalize album");
        assert_eq!(inbound.text, "inspect these");
        assert_eq!(inbound.attachment_ids.len(), 2);
    }

    #[test]
    fn unsafe_document_names_are_sanitized() {
        assert_eq!(
            sanitize_display_name(Some("../secret.rs"), "fallback"),
            ".._secret.rs"
        );
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
