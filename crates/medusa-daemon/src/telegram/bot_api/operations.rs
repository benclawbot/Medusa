//! Telegram Bot API service operations and bounded native file delivery.

use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use super::{
    TelegramBotApiClient, TelegramBotApiError, TelegramBotMessage, TelegramReplyParameters,
};

const MAX_OUTBOUND_FILE_BYTES: usize = 48 * 1024 * 1024;
const MAX_WEBHOOK_URL_CHARS: usize = 2_048;
const MAX_SECRET_TOKEN_CHARS: usize = 256;
const MAX_COMMANDS: usize = 100;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramBotCommand {
    pub command: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramWebhookInfo {
    pub url: String,
    pub has_custom_certificate: bool,
    pub pending_update_count: u64,
    #[serde(default)]
    pub ip_address: Option<String>,
    #[serde(default)]
    pub last_error_date: Option<i64>,
    #[serde(default)]
    pub last_error_message: Option<String>,
    #[serde(default)]
    pub max_connections: Option<u32>,
    #[serde(default)]
    pub allowed_updates: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelegramOutboundFile {
    pub file_name: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
    pub caption: Option<String>,
    pub reply_to_message_id: Option<i64>,
}

impl TelegramBotApiClient {
    pub fn set_webhook(
        &self,
        url: &str,
        secret_token: &str,
        drop_pending_updates: bool,
    ) -> Result<bool, TelegramBotApiError> {
        validate_webhook_url(url)?;
        validate_secret_token(secret_token)?;
        self.call(
            "setWebhook",
            &SetWebhookRequest {
                url,
                secret_token,
                allowed_updates: ["message", "callback_query"],
                drop_pending_updates,
            },
        )
    }

    pub fn delete_webhook(&self, drop_pending_updates: bool) -> Result<bool, TelegramBotApiError> {
        self.call(
            "deleteWebhook",
            &DeleteWebhookRequest {
                drop_pending_updates,
            },
        )
    }

    pub fn webhook_info(&self) -> Result<TelegramWebhookInfo, TelegramBotApiError> {
        self.call("getWebhookInfo", &EmptyOperationRequest {})
    }

    pub fn set_commands(
        &self,
        commands: &[TelegramBotCommand],
    ) -> Result<bool, TelegramBotApiError> {
        validate_commands(commands)?;
        self.call("setMyCommands", &SetCommandsRequest { commands })
    }

    pub fn send_document(
        &self,
        chat_id: i64,
        message_thread_id: Option<i64>,
        file: &TelegramOutboundFile,
    ) -> Result<TelegramBotMessage, TelegramBotApiError> {
        self.send_multipart_file("sendDocument", "document", chat_id, message_thread_id, file)
    }

    pub fn send_voice(
        &self,
        chat_id: i64,
        message_thread_id: Option<i64>,
        file: &TelegramOutboundFile,
    ) -> Result<TelegramBotMessage, TelegramBotApiError> {
        if file.mime_type != "audio/ogg" && file.mime_type != "audio/opus" {
            return Err(TelegramBotApiError::InvalidRequest(
                "Telegram voice output must be OGG/Opus".to_owned(),
            ));
        }
        if !file.bytes.starts_with(b"OggS") {
            return Err(TelegramBotApiError::InvalidRequest(
                "Telegram voice output is not an OGG stream".to_owned(),
            ));
        }
        self.send_multipart_file("sendVoice", "voice", chat_id, message_thread_id, file)
    }

    fn send_multipart_file(
        &self,
        method: &'static str,
        field_name: &str,
        chat_id: i64,
        message_thread_id: Option<i64>,
        file: &TelegramOutboundFile,
    ) -> Result<TelegramBotMessage, TelegramBotApiError> {
        validate_outbound_file(file)?;
        let boundary = format!("medusa-{}", Ulid::new());
        let mut body = Vec::with_capacity(file.bytes.len().saturating_add(2_048));
        push_text_part(&mut body, &boundary, "chat_id", &chat_id.to_string());
        if let Some(topic_id) = message_thread_id {
            push_text_part(
                &mut body,
                &boundary,
                "message_thread_id",
                &topic_id.to_string(),
            );
        }
        if let Some(caption) = file.caption.as_deref().filter(|value| !value.is_empty()) {
            push_text_part(&mut body, &boundary, "caption", caption);
        }
        if let Some(message_id) = file.reply_to_message_id {
            let value =
                serde_json::to_string(&TelegramReplyParameters { message_id }).map_err(|_| {
                    TelegramBotApiError::InvalidRequest(
                        "Telegram reply parameters are invalid".to_owned(),
                    )
                })?;
            push_text_part(&mut body, &boundary, "reply_parameters", &value);
        }
        push_file_part(&mut body, &boundary, field_name, file);
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        let url = format!("{}/bot{}/{}", self.api_base, self.token.expose(), method);
        let response = self
            .client
            .post(url)
            .header(
                CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .map_err(super::classify_transport_error)?;
        self.decode_response(method, response)
    }
}

fn validate_webhook_url(url: &str) -> Result<(), TelegramBotApiError> {
    if url.is_empty() || url.len() > MAX_WEBHOOK_URL_CHARS {
        return Err(TelegramBotApiError::InvalidRequest(
            "Telegram webhook URL is invalid".to_owned(),
        ));
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| {
        TelegramBotApiError::InvalidRequest("Telegram webhook URL is invalid".to_owned())
    })?;
    if parsed.scheme() != "https" || parsed.host_str().is_none() || parsed.fragment().is_some() {
        return Err(TelegramBotApiError::InvalidRequest(
            "Telegram webhook URL must be public HTTPS".to_owned(),
        ));
    }
    Ok(())
}

fn validate_secret_token(secret: &str) -> Result<(), TelegramBotApiError> {
    if secret.is_empty()
        || secret.len() > MAX_SECRET_TOKEN_CHARS
        || !secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(TelegramBotApiError::InvalidRequest(
            "Telegram webhook secret token is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_commands(commands: &[TelegramBotCommand]) -> Result<(), TelegramBotApiError> {
    if commands.is_empty() || commands.len() > MAX_COMMANDS {
        return Err(TelegramBotApiError::InvalidRequest(
            "Telegram command list is invalid".to_owned(),
        ));
    }
    for command in commands {
        if command.command.is_empty()
            || command.command.len() > 32
            || !command
                .command
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            || command.description.is_empty()
            || command.description.chars().count() > 256
        {
            return Err(TelegramBotApiError::InvalidRequest(
                "Telegram bot command is invalid".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_outbound_file(file: &TelegramOutboundFile) -> Result<(), TelegramBotApiError> {
    if file.bytes.is_empty() || file.bytes.len() > MAX_OUTBOUND_FILE_BYTES {
        return Err(TelegramBotApiError::InvalidRequest(
            "Telegram outbound file size is invalid".to_owned(),
        ));
    }
    if file.file_name.trim().is_empty()
        || file.file_name.chars().count() > 240
        || file
            .file_name
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\' | '"'))
        || file.mime_type.trim().is_empty()
        || file.mime_type.len() > 128
        || file.mime_type.bytes().any(|byte| byte.is_ascii_control())
        || file
            .caption
            .as_deref()
            .is_some_and(|caption| caption.chars().count() > 1_024)
    {
        return Err(TelegramBotApiError::InvalidRequest(
            "Telegram outbound file metadata is invalid".to_owned(),
        ));
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

fn push_file_part(body: &mut Vec<u8>, boundary: &str, name: &str, file: &TelegramOutboundFile) {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"{name}\"; filename=\"{}\"\r\n",
            file.file_name
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {}\r\n\r\n", file.mime_type).as_bytes());
    body.extend_from_slice(&file.bytes);
    body.extend_from_slice(b"\r\n");
}

#[derive(Serialize)]
struct EmptyOperationRequest {}

#[derive(Serialize)]
struct SetWebhookRequest<'a> {
    url: &'a str,
    secret_token: &'a str,
    allowed_updates: [&'a str; 2],
    drop_pending_updates: bool,
}

#[derive(Serialize)]
struct DeleteWebhookRequest {
    drop_pending_updates: bool,
}

#[derive(Serialize)]
struct SetCommandsRequest<'a> {
    commands: &'a [TelegramBotCommand],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_secret_and_url_fail_closed() {
        assert!(validate_secret_token("valid_secret-42").is_ok());
        assert!(validate_secret_token("bad secret").is_err());
        assert!(validate_webhook_url("https://example.test/telegram").is_ok());
        assert!(validate_webhook_url("http://example.test/telegram").is_err());
    }

    #[test]
    fn outbound_voice_requires_ogg_opus() {
        let file = TelegramOutboundFile {
            file_name: "reply.ogg".to_owned(),
            mime_type: "audio/ogg".to_owned(),
            bytes: b"not-ogg".to_vec(),
            caption: None,
            reply_to_message_id: None,
        };
        assert!(validate_outbound_file(&file).is_ok());
    }

    #[test]
    fn command_contract_is_bounded() {
        assert!(
            validate_commands(&[TelegramBotCommand {
                command: "status".to_owned(),
                description: "Show the current session".to_owned(),
            }])
            .is_ok()
        );
        assert!(
            validate_commands(&[TelegramBotCommand {
                command: "Status".to_owned(),
                description: "invalid".to_owned(),
            }])
            .is_err()
        );
    }
}
