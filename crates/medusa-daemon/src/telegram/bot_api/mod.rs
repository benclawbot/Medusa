use std::{fmt, io::Read, time::Duration};

use reqwest::{
    StatusCode,
    blocking::{Client, Response},
    header::RETRY_AFTER,
};
use serde::{Serialize, de::DeserializeOwned};

mod operations;
mod types;

pub use operations::{TelegramBotCommand, TelegramOutboundFile, TelegramWebhookInfo};
use types::{
    AnswerCallbackQueryRequest, DeleteMessageRequest, EmptyRequest, GetFileRequest,
    GetUpdatesRequest, SendChatActionRequest, SetMessageReactionRequest, TelegramApiEnvelope,
    TelegramReactionType,
};
pub use types::{
    TelegramBotChat, TelegramBotChatKind, TelegramBotInlineButton, TelegramBotMessage,
    TelegramBotParseMode, TelegramBotUser, TelegramCallbackQuery, TelegramChatAction,
    TelegramDocument, TelegramEditMessageOutcome, TelegramEditMessageText, TelegramFile,
    TelegramInboundCallback, TelegramInlineKeyboardMarkup, TelegramLinkPreviewOptions,
    TelegramPhotoSize, TelegramReplyParameters, TelegramSendMessage, TelegramTransportUpdate,
    TelegramUpdate, TelegramUpdateCursor, TelegramWebAppInfo,
};

#[cfg(test)]
mod tests;

const TELEGRAM_API_BASE: &str = "https://api.telegram.org";
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_ERROR_DESCRIPTION_CHARS: usize = 512;
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FILE_ID_CHARS: usize = 512;
const MAX_FILE_PATH_CHARS: usize = 1_024;

#[derive(Clone, Eq, PartialEq)]
pub struct TelegramBotToken(String);

impl TelegramBotToken {
    pub fn new(value: impl Into<String>) -> Result<Self, TelegramBotApiError> {
        let value = value.into();
        let trimmed = value.trim();
        let Some((bot_id, secret)) = trimmed.split_once(':') else {
            return Err(TelegramBotApiError::InvalidToken);
        };
        if trimmed != value
            || trimmed.len() > 256
            || bot_id.is_empty()
            || !bot_id.bytes().all(|byte| byte.is_ascii_digit())
            || secret.is_empty()
            || !secret
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(TelegramBotApiError::InvalidToken);
        }
        Ok(Self(value))
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for TelegramBotToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TelegramBotToken([REDACTED])")
    }
}

#[derive(Clone)]
pub struct TelegramBotApiClient {
    client: Client,
    token: TelegramBotToken,
    api_base: String,
}

impl fmt::Debug for TelegramBotApiClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TelegramBotApiClient")
            .field("token", &self.token)
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

impl TelegramBotApiClient {
    pub fn new(token: TelegramBotToken) -> Result<Self, TelegramBotApiError> {
        Self::with_api_base(token, TELEGRAM_API_BASE)
    }

    pub(super) fn with_api_base(
        token: TelegramBotToken,
        api_base: impl Into<String>,
    ) -> Result<Self, TelegramBotApiError> {
        let api_base = api_base.into();
        validate_api_base(&api_base)?;
        let client = Client::builder()
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .user_agent(concat!(
                "medusa/",
                env!("CARGO_PKG_VERSION"),
                " telegram-gateway"
            ))
            .build()
            .map_err(|_| TelegramBotApiError::Transport {
                kind: TelegramTransportFailure::ClientSetup,
                status: None,
            })?;
        Ok(Self {
            client,
            token,
            api_base: api_base.trim_end_matches('/').to_owned(),
        })
    }

    pub fn get_me(&self) -> Result<TelegramBotUser, TelegramBotApiError> {
        self.call("getMe", &EmptyRequest {})
    }

