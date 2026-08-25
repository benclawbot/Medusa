//! Telegram Bot API execution for deterministic renderer actions.
//!
//! This layer owns only Telegram message identifiers and callback/Web App presentation. Runtime
//! authorization remains in the shared frontend control plane.

use std::collections::BTreeMap;

use medusa_protocol::frontend::FrontendCommand;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};

use super::{
    TelegramAction, TelegramButtonIntent, TelegramControl, TelegramGateway, TelegramMessageSlot,
    TelegramParseMode, TelegramRenderButton, TelegramSessionServiceError,
    bot_api::{
        TelegramBotApiClient, TelegramBotInlineButton, TelegramBotParseMode,
        TelegramEditMessageOutcome, TelegramEditMessageText, TelegramInlineKeyboardMarkup,
        TelegramLinkPreviewOptions, TelegramOutboundFile, TelegramReplyParameters,
        TelegramSendMessage, TelegramWebAppInfo,
    },
    config::TelegramIdentity,
};

const CALLBACK_LIFETIME: Duration = Duration::minutes(10);

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct TelegramDeliveryState {
    pub source_message_id: Option<i64>,
    pub slots: BTreeMap<TelegramMessageSlot, i64>,
}

impl TelegramDeliveryState {
    pub fn set_source_message(&mut self, message_id: i64) {
        if message_id > 0 {
            self.source_message_id = Some(message_id);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_actions(
    client: &TelegramBotApiClient,
    gateway: &mut TelegramGateway,
    control: &TelegramControl,
    identity: &TelegramIdentity,
    session_id: &str,
    turn_id: Option<&str>,
    state: &mut TelegramDeliveryState,
    actions: &[TelegramAction],
    mini_app_url: Option<&str>,
    now: OffsetDateTime,
) -> Result<(), TelegramSessionServiceError> {
    for action in actions {
        execute_action(
            client,
            gateway,
            control,
            identity,
            session_id,
            turn_id,
            state,
            action,
            mini_app_url,
            now,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn execute_action(
    client: &TelegramBotApiClient,
    gateway: &mut TelegramGateway,
    control: &TelegramControl,
    identity: &TelegramIdentity,
    session_id: &str,
    turn_id: Option<&str>,
    state: &mut TelegramDeliveryState,
    action: &TelegramAction,
    mini_app_url: Option<&str>,
    now: OffsetDateTime,
) -> Result<(), TelegramSessionServiceError> {
    match action {
        TelegramAction::SetReaction { reaction } => {
            if let Some(source_message_id) = state.source_message_id {
                client.set_message_reaction(identity.chat_id, source_message_id, *reaction)?;
            }
        }
        TelegramAction::SetTyping { active } => {
            if *active {
                client.send_chat_action(
                    identity.chat_id,
                    identity.topic_id,
                    super::bot_api::TelegramChatAction::Typing,
                )?;
            }
        }
        TelegramAction::UpsertText {
            slot,
            text,
            parse_mode,
            buttons,
            disable_link_preview,
        } => {
            let keyboard = render_keyboard(
                gateway,
                identity,
                session_id,
                turn_id,
                slot,
                buttons,
                mini_app_url,
                now,
            )?;
            upsert_text(
                client,
                identity,
                state,
                slot,
                text,
                *parse_mode,
                keyboard,
                *disable_link_preview,
            )?;
        }
        TelegramAction::DeleteSlot { slot } => {
            if let Some(message_id) = state.slots.get(slot).copied() {
                if client.delete_message(identity.chat_id, message_id)? {
                    state.slots.remove(slot);
                }
            }
        }
        TelegramAction::SendArtifact {
            artifact_id,
            evidence_ref,
            caption,
        } => {
            let artifact = control.export_attachment(artifact_id)?;
            let slot = TelegramMessageSlot::Notice(format!("artifact:{artifact_id}"));
            let reply_to_message_id = reply_target(state, &slot);
            let message = client.send_document(
                identity.chat_id,
                identity.topic_id,
                &TelegramOutboundFile {
                    file_name: artifact.display_name,
                    mime_type: artifact
                        .mime_type
                        .unwrap_or_else(|| "application/octet-stream".to_owned()),
                    bytes: artifact.bytes,
                    caption: caption
                        .clone()
                        .or_else(|| Some(format!("Evidence: {evidence_ref}"))),
                    reply_to_message_id,
                },
            )?;
            state.slots.insert(slot, message.message_id);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn upsert_text(
    client: &TelegramBotApiClient,
    identity: &TelegramIdentity,
    state: &mut TelegramDeliveryState,
    slot: &TelegramMessageSlot,
    text: &str,
    parse_mode: TelegramParseMode,
    reply_markup: Option<TelegramInlineKeyboardMarkup>,
    disable_link_preview: bool,
) -> Result<(), TelegramSessionServiceError> {
    let link_preview_options = Some(TelegramLinkPreviewOptions {
        is_disabled: disable_link_preview,
    });
    let bot_parse_mode = bot_parse_mode(parse_mode);
    if let Some(message_id) = state.slots.get(slot).copied() {
        let request = TelegramEditMessageText {
            chat_id: identity.chat_id,
            message_id,
            text: text.to_owned(),
            parse_mode: bot_parse_mode,
            reply_markup: reply_markup.clone(),
            link_preview_options: link_preview_options.clone(),
        };
        match client.edit_message_text(&request) {
            Ok(TelegramEditMessageOutcome::Updated(message)) => {
                state.slots.insert(slot.clone(), message.message_id);
            }
            Ok(TelegramEditMessageOutcome::Unchanged) => {}
            Err(error)
                if parse_mode == TelegramParseMode::MarkdownV2
                    && error.is_formatting_rejection() =>
            {
                client.edit_message_text(&TelegramEditMessageText {
                    parse_mode: None,
                    ..request
                })?;
            }
            Err(error) => return Err(error.into()),
        }
        return Ok(());
    }

    let reply_to = reply_target(state, slot);
    let request = TelegramSendMessage {
        chat_id: identity.chat_id,
        text: text.to_owned(),
        message_thread_id: identity.topic_id,
        parse_mode: bot_parse_mode,
        reply_parameters: reply_to.map(|message_id| TelegramReplyParameters { message_id }),
        reply_markup: reply_markup.clone(),
        link_preview_options: link_preview_options.clone(),
    };
    let message = match client.send_message(&request) {
        Ok(message) => message,
        Err(error)
            if parse_mode == TelegramParseMode::MarkdownV2 && error.is_formatting_rejection() =>
        {
            client.send_message(&TelegramSendMessage {
                parse_mode: None,
                ..request
            })?
        }
        Err(error) => return Err(error.into()),
    };
    state.slots.insert(slot.clone(), message.message_id);
    Ok(())
}

fn reply_target(state: &TelegramDeliveryState, slot: &TelegramMessageSlot) -> Option<i64> {
    match slot {
        TelegramMessageSlot::Preview(index) if *index > 0 => state
            .slots
            .get(&TelegramMessageSlot::Preview(index.saturating_sub(1)))
            .copied()
            .or(state.source_message_id),
        _ => state.source_message_id,
    }
}

fn bot_parse_mode(parse_mode: TelegramParseMode) -> Option<TelegramBotParseMode> {
    match parse_mode {
        TelegramParseMode::Plain => None,
        TelegramParseMode::MarkdownV2 => Some(TelegramBotParseMode::MarkdownV2),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_keyboard(
    gateway: &mut TelegramGateway,
    identity: &TelegramIdentity,
    session_id: &str,
    turn_id: Option<&str>,
    slot: &TelegramMessageSlot,
    buttons: &[TelegramRenderButton],
    mini_app_url: Option<&str>,
    now: OffsetDateTime,
) -> Result<Option<TelegramInlineKeyboardMarkup>, TelegramSessionServiceError> {
    let mut rows = Vec::new();
    for button in buttons {
        let rendered = match &button.intent {
            TelegramButtonIntent::StartLiveVoice => {
                mini_app_url.map(|url| TelegramBotInlineButton {
                    text: button.label.clone(),
                    callback_data: None,
                    web_app: Some(TelegramWebAppInfo {
                        url: url.to_owned(),
                    }),
                })
            }
            intent => {
                let command = command_for_intent(intent);
                let group_id = callback_group_id(slot, intent)?;
                let callback = gateway.issue_command_callback(
                    identity,
                    session_id,
                    turn_id,
                    &group_id,
                    &button.label,
                    command,
                    now + CALLBACK_LIFETIME,
                    now,
                )?;
                Some(TelegramBotInlineButton {
                    text: callback.label,
                    callback_data: Some(callback.callback_data),
                    web_app: None,
                })
            }
        };
        if let Some(rendered) = rendered {
            rows.push(vec![rendered]);
        }
    }
    Ok((!rows.is_empty()).then_some(TelegramInlineKeyboardMarkup {
        inline_keyboard: rows,
    }))
}

fn callback_group_id(
    slot: &TelegramMessageSlot,
    intent: &TelegramButtonIntent,
) -> Result<String, TelegramSessionServiceError> {
    let slot = slot_fingerprint(slot)?;
    Ok(match intent {
        TelegramButtonIntent::Details { reference } => {
            format!("render:{slot}:details:{}", digest_prefix(reference))
        }
        TelegramButtonIntent::AnswerQuestion { question_id, .. } => {
            format!("render:{slot}:question:{}", digest_prefix(question_id))
        }
        TelegramButtonIntent::Approval { approval_id, .. } => {
            format!("render:{slot}:approval:{}", digest_prefix(approval_id))
        }
        TelegramButtonIntent::CancelQueued => format!("render:{slot}:cancel"),
        TelegramButtonIntent::StartLiveVoice => format!("render:{slot}:voice"),
    })
}

fn command_for_intent(intent: &TelegramButtonIntent) -> FrontendCommand {
    match intent {
        TelegramButtonIntent::AnswerQuestion { question_id, value } => {
            FrontendCommand::AnswerQuestion {
                question_id: question_id.clone(),
                answer: value.clone(),
            }
        }
        TelegramButtonIntent::Approval {
            approval_id,
            decision,
        } => FrontendCommand::ResolveApproval {
            approval_id: approval_id.clone(),
            decision: *decision,
        },
        TelegramButtonIntent::Details { .. } => FrontendCommand::ShowStatus,
        TelegramButtonIntent::CancelQueued => FrontendCommand::CancelTurn,
        TelegramButtonIntent::StartLiveVoice => FrontendCommand::ShowStatus,
    }
}

fn slot_fingerprint(slot: &TelegramMessageSlot) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(slot)?;
    Ok(hex::encode(Sha256::digest(bytes))[..24].to_owned())
}

fn digest_prefix(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))[..24].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_chunks_reply_to_the_previous_preview() {
        let mut state = TelegramDeliveryState {
            source_message_id: Some(7),
            ..TelegramDeliveryState::default()
        };
        state.slots.insert(TelegramMessageSlot::Preview(0), 10);
        assert_eq!(
            reply_target(&state, &TelegramMessageSlot::Preview(1)),
            Some(10)
        );
        assert_eq!(
            reply_target(&state, &TelegramMessageSlot::Progress),
            Some(7)
        );
    }

    #[test]
    fn renderer_intents_map_only_to_shared_control_commands() {
        assert!(matches!(
            command_for_intent(&TelegramButtonIntent::AnswerQuestion {
                question_id: "q".to_owned(),
                value: "yes".to_owned(),
            }),
            FrontendCommand::AnswerQuestion { .. }
        ));
        assert_eq!(
            command_for_intent(&TelegramButtonIntent::CancelQueued),
            FrontendCommand::CancelTurn
        );
    }

    #[test]
    fn details_callbacks_do_not_consume_approval_or_question_resolution() {
        let approval_slot = TelegramMessageSlot::Approval("approval-1".to_owned());
        let details = callback_group_id(
            &approval_slot,
            &TelegramButtonIntent::Details {
                reference: "approval-1".to_owned(),
            },
        )
        .expect("callback group");
        let approve = callback_group_id(
            &approval_slot,
            &TelegramButtonIntent::Approval {
                approval_id: "approval-1".to_owned(),
                decision: medusa_protocol::frontend::ApprovalDecision::ApproveOnce,
            },
        )
        .expect("callback group");
        let deny = callback_group_id(
            &approval_slot,
            &TelegramButtonIntent::Approval {
                approval_id: "approval-1".to_owned(),
                decision: medusa_protocol::frontend::ApprovalDecision::Deny,
            },
        )
        .expect("callback group");
        assert_ne!(details, approve);
        assert_eq!(approve, deny);

        let question_slot = TelegramMessageSlot::Question("question-1".to_owned());
        let inspect = callback_group_id(
            &question_slot,
            &TelegramButtonIntent::Details {
                reference: "question-1".to_owned(),
            },
        )
        .expect("callback group");
        let answer = callback_group_id(
            &question_slot,
            &TelegramButtonIntent::AnswerQuestion {
                question_id: "question-1".to_owned(),
                value: "yes".to_owned(),
            },
        )
        .expect("callback group");
        assert_ne!(inspect, answer);
    }
}
