use std::{fs, path::Path};

pub fn run() {
    patch_module_wiring();
    patch_runtime();
}

fn patch_module_wiring() {
    let path = Path::new("src/telegram/mod.rs");
    let mut source = read(path);
    replace_if_present(
        &mut source,
        "mod service;\n",
        "mod service;\nmod supervisor;\n",
    );
    if !source.contains("TelegramServiceSupervisor") {
        replace_if_present(
            &mut source,
            "pub use service::{\n",
            "pub use supervisor::{\n    TelegramServiceMode, TelegramServiceSupervisor, TelegramSupervisorError,\n};\npub use service::{\n",
        );
    }
    write(path, source);
}

fn patch_runtime() {
    let path = Path::new("src/telegram/runtime.rs");
    let mut source = read(path);
    replace_if_present(
        &mut source,
        "        mpsc::{Receiver, TryRecvError},\n",
        "        mpsc::{Receiver, RecvTimeoutError, TryRecvError},\n",
    );
    replace_if_present(
        &mut source,
        "        TelegramBotApiClient, TelegramBotApiError, TelegramDocument, TelegramEditMessageOutcome,\n",
        "        TelegramBotApiClient, TelegramBotApiError, TelegramDocument, TelegramEditMessageOutcome,\n        TelegramUpdate,\n",
    );
    replace_if_present(
        &mut source,
        "    mini_app_commands: Option<Receiver<TelegramMiniAppCommand>>,\n}",
        "    mini_app_commands: Option<Receiver<TelegramMiniAppCommand>>,\n    webhook_updates: Option<Receiver<TelegramUpdate>>,\n}",
    );
    replace_if_present(
        &mut source,
        "            mini_app_commands: None,\n        })",
        "            mini_app_commands: None,\n            webhook_updates: None,\n        })",
    );
    let poll_marker = "    pub fn poll_once(&mut self) -> Result<Vec<TelegramServiceOutcome>, TelegramRuntimeError> {\n";
    if !source.contains("with_webhook_updates") {
        let method = "    #[must_use]\n    pub fn with_webhook_updates(mut self, updates: Receiver<TelegramUpdate>) -> Self {\n        self.webhook_updates = Some(updates);\n        self\n    }\n\n";
        replace_required(&mut source, poll_marker, &format!("{method}{poll_marker}"));
    }
    let old = r#"        let updates = self.client.get_updates(
            self.service.next_update_offset(),
            timeout_seconds,
            self.config.limit,
        )?;
"#;
    let new = r#"        let updates = if let Some(receiver) = self.webhook_updates.as_ref() {
            let mut updates = Vec::new();
            match receiver.recv_timeout(Duration::from_secs(1)) {
                Ok(update) => updates.push(update),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(TelegramRuntimeError::WebhookDisconnected);
                }
            }
            while updates.len() < usize::from(self.config.limit) {
                match receiver.try_recv() {
                    Ok(update) => updates.push(update),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        return Err(TelegramRuntimeError::WebhookDisconnected);
                    }
                }
            }
            updates
        } else {
            self.client.get_updates(
                self.service.next_update_offset(),
                timeout_seconds,
                self.config.limit,
            )?
        };
"#;
    replace_if_present(&mut source, old, new);
    replace_if_present(
        &mut source,
        "    #[error(\"Telegram voice pipeline is not configured\")]\n    VoiceUnavailable,\n",
        "    #[error(\"Telegram voice pipeline is not configured\")]\n    VoiceUnavailable,\n    #[error(\"Telegram webhook update channel disconnected\")]\n    WebhookDisconnected,\n",
    );
    write(path, source);
}

fn replace_if_present(source: &mut String, old: &str, new: &str) {
    if source.contains(old) {
        *source = source.replacen(old, new, 1);
    }
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