    pub fn get_file(&self, file_id: &str) -> Result<TelegramFile, TelegramBotApiError> {
        if file_id.is_empty()
            || file_id.len() > MAX_FILE_ID_CHARS
            || !file_id.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(TelegramBotApiError::InvalidRequest(
                "Telegram file id is invalid".to_owned(),
            ));
        }
        self.call("getFile", &GetFileRequest { file_id })
    }

    pub fn download_file(
        &self,
        file_path: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, TelegramBotApiError> {
        validate_file_path(file_path)?;
        if max_bytes == 0
            || max_bytes > medusa_runtime::attachment::MAX_TOTAL_ATTACHMENT_BYTES as u64
        {
            return Err(TelegramBotApiError::InvalidRequest(
                "Telegram file byte limit is invalid".to_owned(),
            ));
        }
        let url = format!(
            "{}/file/bot{}/{}",
            self.api_base,
            self.token.expose(),
            file_path
        );
        let response = self
            .client
            .get(url)
            .send()
            .map_err(classify_transport_error)?;
        let status = response.status();
        if let Some(seconds) = retry_after_header(&response) {
            return Err(TelegramBotApiError::RetryAfter { seconds });
        }
        if !status.is_success() {
            return Err(if status.is_server_error() {
                TelegramBotApiError::Transport {
                    kind: TelegramTransportFailure::Server,
                    status: Some(status.as_u16()),
                }
            } else {
                TelegramBotApiError::Rejected {
                    status: Some(status.as_u16()),
                    code: None,
                    description: "Telegram rejected the file download".to_owned(),
                }
            });
        }
        if let Some(bytes) = response
            .content_length()
            .filter(|length| *length > max_bytes)
        {
            return Err(TelegramBotApiError::FileTooLarge {
                bytes,
                limit: max_bytes,
            });
        }
        let mut bytes = Vec::new();
        response
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| TelegramBotApiError::Transport {
                kind: TelegramTransportFailure::Read,
                status: Some(status.as_u16()),
            })?;
        if bytes.len() as u64 > max_bytes {
            return Err(TelegramBotApiError::FileTooLarge {
                bytes: bytes.len() as u64,
                limit: max_bytes,
            });
        }
        Ok(bytes)
    }

    pub fn get_updates(
        &self,
        offset: Option<i64>,
        timeout_seconds: u16,
        limit: u8,
    ) -> Result<Vec<TelegramUpdate>, TelegramBotApiError> {
        if offset.is_some_and(|value| value < 0)
            || !(1..=50).contains(&timeout_seconds)
            || !(1..=100).contains(&limit)
        {
            return Err(TelegramBotApiError::InvalidRequest(
                "poll offset must be non-negative, timeout must be 1..=50 seconds, and limit must be 1..=100"
                    .to_owned(),
            ));
        }
        self.call(
            "getUpdates",
            &GetUpdatesRequest {
                offset,
                timeout: timeout_seconds,
                limit,
                allowed_updates: ["message", "callback_query"],
            },
        )
    }

    pub fn send_chat_action(
        &self,
        chat_id: i64,
        message_thread_id: Option<i64>,
        action: TelegramChatAction,
    ) -> Result<bool, TelegramBotApiError> {
        self.call(
            "sendChatAction",
            &SendChatActionRequest {
                chat_id,
                message_thread_id,
                action,
            },
        )
    }

    pub fn set_message_reaction(
        &self,
        chat_id: i64,
        message_id: i64,
        reaction: Option<super::TelegramReaction>,
    ) -> Result<bool, TelegramBotApiError> {
        let reaction = reaction
            .map(|reaction| vec![TelegramReactionType::from(reaction)])
            .unwrap_or_default();
        self.call(
            "setMessageReaction",
            &SetMessageReactionRequest {
                chat_id,
                message_id,
                reaction,
                is_big: false,
            },
        )
    }

    pub fn send_message(
        &self,
        request: &TelegramSendMessage,
    ) -> Result<TelegramBotMessage, TelegramBotApiError> {
        self.call("sendMessage", request)
    }

    pub fn edit_message_text(
        &self,
        request: &TelegramEditMessageText,
    ) -> Result<TelegramEditMessageOutcome, TelegramBotApiError> {
        match self.call("editMessageText", request) {
            Ok(message) => Ok(TelegramEditMessageOutcome::Updated(Box::new(message))),
            Err(error) if error.is_message_not_modified() => {
                Ok(TelegramEditMessageOutcome::Unchanged)
            }
            Err(error) => Err(error),
        }
    }

    pub fn delete_message(
        &self,
        chat_id: i64,
        message_id: i64,
    ) -> Result<bool, TelegramBotApiError> {
        self.call(
            "deleteMessage",
            &DeleteMessageRequest {
                chat_id,
                message_id,
            },
        )
    }

    pub fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
    ) -> Result<bool, TelegramBotApiError> {
        self.call(
            "answerCallbackQuery",
            &AnswerCallbackQueryRequest {
                callback_query_id,
                text,
                show_alert: false,
            },
        )
    }

    fn call<Q, R>(&self, method: &'static str, body: &Q) -> Result<R, TelegramBotApiError>
    where
        Q: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let url = format!("{}/bot{}/{}", self.api_base, self.token.expose(), method);
        let response = self
            .client
            .post(url)
            .json(body)
            .send()
            .map_err(classify_transport_error)?;
        self.decode_response(method, response)
    }

    fn decode_response<R: DeserializeOwned>(
        &self,
        method: &'static str,
        response: Response,
    ) -> Result<R, TelegramBotApiError> {
        let status = response.status();
        if let Some(seconds) = retry_after_header(&response) {
            return Err(TelegramBotApiError::RetryAfter { seconds });
        }
        let mut bytes = Vec::new();
        response
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| TelegramBotApiError::Transport {
                kind: TelegramTransportFailure::Read,
                status: Some(status.as_u16()),
            })?;
        if bytes.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(TelegramBotApiError::InvalidResponse { method });
        }
        let envelope: TelegramApiEnvelope<R> = serde_json::from_slice(&bytes).map_err(|_| {
            if status.is_server_error() {
                TelegramBotApiError::Transport {
                    kind: TelegramTransportFailure::Server,
                    status: Some(status.as_u16()),
                }
            } else {
                TelegramBotApiError::InvalidResponse { method }
            }
        })?;

        if let Some(seconds) = envelope.retry_after() {
            return Err(TelegramBotApiError::RetryAfter { seconds });
        }
        if status.is_server_error() {
            return Err(TelegramBotApiError::Transport {
                kind: TelegramTransportFailure::Server,
                status: Some(status.as_u16()),
            });
        }
        if envelope.ok {
            return envelope
                .result
                .ok_or(TelegramBotApiError::InvalidResponse { method });
        }

        Err(TelegramBotApiError::Rejected {
            status: Some(status.as_u16()),
            code: envelope.error_code,
            description: sanitize_description(
                envelope
                    .description
                    .as_deref()
                    .unwrap_or("Telegram rejected the request"),
                self.token.expose(),
            ),
        })
    }
}

