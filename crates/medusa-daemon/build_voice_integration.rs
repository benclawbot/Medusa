use std::{fs, path::Path};

pub fn run() {
    patch_runtime();
    patch_service();
}

fn patch_runtime() {
    let path = Path::new("src/telegram/runtime.rs");
    let mut source = read(path);
    replace_if_present(
        &mut source,
        "    TelegramServiceOutcome, TelegramSessionService, TelegramSessionServiceError,\n",
        "    TelegramServiceOutcome, TelegramSessionService, TelegramSessionServiceError,\n    TelegramVoiceError, TelegramVoiceInput, TelegramVoicePipeline,\n",
    );
    replace_if_present(
        &mut source,
        "const MAX_TEXT_DOWNLOAD_BYTES: u64 = 4 * 1024 * 1024;\n",
        "const MAX_TEXT_DOWNLOAD_BYTES: u64 = 4 * 1024 * 1024;\nconst MAX_AUDIO_DOWNLOAD_BYTES: u64 = 20 * 1024 * 1024;\n",
    );
    replace_if_present(
        &mut source,
        "    pending_text_fragments: PendingTextFragmentState,\n}",
        "    pending_text_fragments: PendingTextFragmentState,\n    voice_pipeline: Option<TelegramVoicePipeline>,\n}",
    );
    replace_if_present(
        &mut source,
        "            pending_text_fragments,\n        })",
        "            pending_text_fragments,\n            voice_pipeline: None,\n        })",
    );
    let marker = "    pub fn poll_once(&mut self) -> Result<Vec<TelegramServiceOutcome>, TelegramRuntimeError> {\n";
    if !source.contains("pub fn with_voice_pipeline") {
        let method = "    #[must_use]\n    pub fn with_voice_pipeline(mut self, pipeline: TelegramVoicePipeline) -> Self {\n        self.voice_pipeline = Some(pipeline);\n        self\n    }\n\n";
        replace_required(&mut source, marker, &format!("{method}{marker}"));
    }
    replace_if_present(
        &mut source,
        "        self.service\n            .deliver_pending(&self.client, OffsetDateTime::now_utc())?;",
        "        self.service.deliver_pending_with_voice(\n            &self.client,\n            self.voice_pipeline.as_ref(),\n            OffsetDateTime::now_utc(),\n        )?;",
    );
    let start = "    fn process_messages(\n";
    let end = "    fn stage_file(\n";
    if !source.contains("fn transcribe_document") {
        let start_index = source.find(start).unwrap_or_else(|| fail("runtime process_messages marker"));
        let end_index = source[start_index..]
            .find(end)
            .map(|index| start_index + index)
            .unwrap_or_else(|| fail("runtime stage_file marker"));
        let replacement = r#"    fn process_messages(
        &mut self,
        update_id: i64,
        messages: &[TelegramBotMessage],
        outcomes: &mut Vec<TelegramServiceOutcome>,
    ) -> Result<(), TelegramRuntimeError> {
        let (inbound, voice_source) = self.inbound_batch(messages)?;
        let result = if voice_source {
            self.service.process_voice_message(update_id, inbound)
        } else {
            self.service.process_message(update_id, inbound)
        };
        match result {
            Ok(outcome) => outcomes.push(outcome),
            Err(error) if is_rejected_input(&error) => {
                self.service.acknowledge_transport_update(update_id)?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    fn inbound_batch(
        &self,
        messages: &[TelegramBotMessage],
    ) -> Result<(TelegramInboundMessage, bool), TelegramRuntimeError> {
        let mut normalized_messages = messages.to_vec();
        let mut attachment_ids = Vec::new();
        let mut voice_source = false;
        for message in &mut normalized_messages {
            if let Some(photo) = largest_photo(&message.photo) {
                attachment_ids.push(self.stage_file(
                    &photo.file_id,
                    photo.file_size,
                    format!(
                        "telegram-photo-{}.jpg",
                        safe_identifier(&photo.file_unique_id)
                    ),
                    Some("image/jpeg".to_owned()),
                    MAX_IMAGE_DOWNLOAD_BYTES,
                )?);
            }
            if let Some(document) = message.document.clone() {
                if document
                    .mime_type
                    .as_deref()
                    .is_some_and(|mime| mime.starts_with("audio/"))
                {
                    let transcript = self.transcribe_document(&document)?;
                    let prefix = message
                        .text
                        .take()
                        .or_else(|| message.caption.take())
                        .map(|text| text.trim().to_owned())
                        .filter(|text| !text.is_empty());
                    message.text = Some(prefix.map_or(transcript.clone(), |prefix| {
                        format!("{prefix}\n\n{transcript}")
                    }));
                    message.document = None;
                    voice_source = true;
                } else {
                    let (display_name, mime_type, limit) = document_metadata(&document)?;
                    attachment_ids.push(self.stage_file(
                        &document.file_id,
                        document.file_size,
                        display_name,
                        mime_type,
                        limit,
                    )?);
                }
            }
        }
        Ok((
            normalize_message_batch(
                &normalized_messages,
                &self.config.bot_username,
                attachment_ids,
            )?,
            voice_source,
        ))
    }

    fn transcribe_document(
        &self,
        document: &TelegramDocument,
    ) -> Result<String, TelegramRuntimeError> {
        let pipeline = self
            .voice_pipeline
            .as_ref()
            .ok_or(TelegramRuntimeError::VoiceUnavailable)?;
        if document
            .file_size
            .is_some_and(|size| size > MAX_AUDIO_DOWNLOAD_BYTES)
        {
            return Err(TelegramRuntimeError::RejectedMedia(format!(
                "Telegram audio exceeds the {MAX_AUDIO_DOWNLOAD_BYTES}-byte limit"
            )));
        }
        let file = self.client.get_file(&document.file_id).map_err(media_api_error)?;
        if file
            .file_size
            .is_some_and(|size| size > MAX_AUDIO_DOWNLOAD_BYTES)
        {
            return Err(TelegramRuntimeError::RejectedMedia(format!(
                "Telegram audio exceeds the {MAX_AUDIO_DOWNLOAD_BYTES}-byte limit"
            )));
        }
        let path = file.file_path.ok_or_else(|| {
            TelegramRuntimeError::RejectedMedia(
                "Telegram did not provide a downloadable audio path".to_owned(),
            )
        })?;
        let bytes = self
            .client
            .download_file(&path, MAX_AUDIO_DOWNLOAD_BYTES)
            .map_err(media_api_error)?;
        pipeline
            .transcribe(&TelegramVoiceInput {
                file_name: document
                    .file_name
                    .clone()
                    .unwrap_or_else(|| "voice.ogg".to_owned()),
                mime_type: document
                    .mime_type
                    .clone()
                    .unwrap_or_else(|| "audio/ogg".to_owned()),
                bytes,
            })
            .map_err(voice_error)
    }

"#;
        source.replace_range(start_index..end_index, replacement);
    }
    let helper_marker = "fn is_poison_transport_update(error: &TelegramBotApiError) -> bool {\n";
    if !source.contains("fn voice_error") {
        let helper = r#"fn voice_error(error: TelegramVoiceError) -> TelegramRuntimeError {
    match error {
        TelegramVoiceError::InvalidInputAudio
        | TelegramVoiceError::InvalidTranscript
        | TelegramVoiceError::InvalidSpeechInput
        | TelegramVoiceError::InvalidOggOpus
        | TelegramVoiceError::Rejected
        | TelegramVoiceError::MalformedResponse
        | TelegramVoiceError::ResponseTooLarge => {
            TelegramRuntimeError::RejectedMedia(error.to_string())
        }
        error => TelegramRuntimeError::Voice(error),
    }
}

"#;
        replace_required(&mut source, helper_marker, &format!("{helper}{helper_marker}"));
    }
    replace_if_present(
        &mut source,
        "    #[error(\"Telegram media was rejected: {0}\")]\n    RejectedMedia(String),\n",
        "    #[error(\"Telegram media was rejected: {0}\")]\n    RejectedMedia(String),\n    #[error(\"Telegram voice pipeline is not configured\")]\n    VoiceUnavailable,\n    #[error(transparent)]\n    Voice(#[from] TelegramVoiceError),\n",
    );
    replace_if_present(
        &mut source,
        "        matches!(self, Self::BotApi(error) if error.is_transient())\n",
        "        matches!(self, Self::BotApi(error) if error.is_transient())\n            || matches!(\n                self,\n                Self::Voice(\n                    TelegramVoiceError::Transport\n                        | TelegramVoiceError::RateLimited\n                        | TelegramVoiceError::ProviderUnavailable\n                )\n            )\n",
    );
    write(path, source);
}

fn patch_service() {
    let path = Path::new("src/telegram/service.rs");
    let mut source = read(path);
    replace_if_present(
        &mut source,
        "    TelegramIdentity, TelegramInboundAction, TelegramInboundMessage, TelegramRenderer,\n    TelegramVoiceMode, ToolProgressMode,\n",
        "    TelegramAction, TelegramIdentity, TelegramInboundAction, TelegramInboundMessage,\n    TelegramMessageSlot, TelegramRenderer, TelegramVoiceMode, TelegramVoicePipeline,\n    ToolProgressMode,\n",
    );
    replace_if_present(
        &mut source,
        "        TelegramBotApiClient, TelegramUpdateCursor",
        "        TelegramBotApiClient, TelegramOutboundFile, TelegramUpdateCursor",
    );
    replace_if_present(
        &mut source,
        "const LEGACY_TELEGRAM_SERVICE_SCHEMA_VERSION: u32 = 1;\nconst TELEGRAM_SERVICE_SCHEMA_VERSION: u32 = 2;",
        "const LEGACY_TELEGRAM_SERVICE_SCHEMA_VERSION: u32 = 1;\nconst PREVIOUS_TELEGRAM_SERVICE_SCHEMA_VERSION: u32 = 2;\nconst TELEGRAM_SERVICE_SCHEMA_VERSION: u32 = 3;",
    );
    replace_if_present(
        &mut source,
        "    #[serde(default)]\n    callbacks: CallbackStore,\n}",
        "    #[serde(default)]\n    callbacks: CallbackStore,\n    #[serde(default)]\n    voice_reply_bindings: BTreeSet<String>,\n}",
    );
    replace_if_present(
        &mut source,
        "            callbacks: CallbackStore::default(),\n        }",
        "            callbacks: CallbackStore::default(),\n            voice_reply_bindings: BTreeSet::new(),\n        }",
    );
    let old_migrate = r#"            LEGACY_TELEGRAM_SERVICE_SCHEMA_VERSION => {
                for binding in self.bindings.values_mut() {
                    binding.delivered_cursor =
                        binding.delivered_cursor.max(binding.acknowledged_cursor);
                    if binding.chat_kind == TelegramChatKind::Private && binding.key.chat_id < 0 {
                        binding.chat_kind = TelegramChatKind::Supergroup;
                    }
                }
                self.schema_version = TELEGRAM_SERVICE_SCHEMA_VERSION;
                Ok(self)
            }
"#;
    let new_migrate = r#"            LEGACY_TELEGRAM_SERVICE_SCHEMA_VERSION => {
                for binding in self.bindings.values_mut() {
                    binding.delivered_cursor =
                        binding.delivered_cursor.max(binding.acknowledged_cursor);
                    if binding.chat_kind == TelegramChatKind::Private && binding.key.chat_id < 0 {
                        binding.chat_kind = TelegramChatKind::Supergroup;
                    }
                }
                self.schema_version = PREVIOUS_TELEGRAM_SERVICE_SCHEMA_VERSION;
                self.migrate()
            }
            PREVIOUS_TELEGRAM_SERVICE_SCHEMA_VERSION => {
                self.schema_version = TELEGRAM_SERVICE_SCHEMA_VERSION;
                Ok(self)
            }
