from pathlib import Path

service_path = Path("crates/medusa-daemon/src/telegram/service.rs")
service = service_path.read_text()
old_methods = '''    /// Processes one normalized Telegram message and persists transport state only after success.
    pub fn process_message(
        &mut self,
        update_id: i64,
        message: TelegramInboundMessage,
    ) -> Result<TelegramServiceOutcome, TelegramSessionServiceError> {
        self.process_message_with_source(update_id, message, false)
    }

    pub fn process_voice_message(
        &mut self,
        update_id: i64,
        message: TelegramInboundMessage,
    ) -> Result<TelegramServiceOutcome, TelegramSessionServiceError> {
        self.process_message_with_source(update_id, message, true)
    }

    fn process_message_with_source(
        &mut self,
        update_id: i64,
        message: TelegramInboundMessage,
        voice_source: bool,
    ) -> Result<TelegramServiceOutcome, TelegramSessionServiceError> {
'''
new_methods = '''    /// Processes one normalized Telegram message and persists transport state only after success.
    pub fn process_message(
        &mut self,
        update_id: i64,
        message: TelegramInboundMessage,
    ) -> Result<TelegramServiceOutcome, TelegramSessionServiceError> {
        self.process_message_with_source(update_id, message, false, false)
    }

    pub fn process_voice_message(
        &mut self,
        update_id: i64,
        message: TelegramInboundMessage,
    ) -> Result<TelegramServiceOutcome, TelegramSessionServiceError> {
        self.process_message_with_source(update_id, message, true, false)
    }

    pub(crate) fn process_acknowledged_message(
        &mut self,
        update_id: i64,
        message: TelegramInboundMessage,
        voice_source: bool,
    ) -> Result<TelegramServiceOutcome, TelegramSessionServiceError> {
        self.process_message_with_source(update_id, message, voice_source, true)
    }

    fn process_message_with_source(
        &mut self,
        update_id: i64,
        message: TelegramInboundMessage,
        voice_source: bool,
        transport_already_acknowledged: bool,
    ) -> Result<TelegramServiceOutcome, TelegramSessionServiceError> {
'''
if service.count(old_methods) != 1:
    raise SystemExit("service method contract did not match exactly")
service = service.replace(old_methods, new_methods, 1)

old_stale = '''        if existing
            .as_ref()
            .and_then(|binding| binding.last_update_id)
            .is_some_and(|last| update_id < last)
        {
            return Err(TelegramSessionServiceError::StaleUpdate(update_id));
        }
        let mut action = self.gateway.map_message(&message)?;
'''
new_stale = '''        if !transport_already_acknowledged
            && existing
                .as_ref()
                .and_then(|binding| binding.last_update_id)
                .is_some_and(|last| update_id < last)
        {
            return Err(TelegramSessionServiceError::StaleUpdate(update_id));
        }
        let binding_update_id = if transport_already_acknowledged {
            existing
                .as_ref()
                .and_then(|binding| binding.last_update_id)
                .map_or(update_id, |last| last.max(update_id))
        } else {
            update_id
        };
        let mut action = self.gateway.map_message(&message)?;
'''
if service.count(old_stale) != 1:
    raise SystemExit("service stale-update block did not match exactly")
service = service.replace(old_stale, new_stale, 1)

start = service.index("    fn process_message_with_source(")
end = service.index("    /// Persists an event delivery cursor", start)
function = service[start:end]
if function.count("                    update_id,\n                    &command,") != 1:
    raise SystemExit("binding acknowledgement update id did not match exactly")
function = function.replace(
    "                    update_id,\n                    &command,",
    "                    binding_update_id,\n                    &command,",
    1,
)
count = function.count("ensure_binding(key, source_chat_kind, existing, update_id)")
if count != 2:
    raise SystemExit(f"expected two ensure_binding update ids, found {count}")
function = function.replace(
    "ensure_binding(key, source_chat_kind, existing, update_id)",
    "ensure_binding(key, source_chat_kind, existing, binding_update_id)",
)
old_persist = '''        let persisted = self
            .acknowledge_update(update_id)
            .and_then(|()| self.persist());
'''
new_persist = '''        let persisted = if transport_already_acknowledged {
            self.persist()
        } else {
            self.acknowledge_update(update_id)
                .and_then(|()| self.persist())
        };
'''
if function.count(old_persist) != 1:
    raise SystemExit("service persistence block did not match exactly")
function = function.replace(old_persist, new_persist, 1)
service = service[:start] + function + service[end:]

