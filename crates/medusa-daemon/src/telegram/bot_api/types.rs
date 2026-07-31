use serde::{Deserialize, Serialize};

use super::TelegramBotApiError;
use crate::telegram::TelegramReaction;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramBotUser {
    pub id: i64,
    pub is_bot: bool,
    pub first_name: String,
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TelegramBotChatKind {
    Private,
    Group,
    Supergroup,
    Channel,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramBotChat {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: TelegramBotChatKind,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub is_forum: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramPhotoSize {
    pub file_id: String,
    pub file_unique_id: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramDocument {
    pub file_id: String,
    pub file_unique_id: String,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramBotMessage {
    pub message_id: i64,
    pub date: i64,
    pub chat: TelegramBotChat,
    #[serde(default)]
    pub from: Option<TelegramBotUser>,
    #[serde(default)]
    pub message_thread_id: Option<i64>,
    #[serde(default)]
    pub media_group_id: Option<String>,
    #[serde(default)]
    pub photo: Vec<TelegramPhotoSize>,
    #[serde(default)]
    pub document: Option<TelegramDocument>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramCallbackQuery {
    pub id: String,
    pub from: TelegramBotUser,
    #[serde(default)]
    pub message: Option<TelegramBotMessage>,
    #[serde(default)]
    pub data: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramUpdate {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<TelegramBotMessage>,
    #[serde(default)]
    pub callback_query: Option<TelegramCallbackQuery>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelegramInboundCallback {
    pub query_id: String,
    pub user: TelegramBotUser,
    pub chat: TelegramBotChat,
    pub message_id: i64,
    pub message_thread_id: Option<i64>,
    pub data: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TelegramTransportUpdate {
    Message {
        update_id: i64,
        message: TelegramBotMessage,
    },
    Callback {
        update_id: i64,
        callback: TelegramInboundCallback,
    },
    Unsupported {
        update_id: i64,
    },
}

impl TryFrom<TelegramUpdate> for TelegramTransportUpdate {
    type Error = TelegramBotApiError;

    fn try_from(update: TelegramUpdate) -> Result<Self, Self::Error> {
        if update.update_id < 0 {
            return Err(TelegramBotApiError::InvalidUpdate);
        }
        if let Some(message) = update.message {
            if message.date < 0 {
                return Err(TelegramBotApiError::InvalidTimestamp);
            }
            return Ok(Self::Message {
                update_id: update.update_id,
                message,
            });
        }
        if let Some(query) = update.callback_query {
            let message = query.message.ok_or(TelegramBotApiError::InvalidUpdate)?;
            let data = query.data.ok_or(TelegramBotApiError::InvalidUpdate)?;
            return Ok(Self::Callback {
                update_id: update.update_id,
                callback: TelegramInboundCallback {
                    query_id: query.id,
                    user: query.from,
                    chat: message.chat,
                    message_id: message.message_id,
                    message_thread_id: message.message_thread_id,
                    data,
                },
            });
        }
        Ok(Self::Unsupported {
            update_id: update.update_id,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TelegramUpdateCursor {
    next_offset: Option<i64>,
}

impl TelegramUpdateCursor {
    #[must_use]
    pub const fn next_offset(&self) -> Option<i64> {
        self.next_offset
    }

    pub fn acknowledge(&mut self, update_id: i64) -> Result<(), TelegramBotApiError> {
        if update_id < 0
            || self
                .next_offset
                .is_some_and(|offset| update_id + 1 < offset)
        {
            return Err(TelegramBotApiError::InvalidUpdate);
        }
        let next = update_id
            .checked_add(1)
            .ok_or(TelegramBotApiError::InvalidUpdate)?;
        self.next_offset = Some(self.next_offset.map_or(next, |current| current.max(next)));
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelegramChatAction {
    Typing,
    UploadDocument,
    RecordVoice,
    UploadVoice,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub enum TelegramBotParseMode {
    MarkdownV2,
}

impl Serialize for TelegramBotParseMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str("MarkdownV2")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramBotInlineButton {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_app: Option<TelegramWebAppInfo>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramWebAppInfo {
    pub url: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramInlineKeyboardMarkup {
    pub inline_keyboard: Vec<Vec<TelegramBotInlineButton>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramReplyParameters {
    pub message_id: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramLinkPreviewOptions {
    pub is_disabled: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramSendMessage {
    pub chat_id: i64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_thread_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_mode: Option<TelegramBotParseMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_parameters: Option<TelegramReplyParameters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<TelegramInlineKeyboardMarkup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_preview_options: Option<TelegramLinkPreviewOptions>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramEditMessageText {
    pub chat_id: i64,
    pub message_id: i64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_mode: Option<TelegramBotParseMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<TelegramInlineKeyboardMarkup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_preview_options: Option<TelegramLinkPreviewOptions>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TelegramEditMessageOutcome {
    Updated(Box<TelegramBotMessage>),
    Unchanged,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramFile {
    pub file_id: String,
    pub file_unique_id: String,
    #[serde(default)]
    pub file_size: Option<u64>,
    #[serde(default)]
    pub file_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub(crate) struct TelegramApiEnvelope<T> {
    pub ok: bool,
    #[serde(default)]
    pub result: Option<T>,
    #[serde(default)]
    pub error_code: Option<i64>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Option<TelegramResponseParameters>,
}

impl<T> TelegramApiEnvelope<T> {
    pub(crate) fn retry_after(&self) -> Option<u64> {
        self.parameters
            .as_ref()
            .and_then(|parameters| parameters.retry_after)
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct TelegramResponseParameters {
    #[serde(default)]
    pub retry_after: Option<u64>,
}

#[derive(Serialize)]
pub(crate) struct EmptyRequest {}
#[derive(Serialize)]
pub(crate) struct GetFileRequest<'a> {
    pub file_id: &'a str,
}
#[derive(Serialize)]
pub(crate) struct GetUpdatesRequest<'a> {
    pub offset: Option<i64>,
    pub timeout: u16,
    pub limit: u8,
    pub allowed_updates: [&'a str; 2],
}
#[derive(Serialize)]
pub(crate) struct SendChatActionRequest {
    pub chat_id: i64,
    pub message_thread_id: Option<i64>,
    pub action: TelegramChatAction,
}
#[derive(Serialize)]
pub(crate) struct DeleteMessageRequest {
    pub chat_id: i64,
    pub message_id: i64,
}
#[derive(Serialize)]
pub(crate) struct AnswerCallbackQueryRequest<'a> {
    pub callback_query_id: &'a str,
    pub text: Option<&'a str>,
    pub show_alert: bool,
}
#[derive(Serialize)]
pub(crate) struct SetMessageReactionRequest {
    pub chat_id: i64,
    pub message_id: i64,
    pub reaction: Vec<TelegramReactionType>,
    pub is_big: bool,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum TelegramReactionType {
    Emoji { emoji: &'static str },
}

impl From<TelegramReaction> for TelegramReactionType {
    fn from(value: TelegramReaction) -> Self {
        let emoji = match value {
            TelegramReaction::Processing => "👀",
            TelegramReaction::Success => "👍",
            TelegramReaction::Failure => "👎",
        };
        Self::Emoji { emoji }
    }
}
