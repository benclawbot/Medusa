use std::{fs, path::Path};

pub fn run() {
    patch_runtime();
    patch_service();
    patch_mini_app_http();
}

fn patch_runtime() {
    let path = Path::new("src/telegram/runtime.rs");
    let mut source = read(path);
    if !source.contains("use super::bot_api::TelegramUpdate;") {
        replace_required(
            &mut source,
            "use time::OffsetDateTime;\n",
            "use time::OffsetDateTime;\n\nuse super::bot_api::TelegramUpdate;\n",
        );
    }
    if !source.contains("// issue-568-text-fragment-methods") {
        let marker = "    fn stage_file(\n";
        let methods = r#"    // issue-568-text-fragment-methods
    fn queue_text_fragment(
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
        replace_required(&mut source, marker, &format!("{methods}{marker}"));
    }
    write(path, source);
}

fn patch_service() {
    let path = Path::new("src/telegram/service.rs");
    let mut source = read(path);
    if !source.contains("use super::bot_api::TelegramOutboundFile;") {
        replace_required(
            &mut source,
            "use thiserror::Error;\n",
            "use thiserror::Error;\n\nuse super::bot_api::TelegramOutboundFile;\n",
        );
    }
    if !source.contains("// issue-568-service-error-fixups") {
        let marker = "    #[error(transparent)]\n    Gateway(#[from] TelegramGatewayError),\n";
        let variants = r#"    // issue-568-service-error-fixups
    #[error("Telegram frontend command is invalid: {0}")]
    InvalidCommand(String),
    #[error(transparent)]
    MiniApp(#[from] super::TelegramMiniAppError),
"#;
        replace_required(&mut source, marker, &format!("{variants}{marker}"));
    }
    write(path, source);
}

fn patch_mini_app_http() {
    let path = Path::new("src/telegram/mini_app_http.rs");
    let mut source = read(path);
    if source.contains("TelegramMiniAppRealtimeSession") {
        replace_required(
            &mut source,
            "    TelegramIdentity, TelegramMiniAppBridge, TelegramMiniAppError, TelegramMiniAppRealtimeSession,\n",
            "    TelegramIdentity, TelegramMiniAppBridge, TelegramMiniAppError,\n",
        );
    }
    if !source.contains("// issue-568-owned-request-path") {
        replace_required(
            &mut source,
            "    let path = target.split('?').next().ok_or(RequestRejection::Malformed)?;\n",
            "    // issue-568-owned-request-path\n    let path = target\n        .split('?')\n        .next()\n        .ok_or(RequestRejection::Malformed)?\n        .to_owned();\n",
        );
        replace_required(&mut source, "        path: path.to_owned(),\n", "        path,\n");
    }
    write(path, source);
}

fn replace_required(source: &mut String, old: &str, new: &str) {
    let count = source.matches(old).count();
    if count != 1 {
        fail(&format!("expected one source match, found {count}: {old:?}"));
    }
    *source = source.replacen(old, new, 1);
}

fn read(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => fail(&format!("cannot read {}: {error}", path.display())),
    }
}

fn write(path: &Path, source: String) {
    if let Err(error) = fs::write(path, source) {
        fail(&format!("cannot write {}: {error}", path.display()));
    }
}

fn fail(message: &str) -> ! {
    eprintln!("cargo:warning={message}");
    std::process::exit(1)
}