"#;
    replace_if_present(&mut source, old_migrate, new_migrate);
    replace_if_present(
        &mut source,
        "        for (stable_id, binding) in &self.bindings {",
        "        if !self\n            .voice_reply_bindings\n            .iter()\n            .all(|stable_id| self.bindings.contains_key(stable_id))\n        {\n            return Err(TelegramSessionServiceError::InvalidBinding);\n        }\n        for (stable_id, binding) in &self.bindings {",
    );
    if !source.contains("pub fn process_voice_message") {
        let signature = "    pub fn process_message(\n        &mut self,\n        update_id: i64,\n        message: TelegramInboundMessage,\n    ) -> Result<TelegramServiceOutcome, TelegramSessionServiceError> {\n";
        let replacement = "    pub fn process_message(\n        &mut self,\n        update_id: i64,\n        message: TelegramInboundMessage,\n    ) -> Result<TelegramServiceOutcome, TelegramSessionServiceError> {\n        self.process_message_with_source(update_id, message, false)\n    }\n\n    pub fn process_voice_message(\n        &mut self,\n        update_id: i64,\n        message: TelegramInboundMessage,\n    ) -> Result<TelegramServiceOutcome, TelegramSessionServiceError> {\n        self.process_message_with_source(update_id, message, true)\n    }\n\n    fn process_message_with_source(\n        &mut self,\n        update_id: i64,\n        message: TelegramInboundMessage,\n        voice_source: bool,\n    ) -> Result<TelegramServiceOutcome, TelegramSessionServiceError> {\n";
        replace_required(&mut source, signature, replacement);
    }
    replace_if_present(
        &mut source,
        "        if let Some(binding) = self.state.bindings.get_mut(&stable_id) {",
        "        if voice_source && matches!(&outcome, TelegramServiceOutcome::Forwarded { .. }) {\n            self.state.voice_reply_bindings.insert(stable_id.clone());\n        }\n\n        if let Some(binding) = self.state.bindings.get_mut(&stable_id) {",
    );
    let deliver_signature = "    pub fn deliver_pending(\n        &mut self,\n        client: &TelegramBotApiClient,\n        now: time::OffsetDateTime,\n    ) -> Result<usize, TelegramSessionServiceError> {\n";
    if !source.contains("pub fn deliver_pending_with_voice") {
        let replacement = "    pub fn deliver_pending(\n        &mut self,\n        client: &TelegramBotApiClient,\n        now: time::OffsetDateTime,\n    ) -> Result<usize, TelegramSessionServiceError> {\n        self.deliver_pending_with_voice(client, None, now)\n    }\n\n    pub fn deliver_pending_with_voice(\n        &mut self,\n        client: &TelegramBotApiClient,\n        voice_pipeline: Option<&TelegramVoicePipeline>,\n        now: time::OffsetDateTime,\n    ) -> Result<usize, TelegramSessionServiceError> {\n";
        replace_required(&mut source, deliver_signature, replacement);
    }
    replace_if_present(
        &mut source,
        "                self.deliver_event(client, &stable_id, &event, now)?;",
        "                self.deliver_event(client, voice_pipeline, &stable_id, &event, now)?;",
    );
    replace_if_present(
        &mut source,
        "    fn deliver_event(\n        &mut self,\n        client: &TelegramBotApiClient,\n        stable_id: &str,",
        "    fn deliver_event(\n        &mut self,\n        client: &TelegramBotApiClient,\n        voice_pipeline: Option<&TelegramVoicePipeline>,\n        stable_id: &str,",
    );
    let action_call = "                execute_actions(\n                    client,\n                    &mut self.gateway,\n                    &self.control,\n                    &identity,\n                    &session_id,\n                    projected.turn_id.as_deref(),\n                    &mut binding.delivery,\n                    &actions,\n                    mini_app_url.as_deref(),\n                    now,\n                )?;\n                binding.presentation_cursor = next_presentation_cursor;";
    if source.contains(action_call) {
        let replacement = format!(
            "{}\n                self.deliver_voice_reply(\n                    client,\n                    voice_pipeline,\n                    stable_id,\n                    event,\n                    &identity,\n                    &mut binding,\n                    &actions,\n                )?;\n                binding.presentation_cursor = next_presentation_cursor;",
            action_call.trim_end_matches("\n                binding.presentation_cursor = next_presentation_cursor;")
        );
        replace_required(&mut source, action_call, &replacement);
    }
    let binding_marker = "    fn binding_after_acknowledgement(\n";
    if !source.contains("fn deliver_voice_reply") {
        let methods = r#"    #[allow(clippy::too_many_arguments)]
    fn deliver_voice_reply(
        &mut self,
        client: &TelegramBotApiClient,
        voice_pipeline: Option<&TelegramVoicePipeline>,
        stable_id: &str,
        event: &EventEnvelope,
        identity: &TelegramIdentity,
        binding: &mut TelegramSessionBinding,
        actions: &[TelegramAction],
    ) -> Result<(), TelegramSessionServiceError> {
        if !matches!(&event.payload, EventPayload::RuntimeTurnFinished) {
            return Ok(());
        }
        let requested = binding.voice_mode == TelegramVoiceMode::All
            || (binding.voice_mode == TelegramVoiceMode::VoiceOnly
                && self.state.voice_reply_bindings.contains(stable_id));
        if !requested {
            return Ok(());
        }
        let pipeline = voice_pipeline.ok_or(TelegramSessionServiceError::VoiceUnavailable)?;
        let text = final_voice_text(actions)
            .ok_or(TelegramSessionServiceError::VoiceReplyMissingText)?;
        let voice = pipeline.synthesize(&text)?;
        let slot = TelegramMessageSlot::Notice(format!("voice:{}", event.sequence));
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

"#;
        replace_required(&mut source, binding_marker, &format!("{methods}{binding_marker}"));
    }
    let helper_marker = "fn digest_prefix(value: &str) -> String {\n";
    if !source.contains("fn final_voice_text") {
        let helper = r#"fn final_voice_text(actions: &[TelegramAction]) -> Option<String> {
    actions.iter().rev().find_map(|action| {
        let TelegramAction::UpsertText {
            slot: TelegramMessageSlot::Preview(_),
            text,
            ..
        } = action
        else {
            return None;
        };
        let mut plain = String::with_capacity(text.len());
        let mut escaped = false;
        for character in text.chars() {
            if escaped {
                plain.push(character);
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else {
                plain.push(character);
            }
        }
        let plain = plain.trim().trim_end_matches('▉').trim();
        (!plain.is_empty()).then(|| plain.to_owned())
    })
}

"#;
        replace_required(&mut source, helper_marker, &format!("{helper}{helper_marker}"));
    }
    replace_if_present(
        &mut source,
        "    #[error(\"Telegram cursor acknowledgement returned an unexpected result\")]\n    InvalidCursorAcknowledgement,\n",
        "    #[error(\"Telegram cursor acknowledgement returned an unexpected result\")]\n    InvalidCursorAcknowledgement,\n    #[error(\"Telegram voice pipeline is not configured\")]\n    VoiceUnavailable,\n    #[error(\"Telegram final voice reply has no canonical assistant text\")]\n    VoiceReplyMissingText,\n    #[error(transparent)]\n    Voice(#[from] super::TelegramVoiceError),\n",
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
