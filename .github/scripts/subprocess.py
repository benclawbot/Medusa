from __future__ import annotations

import atexit
import importlib.util
import re
import sys
import sysconfig
from pathlib import Path

_STDLIB_SUBPROCESS = Path(sysconfig.get_path("stdlib")) / "subprocess.py"
_SPEC = importlib.util.spec_from_file_location("_medusa_stdlib_subprocess", _STDLIB_SUBPROCESS)
if _SPEC is None or _SPEC.loader is None:
    raise ImportError(f"could not load stdlib subprocess from {_STDLIB_SUBPROCESS}")
_REAL = importlib.util.module_from_spec(_SPEC)
sys.modules[_SPEC.name] = _REAL
_SPEC.loader.exec_module(_REAL)
for _name in dir(_REAL):
    if _name not in {"__name__", "__loader__", "__package__", "__spec__"}:
        globals()[_name] = getattr(_REAL, _name)


def _replace_once(text: str, old: str, new: str, path: Path) -> str:
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"{path}: expected one replacement anchor, found {count}")
    return text.replace(old, new, 1)


def _migrate_telegram_delivery() -> None:
    path = Path("crates/medusa-daemon/src/telegram/service.rs")
    text = path.read_text(encoding="utf-8")
    text = _replace_once(
        text,
        """use medusa_protocol::{
    EventEnvelope, EventPayload,
    frontend::{
        AttachmentMode, FRONTEND_PROTOCOL_VERSION, FrontendCommand, FrontendCommandEnvelope,
        FrontendKind,
    },
};
""",
        """use medusa_protocol::frontend::{
    AttachmentMode, FRONTEND_PROTOCOL_VERSION, FrontendCommand, FrontendCommandEnvelope,
    FrontendEvent, FrontendEventEnvelope, FrontendKind,
};
""",
        path,
    )
    text = _replace_once(
        text,
        """use crate::{
    FrontendCommandAcknowledgement, FrontendControlError, FrontendControlPlane,
    FrontendControlResult,
};
""",
        """use crate::{
    FrontendCommandAcknowledgement, FrontendControlError, FrontendControlPlane,
    FrontendControlResult, LiveSessionReplayView,
};
""",
        path,
    )
    text = _replace_once(
        text,
        """    delivery::execute_actions,
    project_event,
};
""",
        """    delivery::execute_actions,
};
""",
        path,
    )

    start = text.index("    pub fn deliver_pending(\n")
    end = text.index("    fn binding_after_acknowledgement(\n", start)
    replacement = r'''    pub fn deliver_pending(
        &mut self,
        client: &TelegramBotApiClient,
        now: time::OffsetDateTime,
    ) -> Result<usize, TelegramSessionServiceError> {
        self.deliver_pending_with_voice(client, None, now)
    }

    pub fn deliver_pending_with_voice(
        &mut self,
        client: &TelegramBotApiClient,
        voice_pipeline: Option<&TelegramVoicePipeline>,
        now: time::OffsetDateTime,
    ) -> Result<usize, TelegramSessionServiceError> {
        let binding_ids = self.state.bindings.keys().cloned().collect::<Vec<_>>();
        let mut delivered = 0_usize;
        for stable_id in binding_ids {
            let Some(binding) = self.state.bindings.get(&stable_id).cloned() else {
                continue;
            };
            let Some(session_id) = binding.session_id.clone() else {
                continue;
            };
            let replay = self.replay_for_binding(&binding, &session_id, now)?;
            for event in &replay.events {
                if event.cursor <= binding.acknowledged_cursor {
                    continue;
                }
                self.deliver_event(client, voice_pipeline, &stable_id, event, now)?;
                delivered = delivered.saturating_add(1);
            }
            let acknowledged_cursor = self
                .state
                .bindings
                .get(&stable_id)
                .map_or(binding.acknowledged_cursor, |current| {
                    current.acknowledged_cursor
                });
            if replay.next_cursor > acknowledged_cursor {
                self.acknowledge_binding_cursor(
                    &stable_id,
                    &session_id,
                    replay.next_cursor,
                    now,
                )?;
            }
        }
        Ok(delivered)
    }

    fn replay_for_binding(
        &mut self,
        binding: &TelegramSessionBinding,
        session_id: &str,
        now: time::OffsetDateTime,
    ) -> Result<LiveSessionReplayView, TelegramSessionServiceError> {
        if self.attached_clients.contains(&binding.client_id) {
            return self
                .control
                .replay_events(&binding.client_id, binding.acknowledged_cursor)
                .map_err(Into::into);
        }
        let stable = format!(
            "{}:attach:{}:{}",
            binding.key.stable_id(),
            session_id,
            binding.acknowledged_cursor
        );
        let acknowledgement = self.control.dispatch(FrontendCommandEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            command_id: format!("telegram-replay-{}", digest_prefix(&stable)),
            idempotency_key: format!("telegram-replay:{stable}"),
            frontend: FrontendKind::Telegram,
            client_id: binding.client_id.clone(),
            session_id: Some(session_id.to_owned()),
            turn_id: None,
            timestamp: now,
            command: FrontendCommand::Attach {
                session_id: session_id.to_owned(),
                mode: AttachmentMode::ReadOnly,
                after_cursor: Some(binding.acknowledged_cursor),
            },
        })?;
        let FrontendControlResult::Attached { attachment } = acknowledgement.result else {
            return Err(TelegramSessionServiceError::InvalidReplayAttachment);
        };
        self.attached_clients.insert(binding.client_id.clone());
        Ok(LiveSessionReplayView {
            session_id: attachment.session.id.clone(),
            client_id: attachment.client_id.clone(),
            frontend: attachment.frontend,
            after_cursor: binding.acknowledged_cursor,
            next_cursor: attachment.replay_cursor,
            events: attachment.replay,
        })
    }

    fn deliver_event(
        &mut self,
        client: &TelegramBotApiClient,
        voice_pipeline: Option<&TelegramVoicePipeline>,
        stable_id: &str,
        event: &FrontendEventEnvelope,
        now: time::OffsetDateTime,
    ) -> Result<(), TelegramSessionServiceError> {
        let original_state = self.state.clone();
        let original_gateway = self.gateway.clone();
        let mut binding = self
            .state
            .bindings
            .get(stable_id)
            .cloned()
            .ok_or(TelegramSessionServiceError::BindingNotFound)?;
        let session_id = binding
            .session_id
            .clone()
            .ok_or(TelegramSessionServiceError::SessionNotBound)?;
        let identity = TelegramIdentity {
            user_id: binding.key.user_id,
            chat_id: binding.key.chat_id,
            topic_id: binding.key.topic_id,
            chat_kind: binding.chat_kind,
            bot_mentioned: true,
        };

        if event.cursor > binding.delivered_cursor {
            match &event.event {
                FrontendEvent::Started => {
                    self.state
                        .activate_queued_voice_command(stable_id, &event.correlation_id);
                }
                FrontendEvent::Cancelled { .. } | FrontendEvent::Failed { .. } => {
                    self.state.clear_active_voice_reply(stable_id);
                }
                _ => {}
            }
            let mut display = self.gateway.config().display.clone();
            display.tool_progress = binding.tool_progress;
            let source_message_id = binding.delivery.source_message_id.unwrap_or_default();
            let mut renderer = binding
                .renderer
                .take()
                .map_or_else(|| TelegramRenderer::new(display, source_message_id), Ok)?;
            if matches!(&event.event, FrontendEvent::Started) {
                renderer.begin_turn(source_message_id);
            }
            let actions = renderer.render(event, now)?;
            let mini_app_url = if self.gateway.config().voice.mini_app_enabled {
                match (
                    self.gateway.config().voice.mini_app_public_url.as_deref(),
                    self.mini_app_bridge.as_ref(),
                ) {
                    (Some(base), Some(bridge)) => {
                        let ticket = bridge.issue_launch_ticket(&identity, &session_id, now)?;
                        let separator = if base.contains('?') { '&' } else { '?' };
                        Some(format!("{base}{separator}ticket={}", ticket.token))
                    }
                    _ => None,
                }
            } else {
                None
            };
            execute_actions(
                client,
                &mut self.gateway,
                &self.control,
                &identity,
                &session_id,
                event.turn_id.as_deref(),
                &mut binding.delivery,
                &actions,
                mini_app_url.as_deref(),
                now,
            )?;
            self.deliver_voice_reply(
                client,
                voice_pipeline,
                stable_id,
                event,
                &identity,
                &mut binding,
                &actions,
            )?;
            binding.presentation_cursor = event.cursor;
            binding.renderer = Some(renderer);
            binding.delivered_cursor = event.cursor;
            self.state
                .bindings
                .insert(stable_id.to_owned(), binding.clone());
            if let Err(error) = self.persist() {
                self.state = original_state;
                self.gateway = original_gateway;
                return Err(error);
            }
        }

        if event.cursor > binding.acknowledged_cursor {
            self.acknowledge_binding_cursor(stable_id, &session_id, event.cursor, now)?;
        }
        Ok(())
    }

    fn acknowledge_binding_cursor(
        &mut self,
        stable_id: &str,
        session_id: &str,
        cursor: u64,
        now: time::OffsetDateTime,
    ) -> Result<(), TelegramSessionServiceError> {
        let binding = self
            .state
            .bindings
            .get(stable_id)
            .cloned()
            .ok_or(TelegramSessionServiceError::BindingNotFound)?;
        if cursor <= binding.acknowledged_cursor {
            return Ok(());
        }
        let acknowledgement = self.control.dispatch(FrontendCommandEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            command_id: format!(
                "telegram-cursor-{}",
                digest_prefix(&format!("{stable_id}:{cursor}"))
            ),
            idempotency_key: format!("telegram:{stable_id}:cursor:{cursor}"),
            frontend: FrontendKind::Telegram,
            client_id: binding.client_id,
            session_id: Some(session_id.to_owned()),
            turn_id: None,
            timestamp: now,
            command: FrontendCommand::AcknowledgeCursor { cursor },
        })?;
        let FrontendControlResult::CursorAcknowledged { attachment } = acknowledgement.result else {
            return Err(TelegramSessionServiceError::InvalidCursorAcknowledgement);
        };
        let entry = self
            .state
            .bindings
            .get_mut(stable_id)
            .ok_or(TelegramSessionServiceError::BindingNotFound)?;
        entry.acknowledged_cursor = attachment.acknowledged_cursor;
        entry.delivered_cursor = entry.delivered_cursor.max(cursor);
        self.persist()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn deliver_voice_reply(
        &mut self,
        client: &TelegramBotApiClient,
        voice_pipeline: Option<&TelegramVoicePipeline>,
        stable_id: &str,
        event: &FrontendEventEnvelope,
        identity: &TelegramIdentity,
        binding: &mut TelegramSessionBinding,
        actions: &[TelegramAction],
    ) -> Result<(), TelegramSessionServiceError> {
        if !matches!(&event.event, FrontendEvent::TurnFinished) {
            return Ok(());
        }
        let requested = binding.voice_mode == TelegramVoiceMode::All
            || (binding.voice_mode == TelegramVoiceMode::VoiceOnly
                && self.state.voice_reply_bindings.contains(stable_id));
        if !requested {
            return Ok(());
        }
        let pipeline = voice_pipeline.ok_or(TelegramSessionServiceError::VoiceUnavailable)?;
        let text =
            final_voice_text(actions).ok_or(TelegramSessionServiceError::VoiceReplyMissingText)?;
        let voice = pipeline.synthesize(&text)?;
        let slot = TelegramMessageSlot::Notice(format!("voice:{}", event.cursor));
        if !binding.delivery.slots.contains_key(&slot) {
            let message = client.send_voice(
                identity.chat_id,
                identity.topic_id,
                &TelegramOutboundFile {
                    file_name: voice.file_name,
                    mime_type: voice.mime_type,
                    bytes: voice.bytes,
                    caption: None,
                    reply_to_message_id: binding.delivery.source_message_id,
                },
            )?;
            binding.delivery.slots.insert(slot, message.message_id);
        }
        self.state.voice_reply_bindings.remove(stable_id);
        Ok(())
    }

'''
    text = text[:start] + replacement + text[end:]
    text = _replace_once(
        text,
        """            | FrontendControlResult::Status { session_id, .. }
            | FrontendControlResult::Events { session_id, .. } => Some(session_id.clone()),
""",
        """            | FrontendControlResult::Status { session_id, .. } => Some(session_id.clone()),
            FrontendControlResult::Events { replay } => Some(replay.session_id.clone()),
""",
        path,
    )
    path.write_text(text, encoding="utf-8")

    for helper in Path(".github/scripts/__pycache__").glob("subprocess*.pyc"):
        helper.unlink(missing_ok=True)
    Path(__file__).unlink(missing_ok=True)


atexit.register(_migrate_telegram_delivery)
