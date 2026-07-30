from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(".")


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if old not in text:
        raise SystemExit(f"expected source fragment not found: {label}")
    return text.replace(old, new, 1)


def replace_regex(text: str, pattern: str, replacement: str, label: str) -> str:
    updated, count = re.subn(pattern, replacement, text, count=1, flags=re.S)
    if count != 1:
        raise SystemExit(f"expected source region not found: {label}")
    return updated


def patch_callback() -> None:
    path = ROOT / "crates/medusa-daemon/src/telegram/callback.rs"
    text = path.read_text()
    if "for record in self.records.values_mut()" in text:
        return

    text = replace_once(text, ".get_mut(nonce)", ".get(nonce)", "callback immutable lookup")
    text = replace_once(
        text,
        "if record.expires_at < now {",
        "if record.expires_at <= now {",
        "callback expiry boundary",
    )
    text = replace_once(
        text,
        "        record.consumed_at = Some(now);\n",
        "",
        "early callback consumption",
    )
    text = replace_once(
        text,
        "            session_id: Some(resolved.session_id),\n            turn_id: resolved.turn_id,",
        "            session_id: Some(resolved.session_id.clone()),\n            turn_id: resolved.turn_id.clone(),",
        "resolved session clones",
    )
    text = replace_once(
        text,
        "                approval_id: resolved.approval_id,",
        "                approval_id: resolved.approval_id.clone(),",
        "resolved approval clone",
    )
    validation = """        envelope
            .validate()
            .map_err(|error| TelegramGatewayError::Protocol(error.to_owned()))?;
"""
    consumption = validation + """        for record in self.records.values_mut() {
            if record.user_id == identity.user_id
                && record.chat_id == identity.chat_id
                && record.topic_id == identity.topic_id
                && record.session_id == resolved.session_id
                && record.turn_id == resolved.turn_id
                && record.approval_id == resolved.approval_id
            {
                record.consumed_at = Some(now);
            }
        }
"""
    text = replace_once(text, validation, consumption, "sibling callback consumption")
    first_assertion = """        assert!(matches!(
            store.resolve(&identity(), &buttons[0].callback_data, now),
            Err(TelegramGatewayError::CallbackAlreadyResolved)
        ));
"""
    sibling_assertion = first_assertion + """        assert!(matches!(
            store.resolve(&identity(), &buttons[1].callback_data, now),
            Err(TelegramGatewayError::CallbackAlreadyResolved)
        ));
"""
    text = replace_once(
        text,
        first_assertion,
        sibling_assertion,
        "sibling callback regression test",
    )
    path.write_text(text)


def patch_renderer() -> None:
    path = ROOT / "crates/medusa-daemon/src/telegram/render.rs"
    text = path.read_text()
    if "struct RenderedEvent" in text:
        return

    text = replace_once(
        text,
        "use serde::{Deserialize, Serialize};\nuse time::{Duration, OffsetDateTime};",
        "use serde::{Deserialize, Serialize};\nuse sha2::{Digest, Sha256};\nuse time::{Duration, OffsetDateTime};",
        "renderer digest import",
    )
    text = replace_once(
        text,
        "#[derive(Clone, Debug, Deserialize, Serialize)]\npub struct TelegramRenderer {",
        """#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RenderedEvent {
    event_id: String,
    fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TelegramRenderer {""",
        "rendered event state",
    )
    text = replace_once(
        text,
        "    cursor_events: BTreeMap<u64, String>,",
        "    cursor_events: BTreeMap<u64, RenderedEvent>,",
        "renderer cursor state",
    )
    text = replace_once(
        text,
        """        if self.already_rendered(envelope)? {
            return Ok(Vec::new());
        }
""",
        """        let fingerprint = event_fingerprint(envelope)?;
        if self.already_rendered(envelope, &fingerprint)? {
            return Ok(Vec::new());
        }
""",
        "renderer fingerprint validation",
    )
    preview_inner = """                self.preview.push_str(text);
                if self.should_flush(now) {
                    actions.extend(self.flush_preview(true, now)?);
                }
"""
    preview_safe = """                let previous_len = self.preview.len();
                self.preview.push_str(text);
                if self.should_flush(now) {
                    match self.flush_preview(true, now) {
                        Ok(flushed) => actions.extend(flushed),
                        Err(error) => {
                            self.preview.truncate(previous_len);
                            return Err(error);
                        }
                    }
                }
"""
    text = replace_once(text, preview_inner, preview_safe, "renderer preview rollback")
    text = replace_once(
        text,
        "        Ok(actions)\n    }\n\n    fn already_rendered(",
        "        self.record_rendered(envelope, fingerprint);\n        Ok(actions)\n    }\n\n    fn already_rendered(",
        "renderer commit after success",
    )
    text = replace_regex(
        text,
        r"    fn already_rendered\(.*?\n    fn should_flush",
        """    fn already_rendered(
        &self,
        envelope: &FrontendEventEnvelope,
        fingerprint: &str,
    ) -> Result<bool, TelegramGatewayError> {
        if let Some(existing) = self.cursor_events.get(&envelope.cursor) {
            if existing.event_id == envelope.event_id && existing.fingerprint == fingerprint {
                return Ok(true);
            }
            return Err(TelegramGatewayError::CursorConflict(envelope.cursor));
        }
        if self
            .cursor_events
            .last_key_value()
            .is_some_and(|(cursor, _)| envelope.cursor < *cursor)
        {
            return Err(TelegramGatewayError::StaleCursor(envelope.cursor));
        }
        Ok(false)
    }

    fn record_rendered(&mut self, envelope: &FrontendEventEnvelope, fingerprint: String) {
        self.cursor_events.insert(
            envelope.cursor,
            RenderedEvent {
                event_id: envelope.event_id.clone(),
                fingerprint,
            },
        );
        while self.cursor_events.len() > MAX_REPLAY_RECORDS {
            let Some(first) = self
                .cursor_events
                .first_key_value()
                .map(|(cursor, _)| *cursor)
            else {
                break;
            };
            self.cursor_events.remove(&first);
        }
    }

    fn should_flush""",
        "renderer replay functions",
    )
    text = replace_once(
        text,
        "fn preview_slot(index: usize) -> Result<TelegramMessageSlot, TelegramGatewayError> {",
        """fn event_fingerprint(
    envelope: &FrontendEventEnvelope,
) -> Result<String, TelegramGatewayError> {
    let encoded = serde_json::to_vec(envelope)
        .map_err(|error| TelegramGatewayError::Protocol(error.to_string()))?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

fn preview_slot(index: usize) -> Result<TelegramMessageSlot, TelegramGatewayError> {""",
        "renderer fingerprint helper",
    )
    conflict = """        assert!(matches!(
            renderer.render(&conflict, conflict.timestamp),
            Err(TelegramGatewayError::CursorConflict(1))
        ));
"""
    altered = conflict + """
        let mut altered = started.clone();
        altered.event = FrontendEvent::SubmissionAccepted;
        assert!(matches!(
            renderer.render(&altered, altered.timestamp),
            Err(TelegramGatewayError::CursorConflict(1))
        ));
"""
    text = replace_once(text, conflict, altered, "renderer conflicting payload test")
    path.write_text(text)


patch_callback()
patch_renderer()