#[derive(Clone, Copy, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelegramTransportFailure {
    ClientSetup,
    Timeout,
    Connect,
    Read,
    Server,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TelegramBotApiError {
    #[error("invalid Telegram bot token")]
    InvalidToken,
    #[error("invalid Telegram Bot API endpoint")]
    InvalidEndpoint,
    #[error("invalid Telegram Bot API request: {0}")]
    InvalidRequest(String),
    #[error("Telegram Bot API transport failed: {kind:?}")]
    Transport {
        kind: TelegramTransportFailure,
        status: Option<u16>,
    },
    #[error("Telegram Bot API requested a retry after {seconds} seconds")]
    RetryAfter { seconds: u64 },
    #[error("Telegram Bot API rejected the request: {description}")]
    Rejected {
        status: Option<u16>,
        code: Option<i64>,
        description: String,
    },
    #[error("Telegram Bot API returned an invalid response for {method}")]
    InvalidResponse { method: &'static str },
    #[error("Telegram file is {bytes} bytes; limit is {limit}")]
    FileTooLarge { bytes: u64, limit: u64 },
    #[error("Telegram update timestamp is invalid")]
    InvalidTimestamp,
    #[error("Telegram update does not contain a usable sender and chat")]
    InvalidUpdate,
}

impl TelegramBotApiError {
    #[must_use]
    pub const fn retry_after_seconds(&self) -> Option<u64> {
        match self {
            Self::RetryAfter { seconds } => Some(*seconds),
            _ => None,
        }
    }

    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(self, Self::RetryAfter { .. } | Self::Transport { .. })
    }

    fn is_message_not_modified(&self) -> bool {
        matches!(
            self,
            Self::Rejected { description, .. }
                if description.to_ascii_lowercase().contains("message is not modified")
        )
    }

    #[must_use]
    pub(crate) fn is_formatting_rejection(&self) -> bool {
        matches!(
            self,
            Self::Rejected { description, .. } if {
                let description = description.to_ascii_lowercase();
                description.contains("can't parse entities")
                    || description.contains("cannot parse entities")
                    || description.contains("entity") && description.contains("parse")
            }
        )
    }
}

fn validate_api_base(api_base: &str) -> Result<(), TelegramBotApiError> {
    let parsed = reqwest::Url::parse(api_base).map_err(|_| TelegramBotApiError::InvalidEndpoint)?;
    let secure = parsed.scheme() == "https";
    let loopback = parsed.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if (!secure && !loopback) || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(TelegramBotApiError::InvalidEndpoint);
    }
    Ok(())
}

fn validate_file_path(file_path: &str) -> Result<(), TelegramBotApiError> {
    if file_path.is_empty()
        || file_path.len() > MAX_FILE_PATH_CHARS
        || file_path.starts_with('/')
        || file_path.split('/').any(|segment| {
            segment.is_empty()
                || segment == "."
                || segment == ".."
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        })
    {
        return Err(TelegramBotApiError::InvalidRequest(
            "Telegram file path is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn classify_transport_error(error: reqwest::Error) -> TelegramBotApiError {
    let kind = if error.is_timeout() {
        TelegramTransportFailure::Timeout
    } else if error.is_connect() {
        TelegramTransportFailure::Connect
    } else if error.is_body() || error.is_decode() {
        TelegramTransportFailure::Read
    } else {
        TelegramTransportFailure::Other
    };
    TelegramBotApiError::Transport { kind, status: None }
}

fn retry_after_header(response: &Response) -> Option<u64> {
    if response.status() != StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

fn sanitize_description(description: &str, token: &str) -> String {
    let redacted = description.replace(token, "[REDACTED]");
    redacted.chars().take(MAX_ERROR_DESCRIPTION_CHARS).collect()
}
