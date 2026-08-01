use std::{fs, path::Path};

pub fn run() {
    materialize_module_wiring();
    materialize_mini_app_interfaces();
    materialize_artifact_export_allowance();
    materialize_text_fragment_runtime();
}

fn materialize_module_wiring() {
    println!("cargo:rerun-if-changed=src/telegram/mod.rs");
    println!("cargo:rerun-if-changed=src/telegram/bot_api/mod.rs");
    println!("cargo:rerun-if-changed=src/telegram/bot_api/operations.rs");

    let telegram_path = Path::new("src/telegram/mod.rs");
    let mut telegram = read_source(telegram_path, "Telegram module");
    if !telegram.contains("mod mini_app;") {
        replace_once(
            &mut telegram,
            "mod format;\n",
            "mod format;\nmod mini_app;\n",
        );
        replace_once(
            &mut telegram,
            "mod text_fragments;\n",
            "mod text_fragments;\nmod voice;\nmod webhook;\n",
        );
        replace_once(
            &mut telegram,
            "pub use projection::project_event;\n",
            "pub use mini_app::{\n    TelegramMiniAppBridge, TelegramMiniAppError, TelegramMiniAppLaunchTicket,\n    TelegramMiniAppRealtimeSession, TelegramMiniAppSecret, TelegramMiniAppUser,\n    VerifiedMiniAppIdentity,\n};\npub use projection::project_event;\n",
        );
        replace_once(
            &mut telegram,
            "pub use runtime::{TelegramPollingConfig, TelegramPollingRuntime, TelegramRuntimeError};\n",
            "pub use runtime::{TelegramPollingConfig, TelegramPollingRuntime, TelegramRuntimeError};\npub use voice::{\n    OpenAiAudioToken, TelegramSynthesizedVoice, TelegramVoiceError, TelegramVoiceInput,\n    TelegramVoicePipeline,\n};\npub use webhook::{TelegramWebhookConfig, TelegramWebhookError, TelegramWebhookServer};\n",
        );
        write_source(telegram_path, telegram, "Telegram module");
    }

    let bot_api_path = Path::new("src/telegram/bot_api/mod.rs");
    let mut bot_api = read_source(bot_api_path, "Telegram Bot API module");
    if !bot_api.contains("mod operations;") {
        replace_once(&mut bot_api, "mod types;\n", "mod operations;\nmod types;\n");
        replace_once(
            &mut bot_api,
            "pub use types::{\n",
            "pub use operations::{\n    TelegramBotCommand, TelegramOutboundFile, TelegramWebhookInfo,\n};\npub use types::{\n",
        );
        write_source(bot_api_path, bot_api, "Telegram Bot API module");
    }

    let operations_path = Path::new("src/telegram/bot_api/operations.rs");
    let mut operations = read_source(operations_path, "Telegram Bot API operations");
    if operations.contains("use std::fmt::Write as _;") {
        replace_once(&mut operations, "use std::fmt::Write as _;\n\n", "");
    }
    if operations.contains("        write!(&mut String::new(), \"\").ok();\n") {
        replace_once(
            &mut operations,
            "        write!(&mut String::new(), \"\").ok();\n",
            "",
        );
    }
    write_source(operations_path, operations, "Telegram Bot API operations");
}

