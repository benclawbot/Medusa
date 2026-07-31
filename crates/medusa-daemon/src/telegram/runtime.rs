//! Supervised Telegram polling runtime over the authoritative daemon control plane.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
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
const MEDIA_GROUP_QUIET_PERIOD_SECONDS: i64 = 2;
const MAX_PENDING_MEDIA_GROUPS: usize = 256;
const MAX_MEDIA_GROUP_MESSAGES: usize = 10;
const MEDIA_GROUP_SCHEMA_VERSION: u32 = 1;
const MAX_IMAGE_DOWNLOAD_BYTES: u64 = 20 * 1024 * 1024;
const MAX_TEXT_DOWNLOAD_BYTES: u64 = 4 * 1024 * 1024;
const MEDIA_GROUP_STATE_ENV: &str = "MEDUSA_TELEGRAM_MEDIA_GROUP_STATE_PATH";
const DEFAULT_MEDIA_GROUP_STATE_PATH: &str = ".medusa/telegram-media-groups.json";

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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingMediaGroup {
    highest_update_id: i64,
    updated_at_unix: i64,
    messages: Vec<TelegramBotMessage>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingMediaGroupState {
    schema_version: u32,
    groups: BTreeMap<String, PendingMediaGroup>,
}

impl Default for PendingMediaGroupState {
    fn default() -> Self {
        Self {
            schema_version: MEDIA_GROUP_SCHEMA_VERSION,
            groups: BTreeMap::new(),
        }
    }
}

impl PendingMediaGroupState {
    fn validate(&self) -> Result<(), TelegramRuntimeError> {
        if self.schema_version != MEDIA_GROUP_SCHEMA_VERSION
            || self.groups.len() > MAX_PENDING_MEDIA_GROUPS
            || self.groups.values().any(|group| {
                group.highest_update_id < 0
                    || group.updated_at_unix < 0
                    || group.messages.is_empty()
                    || group.messages.len() > MAX_MEDIA_GROUP_MESSAGES
                    || group
                        .messages
                        .iter()
                        .any(|message| message.media_group_id.is_none())
            })
        {
            return Err(TelegramRuntimeError::InvalidMediaGroupState);
        }
        Ok(())
    }

    fn insert(
        &mut self,
        update_id: i64,
        message: TelegramBotMessage,
        now: OffsetDateTime,
    ) -> Result<(), TelegramRuntimeError> {
        let key = media_group_key(&message).ok_or(TelegramRuntimeError::InvalidUpdate)?;
        if !self.groups.contains_key(&key) && self.groups.len() >= MAX_PENDING_MEDIA_GROUPS {
            return Err(TelegramRuntimeError::TooManyPendingMediaGroups);
        }
        let group = self.groups.entry(key).or_insert_with(|| PendingMediaGroup {
            highest_update_id: update_id,
            updated_at_unix: now.unix_timestamp(),
            messages: Vec::new(),
        });
        if let Some(first) = group.messages.first()
            && !same_media_group(Some(first), &message)
        {
            return Err(TelegramRuntimeError::InvalidUpdate);
        }
        if !group
            .messages
            .iter()
            .any(|candidate| candidate.message_id == message.message_id)
        {
            if group.messages.len() >= MAX_MEDIA_GROUP_MESSAGES {
                return Err(TelegramRuntimeError::MediaGroupTooLarge);
            }
            group.messages.push(message);
            group.messages.sort_by_key(|candidate| candidate.message_id);
        }
        group.highest_update_id = group.highest_update_id.max(update_id);
        group.updated_at_unix = now.unix_timestamp();
        Ok(())
    }

    fn due_keys(&self, now: OffsetDateTime) -> Vec<String> {
        self.groups
            .iter()
            .filter(|(_, group)| {
                now.unix_timestamp().saturating_sub(group.updated_at_unix)
                    >= MEDIA_GROUP_QUIET_PERIOD_SECONDS
            })
            .map(|(key, _)| key.clone())
            .collect()
    }
}

pub struct TelegramPollingRuntime {
    client: TelegramBotApiClient,
    service: TelegramSessionService,
    config: TelegramPollingConfig,
    media_group_path: PathBuf,
    pending_media_groups: PendingMediaGroupState,
}

impl TelegramPollingRuntime {
    pub fn new(
        client: TelegramBotApiClient,
        service: TelegramSessionService,
        config: TelegramPollingConfig,
    ) -> Result<Self, TelegramRuntimeError> {
        config.validate()?;
        let media_group_path = std::env::var_os(MEDIA_GROUP_STATE_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_MEDIA_GROUP_STATE_PATH));
        let pending_media_groups = load_media_group_state(&media_group_path)?;
        Ok(Self {
            client,
            service,
            config,
            media_group_path,
            pending_media_groups,
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
        let mut outcomes = Vec::new();
        self.flush_due_media_groups(OffsetDateTime::now_utc(), &mut outcomes)?;

        let timeout_seconds = if self.pending_media_groups.groups.is_empty() {
            self.config.timeout_seconds
        } else {
            self.config.timeout_seconds.min(1)
        };
        let updates = self.client.get_updates(
            self.service.next_update_offset(),
            timeout_seconds,
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

        for update in normalized {
            match update {
                TelegramTransportUpdate::Message { update_id, message }
                    if message.media_group_id.is_some() =>
                {
                    self.queue_media_group(update_id, message, OffsetDateTime::now_utc())?;
                }
                TelegramTransportUpdate::Message { update_id, message } => {
                    self.flush_conversation_media_groups(&message, &mut outcomes)?;
                    self.process_messages(update_id, &[message], &mut outcomes)?;
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
                        Ok(_) => self.client.answer_callback_query(&callback.query_id, None)?,
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

        self.flush_due_media_groups(OffsetDateTime::now_utc(), &mut outcomes)?;
        self.service
            .deliver_pending(&self.client, OffsetDateTime::now_utc())?;
        Ok(outcomes)
    }

    fn queue_media_group(
        &mut self,
        update_id: i64,
        message: TelegramBotMessage,
        now: OffsetDateTime,
    ) -> Result<(), TelegramRuntimeError> {
        let previous = self.pending_media_groups.clone();
        self.pending_media_groups.insert(update_id, message, now)?;
        if let Err(error) = persist_media_group_state(
            &self.media_group_path,
            &self.pending_media_groups,
        ) {
            self.pending_media_groups = previous;
            return Err(error);
        }
        self.service.acknowledge_transport_update(update_id)?;
        Ok(())
    }

    fn flush_due_media_groups(
        &mut self,
        now: OffsetDateTime,
        outcomes: &mut Vec<TelegramServiceOutcome>,
    ) -> Result<(), TelegramRuntimeError> {
        for key in self.pending_media_groups.due_keys(now) {
            self.flush_media_group(&key, outcomes)?;
        }
        Ok(())
    }

    fn flush_conversation_media_groups(
        &mut self,
        next: &TelegramBotMessage,
        outcomes: &mut Vec<TelegramServiceOutcome>,
    ) -> Result<(), TelegramRuntimeError> {
        let keys = self
            .pending_media_groups
            .groups
            .iter()
            .filter(|(_, group)| group.messages.first().is_some_and(|first| {
                first.chat.id == next.chat.id
                    && first.message_thread_id == next.message_thread_id
                    && first.from.as_ref().map(|user| user.id)
                        == next.from.as_ref().map(|user| user.id)
            }))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            self.flush_media_group(&key, outcomes)?;
        }
        Ok(())
    }

    fn flush_media_group(
        &mut self,
        key: &str,
        outcomes: &mut Vec<TelegramServiceOutcome>,
    ) -> Result<(), TelegramRuntimeError> {
        let Some(group) = self.pending_media_groups.groups.get(key).cloned() else {
            return Ok(());
        };
        match self.process_messages(group.highest_update_id, &group.messages, outcomes) {
            Ok(()) => {
                self.pending_media_groups.groups.remove(key);
                persist_media_group_state(&self.media_group_path, &self.pending_media_groups)?;
                Ok(())
            }
            Err(error) if error.is_rejected_media() => {
                self.pending_media_groups.groups.remove(key);
                persist_media_group_state(&self.media_group_path, &self.pending_media_groups)?;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn process_messages(
        &mut self,
        update_id: i64,
        messages: &[TelegramBotMessage],
        outcomes: &mut Vec<TelegramServiceOutcome>,
    ) -> Result<(), TelegramRuntimeError> {
        let inbound = self.inbound_batch(messages)?;
        match self.service.process_message(update_id, inbound) {
            Ok(outcome) => outcomes.push(outcome),
            Err(error) if is_rejected_input(&error) => {
                self.service.acknowledge_transport_update(update_id)?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
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

fn load_media_group_state(path: &Path) -> Result<PendingMediaGroupState, TelegramRuntimeError> {
    if !path.is_file() {
        return Ok(PendingMediaGroupState::default());
    }
    let bytes = fs::read(path)?;
    let state: PendingMediaGroupState = serde_json::from_slice(&bytes)?;
    state.validate()?;
    Ok(state)
}

fn persist_media_group_state(
    path: &Path,
    state: &PendingMediaGroupState,
) -> Result<(), TelegramRuntimeError> {
    state.validate()?;
    if state.groups.is_empty() {
        if path.is_file() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(state)?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    if path.is_file() {
        fs::remove_file(path)?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

fn media_group_key(message: &TelegramBotMessage) -> Option<String> {
    let group_id = message.media_group_id.as_deref()?;
    let user_id = message.from.as_ref()?.id;
    Some(format!(
        "{}:{}:{}:{}",
        message.chat.id,
        message.message_thread_id.unwrap_or_default(),
        user_id,
        group_id
    ))
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
    #[error("Telegram media-group state is invalid")]
    InvalidMediaGroupState,
    #[error("too many pending Telegram media groups")]
    TooManyPendingMediaGroups,
    #[error("Telegram media group exceeds the supported message count")]
    MediaGroupTooLarge,
    #[error("Telegram media was rejected: {0}")]
    RejectedMedia(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    BotApi(#[from] TelegramBotApiError),
    #[error(transparent)]
    Service(#[from] TelegramSessionServiceError),
}

impl TelegramRuntimeError {
    fn is_rejected_media(&self) -> bool {
        matches!(
            self,
            Self::InvalidUpdate
                | Self::InvalidMediaGroupState
                | Self::TooManyPendingMediaGroups
                | Self::MediaGroupTooLarge
                | Self::RejectedMedia(_)
        )
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

    fn album_message(message_id: i64, text: &str) -> TelegramBotMessage {
        let mut source = message(TelegramBotChatKind::Private, text);
        source.message_id = message_id;
        source.media_group_id = Some("album-1".to_owned());
        source.photo.push(TelegramPhotoSize {
            file_id: format!("file-{message_id}"),
            file_unique_id: format!("unique-{message_id}"),
            width: 10,
            height: 10,
            file_size: Some(100),
        });
        source
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
    fn media_group_members_coalesce_across_insert_calls() {
        let mut state = PendingMediaGroupState::default();
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("timestamp");
        state
            .insert(11, album_message(7, "album caption"), now)
            .expect("first");
        state
            .insert(12, album_message(8, ""), now + Duration::from_secs(1))
            .expect("second");
        let group = state.groups.values().next().expect("group");
        assert_eq!(group.highest_update_id, 12);
        assert_eq!(group.messages.len(), 2);
        assert!(state.due_keys(now + Duration::from_secs(2)).is_empty());
        assert_eq!(state.due_keys(now + Duration::from_secs(3)).len(), 1);
    }

    #[test]
    fn duplicate_redelivery_does_not_duplicate_album_members() {
        let mut state = PendingMediaGroupState::default();
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("timestamp");
        let source = album_message(7, "album caption");
        state.insert(11, source.clone(), now).expect("first");
        state.insert(11, source, now).expect("redelivery");
        assert_eq!(state.groups.values().next().expect("group").messages.len(), 1);
    }

    #[test]
    fn pending_media_groups_round_trip_durably() {
        let root = std::env::temp_dir().join(format!(
            "medusa-telegram-media-groups-{}",
            std::process::id()
        ));
        let path = root.join("pending.json");
        let _ = fs::remove_dir_all(&root);
        let mut state = PendingMediaGroupState::default();
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("timestamp");
        state
            .insert(11, album_message(7, "album caption"), now)
            .expect("insert");
        persist_media_group_state(&path, &state).expect("persist");
        assert_eq!(load_media_group_state(&path).expect("load"), state);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn attachment_batch_preserves_caption_and_ids() {
        let inbound = normalize_message_batch(
            &[album_message(7, "inspect these"), album_message(8, "")],
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
