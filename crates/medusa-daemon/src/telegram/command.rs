use medusa_protocol::frontend::{
    AttachmentMode, FRONTEND_PROTOCOL_VERSION, FrontendCommand, FrontendCommandEnvelope,
    FrontendKind,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::{
    TelegramConfig, TelegramGatewayError, TelegramIdentity, TelegramVoiceMode, ToolProgressMode,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelegramInboundMessage {
    pub identity: TelegramIdentity,
    pub message_id: i64,
    pub text: String,
    #[serde(default)]
    pub attachment_ids: Vec<String>,
    pub attached_session_id: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub received_at: OffsetDateTime,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum TelegramInboundAction {
    Forward(Box<FrontendCommandEnvelope>),
    SetToolProgress(ToolProgressMode),
    SetVoiceMode(TelegramVoiceMode),
    VoiceStatus,
    StartLiveVoice,
    Help,
}

pub(crate) fn map_message(
    config: &TelegramConfig,
    message: &TelegramInboundMessage,
) -> Result<TelegramInboundAction, TelegramGatewayError> {
    config.authorize(&message.identity)?;
    let trimmed = message.text.trim();
    if trimmed.is_empty() && message.attachment_ids.is_empty() {
        return Err(TelegramGatewayError::EmptyMessage);
    }
    let (command, arguments) = split_command(trimmed);
    if command.is_some() && !message.attachment_ids.is_empty() {
        return Err(TelegramGatewayError::AttachmentsNotAllowedForCommand);
    }
    match command {
        None => forward(
            message,
            FrontendCommand::Submit {
                text: trimmed.to_owned(),
                attachment_ids: message.attachment_ids.clone(),
            },
        ),
        Some("/new") => forward(
            message,
            FrontendCommand::CreateSession {
                repository_profile: config.repository_profile.clone(),
                objective: non_empty(arguments).map(str::to_owned),
                attachment_ids: Vec::new(),
            },
        ),
        Some("/sessions") => forward(message, FrontendCommand::ListSessions),
        Some("/attach") => forward(
            message,
            FrontendCommand::Attach {
                session_id: required(arguments, "usage: /attach <session>")?.to_owned(),
                mode: AttachmentMode::ReadOnly,
                after_cursor: None,
            },
        ),
        Some("/detach") => forward(message, FrontendCommand::Detach),
        Some("/resume") => forward(
            message,
            FrontendCommand::ResumeSession {
                session_id: required(arguments, "usage: /resume <session>")?.to_owned(),
            },
        ),
        Some("/status") => forward(message, FrontendCommand::ShowStatus),
        Some("/stop") => forward(message, FrontendCommand::CancelTurn),
        Some("/model") => forward(
            message,
            FrontendCommand::ConfigureModel {
                provider: None,
                model: required(arguments, "usage: /model <model>")?.to_owned(),
                base_url: None,
            },
        ),
        Some("/effort") => forward(
            message,
            FrontendCommand::SetEffort {
                effort: required(arguments, "usage: /effort <low|medium|high>")?.to_owned(),
            },
        ),
        Some("/plan") => forward(
            message,
            FrontendCommand::SetPlanMode {
                enabled: parse_on_off(arguments, "usage: /plan <on|off>")?,
            },
        ),
        Some("/verbose") => Ok(TelegramInboundAction::SetToolProgress(parse_progress(
            arguments,
        )?)),
        Some("/voice") => parse_voice(arguments),
        Some("/help") => Ok(TelegramInboundAction::Help),
        Some(other) => Err(TelegramGatewayError::UnknownCommand(other.to_owned())),
    }
}

fn forward(
    message: &TelegramInboundMessage,
    command: FrontendCommand,
) -> Result<TelegramInboundAction, TelegramGatewayError> {
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
        client_id: client_id(identity),
        session_id: message.attached_session_id.clone(),
        turn_id: None,
        timestamp: message.received_at,
        command,
    };
    envelope
        .validate()
        .map_err(|error| TelegramGatewayError::Protocol(error.to_owned()))?;
    Ok(TelegramInboundAction::Forward(Box::new(envelope)))
}

pub(crate) fn client_id(identity: &TelegramIdentity) -> String {
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
    let (command, arguments) = input.split_once(char::is_whitespace).unwrap_or((input, ""));
    let command = command.split_once('@').map_or(command, |(name, _)| name);
    (Some(command), arguments.trim())
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::telegram::{TelegramChatKind, TelegramConfig};
    use time::macros::datetime;

    fn config() -> TelegramConfig {
        TelegramConfig {
            enabled: true,
            allowed_users: BTreeSet::from([42]),
            ..TelegramConfig::default()
        }
    }

    fn message(text: &str) -> TelegramInboundMessage {
        TelegramInboundMessage {
            identity: TelegramIdentity {
                user_id: 42,
                chat_id: 42,
                topic_id: None,
                chat_kind: TelegramChatKind::Private,
                bot_mentioned: false,
            },
            message_id: 9,
            text: text.to_owned(),
            attachment_ids: Vec::new(),
            attached_session_id: Some("session-1".to_owned()),
            received_at: datetime!(2026-07-30 16:00 UTC),
        }
    }

    #[test]
    fn text_and_commands_map_to_shared_protocol() {
        let TelegramInboundAction::Forward(submit) =
            map_message(&config(), &message("inspect the test")).expect("submit")
        else {
            panic!("expected forwarded submission");
        };
        assert!(matches!(submit.command, FrontendCommand::Submit { .. }));

        let TelegramInboundAction::Forward(stop) =
            map_message(&config(), &message("/stop")).expect("stop")
        else {
            panic!("expected forwarded cancellation");
        };
        assert_eq!(stop.command, FrontendCommand::CancelTurn);
    }

    #[test]
    fn one_telegram_message_has_stable_idempotency() {
        let first = map_message(&config(), &message("/status")).expect("first");
        let second = map_message(&config(), &message("/status")).expect("second");
        assert_eq!(first, second);
    }

    #[test]
    fn attachment_only_message_maps_to_shared_submission() {
        let mut source = message("");
        source.attachment_ids = vec!["frontend-artifact-abc".to_owned()];
        let TelegramInboundAction::Forward(submit) =
            map_message(&config(), &source).expect("attachment submission")
        else {
            panic!("expected forwarded attachment submission");
        };
        assert!(matches!(
            submit.command,
            FrontendCommand::Submit { ref text, ref attachment_ids }
                if text.is_empty() && attachment_ids == &["frontend-artifact-abc"]
        ));
    }
}