test_marker = '''    #[test]
    fn duplicate_update_is_idempotent_and_detach_clears_binding() {
'''
test = '''    #[test]
    fn acknowledged_buffered_message_preserves_newer_transport_and_binding_cursors() {
        let repository = tempfile::tempdir().expect("repository");
        let state_path = repository.path().join(".medusa/telegram/state.json");
        let control =
            FrontendControlPlane::new(repository.path().to_path_buf(), Config::default());
        let mut service =
            TelegramSessionService::load(&state_path, gateway(), control).expect("service");

        service
            .process_message(12, message(12, "/verbose all"))
            .expect("newer update");
        assert!(matches!(
            service.process_message(11, message(11, "/voice tts")),
            Err(TelegramSessionServiceError::StaleUpdate(11))
        ));
        service
            .process_acknowledged_message(11, message(11, "/voice tts"), false)
            .expect("buffered update");

        let binding = service.binding(&identity()).expect("binding");
        assert_eq!(binding.last_update_id, Some(12));
        assert_eq!(binding.tool_progress, ToolProgressMode::All);
        assert_eq!(binding.voice_mode, TelegramVoiceMode::All);
        assert_eq!(service.next_update_offset(), Some(13));
        drop(service);

        let control =
            FrontendControlPlane::new(repository.path().to_path_buf(), Config::default());
        let reloaded =
            TelegramSessionService::load(&state_path, gateway(), control).expect("reload");
        let binding = reloaded.binding(&identity()).expect("reloaded binding");
        assert_eq!(binding.last_update_id, Some(12));
        assert_eq!(binding.tool_progress, ToolProgressMode::All);
        assert_eq!(binding.voice_mode, TelegramVoiceMode::All);
        assert_eq!(reloaded.next_update_offset(), Some(13));
    }

'''
if service.count(test_marker) != 1:
    raise SystemExit("service test insertion marker did not match exactly")
service = service.replace(test_marker, test + test_marker, 1)
service_path.write_text(service)

runtime_path = Path("crates/medusa-daemon/src/telegram/runtime.rs")
runtime = runtime_path.read_text()
old_media = "match self.process_messages(group.highest_update_id, &group.messages, outcomes) {"
new_media = "match self.process_acknowledged_messages(group.highest_update_id, &group.messages, outcomes) {"
if runtime.count(old_media) != 1:
    raise SystemExit("media-group flush call did not match exactly")
runtime = runtime.replace(old_media, new_media, 1)
old_text = "match self.process_messages(group.highest_update_id, &[message], outcomes) {"
new_text = "match self.process_acknowledged_messages(group.highest_update_id, &[message], outcomes) {"
if runtime.count(old_text) != 1:
    raise SystemExit("text-fragment flush call did not match exactly")
runtime = runtime.replace(old_text, new_text, 1)

old_process = '''    fn process_messages(
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
'''
new_process = '''    fn process_messages(
        &mut self,
        update_id: i64,
        messages: &[TelegramBotMessage],
        outcomes: &mut Vec<TelegramServiceOutcome>,
    ) -> Result<(), TelegramRuntimeError> {
        self.process_messages_with_transport_state(update_id, messages, outcomes, false)
    }

    fn process_acknowledged_messages(
        &mut self,
        update_id: i64,
        messages: &[TelegramBotMessage],
        outcomes: &mut Vec<TelegramServiceOutcome>,
    ) -> Result<(), TelegramRuntimeError> {
        self.process_messages_with_transport_state(update_id, messages, outcomes, true)
    }

    fn process_messages_with_transport_state(
        &mut self,
        update_id: i64,
        messages: &[TelegramBotMessage],
        outcomes: &mut Vec<TelegramServiceOutcome>,
        transport_already_acknowledged: bool,
    ) -> Result<(), TelegramRuntimeError> {
        let (inbound, voice_source) = self.inbound_batch(messages)?;
        let result = if transport_already_acknowledged {
            self.service
                .process_acknowledged_message(update_id, inbound, voice_source)
        } else if voice_source {
            self.service.process_voice_message(update_id, inbound)
        } else {
            self.service.process_message(update_id, inbound)
        };
        match result {
            Ok(outcome) => outcomes.push(outcome),
            Err(error) if is_rejected_input(&error) => {
                if !transport_already_acknowledged {
                    self.service.acknowledge_transport_update(update_id)?;
                }
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
'''
if runtime.count(old_process) != 1:
    raise SystemExit("runtime process_messages block did not match exactly")
runtime = runtime.replace(old_process, new_process, 1)
runtime_path.write_text(runtime)
