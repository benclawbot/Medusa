from pathlib import Path


def replace_once(source: str, old: str, new: str, label: str) -> str:
    if new in source:
        return source
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one target, found {count}")
    return source.replace(old, new, 1)


path = Path("crates/medusa-daemon/src/telegram/mod.rs")
source = path.read_text()
source = replace_once(
    source,
    "mod render;\n",
    "mod render;\nmod service;\n",
    "Telegram service module",
)
source = replace_once(
    source,
    '''pub use render::{
    TelegramAction, TelegramButtonIntent, TelegramMessageSlot, TelegramParseMode, TelegramReaction,
    TelegramRenderButton, TelegramRenderer,
};
''',
    '''pub use render::{
    TelegramAction, TelegramButtonIntent, TelegramMessageSlot, TelegramParseMode, TelegramReaction,
    TelegramRenderButton, TelegramRenderer,
};
pub use service::{
    TelegramBindingKey, TelegramServiceOutcome, TelegramSessionBinding, TelegramSessionService,
    TelegramSessionServiceError,
};
''',
    "Telegram service exports",
)
path.write_text(source)

path = Path("crates/medusa-daemon/src/telegram/command.rs")
source = path.read_text()
source = replace_once(
    source,
    '''            FrontendCommand::Attach {
                session_id: required(arguments, "usage: /attach <session>")?.to_owned(),
                mode: AttachmentMode::Owner,
                after_cursor: None,
            },
''',
    '''            FrontendCommand::Attach {
                session_id: required(arguments, "usage: /attach <session>")?.to_owned(),
                mode: AttachmentMode::ReadOnly,
                after_cursor: None,
            },
''',
    "Telegram attach remains a frontend observer",
)
path.write_text(source)

path = Path("crates/medusa-daemon/src/telegram/service.rs")
source = path.read_text()
source = replace_once(
    source,
    '''    pub fn process_message(
        &mut self,
        update_id: i64,
        mut message: TelegramInboundMessage,
    ) -> Result<TelegramServiceOutcome, TelegramSessionServiceError> {
''',
    '''    pub fn process_message(
        &mut self,
        update_id: i64,
        message: TelegramInboundMessage,
    ) -> Result<TelegramServiceOutcome, TelegramSessionServiceError> {
''',
    "immutable Telegram message input",
)
source = replace_once(
    source,
    '''        if message.attached_session_id.is_none() {
            message.attached_session_id = existing
                .as_ref()
                .and_then(|binding| binding.session_id.clone());
        }

        let action = self.gateway.map_message(&message)?;
''',
    '''        let mut action = self.gateway.map_message(&message)?;
        if let TelegramInboundAction::Forward(envelope) = &mut action
            && envelope.session_id.is_none()
            && command_uses_current_binding(&envelope.command)
        {
            envelope.session_id = existing
                .as_ref()
                .and_then(|binding| binding.session_id.clone());
        }
''',
    "stable command identity before binding enrichment",
)
source = replace_once(
    source,
    '''            FrontendControlResult::SubmissionAccepted { session_id, .. }
            | FrontendControlResult::CancellationRequested { session_id, .. }
            | FrontendControlResult::Status { session_id, .. }
            | FrontendControlResult::Events { session_id, .. } => Some(session_id.clone()),
''',
    '''            FrontendControlResult::SubmissionAccepted { session_id, .. }
            | FrontendControlResult::CancellationRequested { session_id, .. }
            | FrontendControlResult::CommandAccepted { session_id, .. }
            | FrontendControlResult::Status { session_id, .. }
            | FrontendControlResult::Events { session_id, .. } => Some(session_id.clone()),
''',
    "Telegram command acknowledgement session binding",
)
source = replace_once(
    source,
    '''fn ensure_binding(
    key: TelegramBindingKey,
''',
    '''fn command_uses_current_binding(command: &FrontendCommand) -> bool {
    !matches!(
        command,
        FrontendCommand::CreateSession { .. }
            | FrontendCommand::ListSessions
            | FrontendCommand::ResumeSession { .. }
            | FrontendCommand::Attach { .. }
            | FrontendCommand::Detach
    )
}

fn ensure_binding(
    key: TelegramBindingKey,
''',
    "current binding command classification",
)
path.write_text(source)
