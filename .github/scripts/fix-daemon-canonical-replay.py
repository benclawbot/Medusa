from __future__ import annotations

from pathlib import Path


def replace_once(text: str, old: str, new: str, path: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one compatibility anchor, found {count}")
    return text.replace(old, new, 1)


live_path = Path("crates/medusa-daemon/src/live_session.rs")
live = live_path.read_text(encoding="utf-8")
canonical_replay = '''    pub fn replay(
        &self,
        client_id: &str,
        cursor: u64,
    ) -> Result<LiveSessionReplayView, LiveSessionBrokerError> {
        let attachment = self.attachment(client_id)?;
        let client_kind = attachment
            .continuity
            .attachments
            .iter()
            .find(|candidate| candidate.client_id == client_id)
            .map(|candidate| candidate.client_kind.clone())
            .ok_or_else(|| LiveSessionBrokerError::ClientNotAttached(client_id.to_owned()))?;
        let replay = attachment.replay_from(cursor)?;
        Ok(replay_view(attachment, &client_kind, cursor, replay))
    }
'''
compatibility_replay = '''    /// Replays raw journal envelopes for the bounded Telegram compatibility path.
    ///
    /// Remove this method when Telegram consumes `FrontendEventEnvelope` directly.
    pub(crate) fn replay_raw(
        &self,
        client_id: &str,
        cursor: u64,
    ) -> Result<Vec<EventEnvelope>, LiveSessionBrokerError> {
        self.attachment(client_id)?
            .replay_from(cursor)
            .map_err(Into::into)
    }

    /// Replays frontend-scoped canonical events and advances across hidden journal entries.
    pub fn replay(
        &self,
        client_id: &str,
        cursor: u64,
    ) -> Result<LiveSessionReplayView, LiveSessionBrokerError> {
        let attachment = self.attachment(client_id)?;
        let client_kind = attachment
            .continuity
            .attachments
            .iter()
            .find(|candidate| candidate.client_id == client_id)
            .map(|candidate| candidate.client_kind.clone())
            .ok_or_else(|| LiveSessionBrokerError::ClientNotAttached(client_id.to_owned()))?;
        let replay = attachment.replay_from(cursor)?;
        Ok(replay_view(attachment, &client_kind, cursor, replay))
    }
'''
live = replace_once(live, canonical_replay, compatibility_replay, str(live_path))
live_path.write_text(live, encoding="utf-8")

control_path = Path("crates/medusa-daemon/src/frontend_control.rs")
control = control_path.read_text(encoding="utf-8")
control = replace_once(
    control,
    '''use medusa_protocol::frontend::{
    ApprovalDecision, AttachmentMode as FrontendAttachmentMode, FrontendCommand,
    FrontendCommandEnvelope, FrontendKind,
};
''',
    '''use medusa_protocol::{
    EventEnvelope,
    frontend::{
        ApprovalDecision, AttachmentMode as FrontendAttachmentMode, FrontendCommand,
        FrontendCommandEnvelope, FrontendKind,
    },
};
''',
    str(control_path),
)
canonical_control_replay = '''    /// Replays canonical journal events for one process-local attached client.
    pub fn replay_events(
        &self,
        client_id: &str,
        cursor: u64,
    ) -> Result<LiveSessionReplayView, FrontendControlError> {
        self.broker.replay(client_id, cursor).map_err(Into::into)
    }
'''
control_replay_with_compatibility = '''    /// Replays canonical journal events for one process-local attached client.
    pub fn replay_events(
        &self,
        client_id: &str,
        cursor: u64,
    ) -> Result<LiveSessionReplayView, FrontendControlError> {
        self.broker.replay(client_id, cursor).map_err(Into::into)
    }

    /// Temporary raw-journal replay used only by the legacy Telegram delivery adapter.
    pub(crate) fn replay_raw_events(
        &self,
        client_id: &str,
        cursor: u64,
    ) -> Result<Vec<EventEnvelope>, FrontendControlError> {
        self.broker
            .replay_raw(client_id, cursor)
            .map_err(Into::into)
    }
'''
control = replace_once(
    control,
    canonical_control_replay,
    control_replay_with_compatibility,
    str(control_path),
)
control_path.write_text(control, encoding="utf-8")

telegram_path = Path("crates/medusa-daemon/src/telegram/service.rs")
telegram = telegram_path.read_text(encoding="utf-8")
telegram = replace_once(
    telegram,
    ".replay_events(&binding.client_id, binding.acknowledged_cursor)",
    ".replay_raw_events(&binding.client_id, binding.acknowledged_cursor)",
    str(telegram_path),
)
telegram = replace_once(
    telegram,
    "        let FrontendControlResult::Attached { attachment } = acknowledgement.result else {\n",
    "        let FrontendControlResult::Attached { .. } = acknowledgement.result else {\n",
    str(telegram_path),
)
telegram = replace_once(
    telegram,
    "        Ok(attachment.replay)\n",
    "        self.control\n"
    "            .replay_raw_events(&binding.client_id, binding.acknowledged_cursor)\n"
    "            .map_err(Into::into)\n",
    str(telegram_path),
)
telegram = replace_once(
    telegram,
    "            | FrontendControlResult::Events { session_id, .. } => Some(session_id.clone()),\n",
    "            | FrontendControlResult::Events { replay } => Some(replay.session_id.clone()),\n",
    str(telegram_path),
)
telegram_path.write_text(telegram, encoding="utf-8")