fn materialize_mini_app_interfaces() {
    println!("cargo:rerun-if-changed=src/telegram/mini_app.rs");
    let path = Path::new("src/telegram/mini_app.rs");
    let mut source = read_source(path, "Telegram Mini App bridge");
    if source.contains("use medusa_config::MedusaConfig;") {
        replace_once(
            &mut source,
            "use medusa_config::MedusaConfig;\n",
            "use medusa_config::Config;\n",
        );
    }
    if source.contains("    FrontendActor, FrontendCommand, FrontendCommandEnvelope, FrontendRequestContext,\n") {
        replace_once(
            &mut source,
            "    FrontendActor, FrontendCommand, FrontendCommandEnvelope, FrontendRequestContext,\n",
            "    FRONTEND_PROTOCOL_VERSION, FrontendCommand, FrontendCommandEnvelope, FrontendKind,\n",
        );
    }
    replace_all(&mut source, "config: &MedusaConfig", "config: &Config");
    if source.contains("use super::{TelegramIdentity, TelegramSessionServiceError};") {
        replace_once(
            &mut source,
            "use super::{TelegramIdentity, TelegramSessionServiceError};\n",
            "use super::TelegramIdentity;\n",
        );
    }
    let old = r#"        let actor = FrontendActor {
            frontend: medusa_protocol::frontend::FrontendKind::Telegram,
            principal: identity.user_id.to_string(),
            display_name: None,
        };
        let command = FrontendCommandEnvelope::new(
            actor,
            FrontendRequestContext {
                session_id: Some(session_id),
                turn_id: None,
                idempotency_key: Some(format!("telegram-mini-app:{}", Ulid::new())),
            },
            FrontendCommand::Submit {
                prompt: transcript.to_owned(),
                attachment_ids: Vec::new(),
                queue_if_busy: true,
            },
            now,
        )
        .map_err(|error| TelegramMiniAppError::Protocol(error.to_string()))?;
        control_plane
            .dispatch(command, now)
"#;
    let new = r#"        let command = FrontendCommandEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            command_id: Ulid::new().to_string(),
            idempotency_key: format!("telegram-mini-app:{}", Ulid::new()),
            frontend: FrontendKind::Telegram,
            client_id: identity.user_id.to_string(),
            session_id: Some(session_id),
            turn_id: None,
            timestamp: now,
            command: FrontendCommand::Submit {
                text: transcript.to_owned(),
                attachment_ids: Vec::new(),
            },
        };
        command
            .validate()
            .map_err(|error| TelegramMiniAppError::Protocol(error.to_owned()))?;
        control_plane
            .dispatch(command)
"#;
    if source.contains(old) {
        replace_once(&mut source, old, new);
    }
    if source.contains(
        "    #[error(transparent)]\n    Telegram(#[from] TelegramSessionServiceError),\n",
    ) {
        replace_once(
            &mut source,
            "    #[error(transparent)]\n    Telegram(#[from] TelegramSessionServiceError),\n",
            "",
        );
    }
    write_source(path, source, "Telegram Mini App bridge");
}

fn materialize_artifact_export_allowance() {
    println!("cargo:rerun-if-changed=src/artifact_store.rs");
    let path = Path::new("src/artifact_store.rs");
    let mut source = read_source(path, "frontend artifact store");
    if source.contains("#[allow(dead_code)]\n    pub fn export") {
        return;
    }
    replace_once(
        &mut source,
        "    pub fn export(\n",
        "    #[allow(dead_code)]\n    pub fn export(\n",
    );
    write_source(path, source, "frontend artifact store");
}

fn materialize_text_fragment_runtime() {
    println!("cargo:rerun-if-changed=src/telegram/runtime.rs");
    let path = Path::new("src/telegram/runtime.rs");
    let mut source = read_source(path, "Telegram runtime");
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
    write_source(path, source, "Telegram runtime");
}

fn read_source(path: &Path, label: &str) -> String {
    match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => fail(&format!("cannot read {label} source: {error}")),
    }
}

fn write_source(path: &Path, source: String, label: &str) {
    if let Err(error) = fs::write(path, source) {
        fail(&format!("cannot write materialized {label} source: {error}"));
    }
}

fn replace_once(source: &mut String, old: &str, new: &str) {
    let count = source.matches(old).count();
    if count != 1 {
        fail(&format!(
            "expected one source match, found {count}: {old:?}"
        ));
    }
    *source = source.replacen(old, new, 1);
}

fn replace_all(source: &mut String, old: &str, new: &str) {
    if source.contains(old) {
        *source = source.replace(old, new);
    }
}

fn fail(message: &str) -> ! {
    eprintln!("cargo:warning={message}");
    std::process::exit(1)
}
