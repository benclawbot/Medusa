//! Durable coalescing for rapid plain-text Telegram fragments.
//!
//! Telegram clients can split one intended prompt into several messages. The buffer advances the
//! Bot API cursor only after each fragment is persisted, then submits one stable prompt after a
//! short quiet period or before a later non-fragment message from the same conversation.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{TelegramRuntimeError, bot_api::TelegramBotMessage, utf16_len};

const TEXT_FRAGMENT_SCHEMA_VERSION: u32 = 1;
const TEXT_FRAGMENT_QUIET_PERIOD_MS: i64 = 600;
const MAX_PENDING_TEXT_GROUPS: usize = 256;
const MAX_TEXT_FRAGMENT_MESSAGES: usize = 8;
const MAX_TEXT_FRAGMENT_UTF16: usize = 16_000;
const TEXT_FRAGMENT_STATE_ENV: &str = "MEDUSA_TELEGRAM_TEXT_FRAGMENT_STATE_PATH";
const DEFAULT_TEXT_FRAGMENT_STATE_PATH: &str = ".medusa/telegram-text-fragments.json";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingTextFragment {
    pub(crate) highest_update_id: i64,
    updated_at_unix_ms: i64,
    pub(crate) messages: Vec<TelegramBotMessage>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingTextFragmentState {
    schema_version: u32,
    groups: BTreeMap<String, PendingTextFragment>,
    #[serde(skip)]
    path: PathBuf,
}

impl PendingTextFragmentState {
    pub(crate) fn load() -> Result<Self, TelegramRuntimeError> {
        let path = std::env::var_os(TEXT_FRAGMENT_STATE_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_TEXT_FRAGMENT_STATE_PATH));
        if !path.is_file() {
            return Ok(Self {
                schema_version: TEXT_FRAGMENT_SCHEMA_VERSION,
                groups: BTreeMap::new(),
                path,
            });
        }
        let bytes = fs::read(&path)?;
        let mut state: Self = serde_json::from_slice(&bytes)?;
        state.path = path;
        state.validate()?;
        Ok(state)
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        Self {
            schema_version: TEXT_FRAGMENT_SCHEMA_VERSION,
            groups: BTreeMap::new(),
            path: PathBuf::new(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    pub(crate) fn insert(
        &mut self,
        update_id: i64,
        message: TelegramBotMessage,
        now: OffsetDateTime,
    ) -> Result<(), TelegramRuntimeError> {
        if update_id < 0 || !is_text_fragment_candidate(&message) {
            return Err(TelegramRuntimeError::InvalidUpdate);
        }
        let key = conversation_key(&message).ok_or(TelegramRuntimeError::InvalidUpdate)?;
        if !self.groups.contains_key(&key) && self.groups.len() >= MAX_PENDING_TEXT_GROUPS {
            return Err(TelegramRuntimeError::TooManyPendingTextFragments);
        }
        let now_ms = unix_millis(now)?;
        let group = self
            .groups
            .entry(key)
            .or_insert_with(|| PendingTextFragment {
                highest_update_id: update_id,
                updated_at_unix_ms: now_ms,
                messages: Vec::new(),
            });
        if let Some(first) = group.messages.first()
            && !same_conversation(first, &message)
        {
            return Err(TelegramRuntimeError::InvalidTextFragmentState);
        }
        if !group
            .messages
            .iter()
            .any(|candidate| candidate.message_id == message.message_id)
        {
            if group.messages.len() >= MAX_TEXT_FRAGMENT_MESSAGES {
                return Err(TelegramRuntimeError::TextFragmentGroupTooLarge);
            }
            group.messages.push(message);
            group.messages.sort_by_key(|candidate| candidate.message_id);
        }
        group.highest_update_id = group.highest_update_id.max(update_id);
        group.updated_at_unix_ms = now_ms;
        validate_group(group)?;
        Ok(())
    }

    pub(crate) fn due_keys(
        &self,
        now: OffsetDateTime,
    ) -> Result<Vec<String>, TelegramRuntimeError> {
        let now_ms = unix_millis(now)?;
        Ok(self
            .groups
            .iter()
            .filter(|(_, group)| {
                now_ms.saturating_sub(group.updated_at_unix_ms) >= TEXT_FRAGMENT_QUIET_PERIOD_MS
            })
            .map(|(key, _)| key.clone())
            .collect())
    }

    pub(crate) fn conversation_keys(&self, message: &TelegramBotMessage) -> Vec<String> {
        self.groups
            .iter()
            .filter(|(_, group)| {
                group
                    .messages
                    .first()
                    .is_some_and(|first| same_conversation(first, message))
            })
            .map(|(key, _)| key.clone())
            .collect()
    }

    pub(crate) fn get(&self, key: &str) -> Option<&PendingTextFragment> {
        self.groups.get(key)
    }

    pub(crate) fn remove(&mut self, key: &str) {
        self.groups.remove(key);
    }

    pub(crate) fn persist(&self) -> Result<(), TelegramRuntimeError> {
        self.validate()?;
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        if self.groups.is_empty() {
            if self.path.is_file() {
                fs::remove_file(&self.path)?;
            }
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("tmp");
        let bytes = serde_json::to_vec_pretty(self)?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        if self.path.is_file() {
            fs::remove_file(&self.path)?;
        }
        fs::rename(temporary, &self.path)?;
        Ok(())
    }

    fn validate(&self) -> Result<(), TelegramRuntimeError> {
        if self.schema_version != TEXT_FRAGMENT_SCHEMA_VERSION
            || self.groups.len() > MAX_PENDING_TEXT_GROUPS
            || self.groups.iter().any(|(key, group)| {
                group.messages.first().and_then(conversation_key).as_deref() != Some(key.as_str())
                    || validate_group(group).is_err()
            })
        {
            return Err(TelegramRuntimeError::InvalidTextFragmentState);
        }
        Ok(())
    }
}

pub(crate) fn is_text_fragment_candidate(message: &TelegramBotMessage) -> bool {
    message.media_group_id.is_none()
        && message.photo.is_empty()
        && message.document.is_none()
        && message.caption.is_none()
        && message.text.as_deref().map(str::trim).is_some_and(|text| {
            !text.is_empty() && !text.starts_with('/') && utf16_len(text) <= MAX_TEXT_FRAGMENT_UTF16
        })
        && message.from.as_ref().is_some_and(|user| !user.is_bot)
}

pub(crate) fn merge_text_fragment(
    group: &PendingTextFragment,
) -> Result<TelegramBotMessage, TelegramRuntimeError> {
    validate_group(group)?;
    let mut merged = group
        .messages
        .first()
        .cloned()
        .ok_or(TelegramRuntimeError::InvalidTextFragmentState)?;
    merged.message_id = group
        .messages
        .iter()
        .map(|message| message.message_id)
        .min()
        .ok_or(TelegramRuntimeError::InvalidTextFragmentState)?;
    let mut parts = Vec::new();
    for message in &group.messages {
        let text = message
            .text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or(TelegramRuntimeError::InvalidTextFragmentState)?;
        parts.push(text);
    }
    merged.text = Some(parts.join("\n"));
    Ok(merged)
}

fn validate_group(group: &PendingTextFragment) -> Result<(), TelegramRuntimeError> {
    if group.highest_update_id < 0
        || group.updated_at_unix_ms < 0
        || group.messages.is_empty()
        || group.messages.len() > MAX_TEXT_FRAGMENT_MESSAGES
        || group
            .messages
            .iter()
            .any(|message| !is_text_fragment_candidate(message))
    {
        return Err(TelegramRuntimeError::InvalidTextFragmentState);
    }
    let first = group
        .messages
        .first()
        .ok_or(TelegramRuntimeError::InvalidTextFragmentState)?;
    if group
        .messages
        .iter()
        .any(|message| !same_conversation(first, message))
    {
        return Err(TelegramRuntimeError::InvalidTextFragmentState);
    }
    let total = group
        .messages
        .iter()
        .filter_map(|message| message.text.as_deref())
        .map(utf16_len)
        .fold(0_usize, usize::saturating_add);
    if total > MAX_TEXT_FRAGMENT_UTF16 {
        return Err(TelegramRuntimeError::TextFragmentGroupTooLarge);
    }
    Ok(())
}

fn conversation_key(message: &TelegramBotMessage) -> Option<String> {
    let user_id = message.from.as_ref()?.id;
    Some(format!(
        "{}:{}:{}",
        message.chat.id,
        message.message_thread_id.unwrap_or_default(),
        user_id
    ))
}

fn same_conversation(current: &TelegramBotMessage, next: &TelegramBotMessage) -> bool {
    current.chat.id == next.chat.id
        && current.message_thread_id == next.message_thread_id
        && current.from.as_ref().map(|user| user.id) == next.from.as_ref().map(|user| user.id)
}

fn unix_millis(now: OffsetDateTime) -> Result<i64, TelegramRuntimeError> {
    i64::try_from(now.unix_timestamp_nanos() / 1_000_000)
        .map_err(|_| TelegramRuntimeError::InvalidTextFragmentState)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::bot_api::{TelegramBotChat, TelegramBotChatKind, TelegramBotUser};
    use time::{Duration, macros::datetime};

    fn message(id: i64, text: &str) -> TelegramBotMessage {
        TelegramBotMessage {
            message_id: id,
            date: 1_700_000_000,
            chat: TelegramBotChat {
                id: 42,
                kind: TelegramBotChatKind::Private,
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
            message_thread_id: None,
            media_group_id: None,
            photo: Vec::new(),
            document: None,
            text: Some(text.to_owned()),
            caption: None,
        }
    }

    #[test]
    fn rapid_fragments_merge_in_message_order() {
        let mut state = PendingTextFragmentState::in_memory();
        let now = datetime!(2026-08-01 00:00 UTC);
        state.insert(11, message(8, "second"), now).expect("second");
        state
            .insert(10, message(7, "first"), now + Duration::milliseconds(10))
            .expect("first");
        let key = state.groups.keys().next().expect("group").clone();
        let merged = merge_text_fragment(state.get(&key).expect("pending")).expect("merge");
        assert_eq!(merged.message_id, 7);
        assert_eq!(merged.text.as_deref(), Some("first\nsecond"));
    }

    #[test]
    fn slash_commands_and_attachments_are_not_buffered() {
        let mut command = message(7, "/status");
        assert!(!is_text_fragment_candidate(&command));
        command.text = Some("inspect".to_owned());
        command.document = Some(crate::telegram::bot_api::TelegramDocument {
            file_id: "file".to_owned(),
            file_unique_id: "unique".to_owned(),
            file_name: Some("notes.txt".to_owned()),
            mime_type: Some("text/plain".to_owned()),
            file_size: Some(10),
        });
        assert!(!is_text_fragment_candidate(&command));
    }

    #[test]
    fn quiet_period_is_bounded() {
        let mut state = PendingTextFragmentState::in_memory();
        let now = datetime!(2026-08-01 00:00 UTC);
        state.insert(10, message(7, "hello"), now).expect("insert");
        assert!(
            state
                .due_keys(now + Duration::milliseconds(599))
                .expect("due")
                .is_empty()
        );
        assert_eq!(
            state
                .due_keys(now + Duration::milliseconds(600))
                .expect("due")
                .len(),
            1
        );
    }
}
