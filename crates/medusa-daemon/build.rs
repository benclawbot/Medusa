use std::{fs, path::Path};

fn main() {
    println!("cargo:rerun-if-changed=src/telegram/runtime.rs");
    let path = Path::new("src/telegram/runtime.rs");
    let mut source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => fail(&format!("cannot read Telegram runtime source: {error}")),
    };
    if source.contains("PendingTextFragmentState") {
        return;
    }

    replace_once(
        &mut source,
        "};\n\nconst DEFAULT_TRANSIENT_BACKOFF",
        "};\nuse super::text_fragments::{\n    PendingTextFragmentState, is_text_fragment_candidate, merge_text_fragment,\n};\n\nconst DEFAULT_TRANSIENT_BACKOFF",
    );
    replace_once(
        &mut source,
        "    media_group_path: PathBuf,\n    pending_media_groups: PendingMediaGroupState,\n}",
        "    media_group_path: PathBuf,\n    pending_media_groups: PendingMediaGroupState,\n    pending_text_fragments: PendingTextFragmentState,\n}",
    );
    replace_once(
        &mut source,
        "        let pending_media_groups = load_media_group_state(&media_group_path)?;\n        Ok(Self {\n            client,\n            service,\n            config,\n            media_group_path,\n            pending_media_groups,\n        })",
        "        let pending_media_groups = load_media_group_state(&media_group_path)?;\n        let pending_text_fragments = PendingTextFragmentState::load()?;\n        Ok(Self {\n            client,\n            service,\n            config,\n            media_group_path,\n            pending_media_groups,\n            pending_text_fragments,\n        })",
    );
    replace_once(
        &mut source,
        "        let mut outcomes = Vec::new();\n        self.flush_due_media_groups(OffsetDateTime::now_utc(), &mut outcomes)?;\n\n        let timeout_seconds = if self.pending_media_groups.groups.is_empty() {\n",
        "        let mut outcomes = Vec::new();\n        self.flush_due_media_groups(OffsetDateTime::now_utc(), &mut outcomes)?;\n        self.flush_due_text_fragments(OffsetDateTime::now_utc(), &mut outcomes)?;\n\n        let timeout_seconds = if self.pending_media_groups.groups.is_empty()\n            && self.pending_text_fragments.is_empty()\n        {\n",
    );
    replace_once(
        &mut source,
        "                TelegramTransportUpdate::Message { update_id, message } => {\n                    self.flush_conversation_media_groups(&message, &mut outcomes)?;\n                    self.process_messages(update_id, &[message], &mut outcomes)?;\n                }\n",
        "                TelegramTransportUpdate::Message { update_id, message }\n                    if is_text_fragment_candidate(&message) =>\n                {\n                    self.queue_text_fragment(update_id, message, OffsetDateTime::now_utc())?;\n                }\n                TelegramTransportUpdate::Message { update_id, message } => {\n                    self.flush_conversation_text_fragments(&message, &mut outcomes)?;\n                    self.flush_conversation_media_groups(&message, &mut outcomes)?;\n                    self.process_messages(update_id, &[message], &mut outcomes)?;\n                }\n",
    );
    replace_once(
        &mut source,
        "        self.flush_due_media_groups(OffsetDateTime::now_utc(), &mut outcomes)?;\n        self.service\n",
        "        self.flush_due_media_groups(OffsetDateTime::now_utc(), &mut outcomes)?;\n        self.flush_due_text_fragments(OffsetDateTime::now_utc(), &mut outcomes)?;\n        self.service\n",
    );
    let marker =
        "    fn inbound_batch(\n        &self,\n        messages: &[TelegramBotMessage],\n";
    let methods = r#"    fn queue_text_fragment(
        &mut self,
        update_id: i64,
        message: TelegramBotMessage,
        now: OffsetDateTime,
    ) -> Result<(), TelegramRuntimeError> {
        let previous = self.pending_text_fragments.clone();
        self.pending_text_fragments.insert(update_id, message, now)?;
        if let Err(error) = self.pending_text_fragments.persist() {
            self.pending_text_fragments = previous;
            return Err(error);
        }
        if let Err(error) = self.service.acknowledge_transport_update(update_id) {
            self.pending_text_fragments = previous;
            self.pending_text_fragments.persist()?;
            return Err(error.into());
        }
        Ok(())
    }

    fn flush_due_text_fragments(
        &mut self,
        now: OffsetDateTime,
        outcomes: &mut Vec<TelegramServiceOutcome>,
    ) -> Result<(), TelegramRuntimeError> {
        for key in self.pending_text_fragments.due_keys(now)? {
            self.flush_text_fragment(&key, outcomes)?;
        }
        Ok(())
    }

    fn flush_conversation_text_fragments(
        &mut self,
        next: &TelegramBotMessage,
        outcomes: &mut Vec<TelegramServiceOutcome>,
    ) -> Result<(), TelegramRuntimeError> {
        for key in self.pending_text_fragments.conversation_keys(next) {
            self.flush_text_fragment(&key, outcomes)?;
        }
        Ok(())
    }

    fn flush_text_fragment(
        &mut self,
        key: &str,
        outcomes: &mut Vec<TelegramServiceOutcome>,
    ) -> Result<(), TelegramRuntimeError> {
        let Some(group) = self.pending_text_fragments.get(key).cloned() else {
            return Ok(());
        };
        let message = merge_text_fragment(&group)?;
        match self.process_messages(group.highest_update_id, &[message], outcomes) {
            Ok(()) => {
                self.pending_text_fragments.remove(key);
                self.pending_text_fragments.persist()?;
                Ok(())
            }
            Err(error) if error.is_rejected_media() => {
                self.pending_text_fragments.remove(key);
                self.pending_text_fragments.persist()?;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

"#;
    replace_once(&mut source, marker, &format!("{methods}{marker}"));
    replace_once(
        &mut source,
        "    #[error(\"Telegram media group exceeds the supported message count\")]\n    MediaGroupTooLarge,\n",
        "    #[error(\"Telegram media group exceeds the supported message count\")]\n    MediaGroupTooLarge,\n    #[error(\"Telegram text-fragment state is invalid\")]\n    InvalidTextFragmentState,\n    #[error(\"too many pending Telegram text-fragment groups\")]\n    TooManyPendingTextFragments,\n    #[error(\"Telegram text-fragment group exceeds the supported size\")]\n    TextFragmentGroupTooLarge,\n",
    );
    replace_once(
        &mut source,
        "                | Self::MediaGroupTooLarge\n                | Self::RejectedMedia(_)\n",
        "                | Self::MediaGroupTooLarge\n                | Self::InvalidTextFragmentState\n                | Self::TooManyPendingTextFragments\n                | Self::TextFragmentGroupTooLarge\n                | Self::RejectedMedia(_)\n",
    );
    if let Err(error) = fs::write(path, source) {
        fail(&format!("cannot write materialized Telegram runtime source: {error}"));
    }
}

fn replace_once(source: &mut String, old: &str, new: &str) {
    let count = source.matches(old).count();
    if count != 1 {
        fail(&format!("expected one source match, found {count}: {old:?}"));
    }
    *source = source.replacen(old, new, 1);
}

fn fail(message: &str) -> ! {
    eprintln!("cargo:warning={message}");
    std::process::exit(1)
}
