use std::{fs, path::Path};

pub fn run() {
    patch_bridge();
    patch_module_wiring();
    patch_service();
    patch_runtime();
}

fn patch_bridge() {
    let path = Path::new("src/telegram/mini_app.rs");
    let mut source = read(path);
    replace_if_present(
        &mut source,
        "pub struct TelegramMiniAppBridge {",
        "#[derive(Clone)]\npub struct TelegramMiniAppBridge {",
    );
    let marker = "#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]\n#[serde(deny_unknown_fields)]\npub struct TelegramMiniAppLaunchTicket";
    if !source.contains("pub struct TelegramMiniAppBinding") {
        let binding = "#[derive(Clone, Debug, Eq, PartialEq)]\npub struct TelegramMiniAppBinding {\n    pub identity: TelegramIdentity,\n    pub session_id: String,\n    pub expires_at: i64,\n}\n\n";
        replace_required(&mut source, marker, &format!("{binding}{marker}"));
    }
    let verify_marker = "    pub fn verify_launch_ticket(\n";
    if !source.contains("pub fn inspect_launch_ticket") {
        let method = r#"    pub fn inspect_launch_ticket(
        &self,
        token: &str,
        now: OffsetDateTime,
    ) -> Result<TelegramMiniAppBinding, TelegramMiniAppError> {
        let (payload_hex, signature_hex) = token
            .split_once('.')
            .ok_or(TelegramMiniAppError::InvalidTicket)?;
        if payload_hex.len() > MAX_INIT_DATA_BYTES * 2
            || signature_hex.len() != 64
            || !payload_hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !signature_hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(TelegramMiniAppError::InvalidTicket);
        }
        let payload = hex::decode(payload_hex).map_err(|_| TelegramMiniAppError::InvalidTicket)?;
        let supplied = hex::decode(signature_hex).map_err(|_| TelegramMiniAppError::InvalidTicket)?;
        let expected_signature = hmac_sha256(&self.secret.0, &payload);
        if !constant_time_eq(&expected_signature, &supplied) {
            return Err(TelegramMiniAppError::InvalidSignature);
        }
        let claims: LaunchClaims =
            serde_json::from_slice(&payload).map_err(|_| TelegramMiniAppError::InvalidTicket)?;
        if claims.version != 1 || claims.expires_at < now.unix_timestamp() {
            return Err(TelegramMiniAppError::InvalidTicket);
        }
        validate_session_id(&claims.session_id)?;
        Ok(TelegramMiniAppBinding {
            identity: TelegramIdentity {
                chat_id: claims.chat_id,
                topic_id: claims.topic_id,
                user_id: claims.user_id,
                chat_kind: TelegramChatKind::Private,
                bot_mentioned: false,
            },
            session_id: claims.session_id,
            expires_at: claims.expires_at,
        })
    }

"#;
        replace_required(&mut source, verify_marker, &format!("{method}{verify_marker}"));
    }
    replace_if_present(
        &mut source,
        "use super::TelegramIdentity;",
        "use super::{TelegramChatKind, TelegramIdentity};",
    );
    write(path, source);
}

fn patch_module_wiring() {
    let path = Path::new("src/telegram/mod.rs");
    let mut source = read(path);
    replace_if_present(
        &mut source,
        "mod mini_app;\n",
        "mod mini_app;\nmod mini_app_http;\n",
    );
    replace_if_present(
        &mut source,
        "    TelegramMiniAppBridge, TelegramMiniAppError, TelegramMiniAppLaunchTicket,\n",
        "    TelegramMiniAppBinding, TelegramMiniAppBridge, TelegramMiniAppError,\n    TelegramMiniAppLaunchTicket,\n",
    );
    if !source.contains("TelegramMiniAppHttpConfig") {
        replace_if_present(
            &mut source,
            "pub use projection::project_event;\n",
            "pub use mini_app_http::{\n    TelegramMiniAppCommand, TelegramMiniAppHttpConfig, TelegramMiniAppHttpError,\n    TelegramMiniAppHttpServer,\n};\npub use projection::project_event;\n",
        );
    }
    write(path, source);
}

fn patch_service() {
    let path = Path::new("src/telegram/service.rs");
    let mut source = read(path);
    replace_if_present(
        &mut source,
        "    TelegramAction, TelegramIdentity, TelegramInboundAction, TelegramInboundMessage,\n",
        "    TelegramAction, TelegramIdentity, TelegramInboundAction, TelegramInboundMessage,\n    TelegramMiniAppBridge, TelegramMiniAppCommand,\n",
    );
    replace_if_present(
        &mut source,
        "    attached_clients: BTreeSet<String>,\n}",
        "    attached_clients: BTreeSet<String>,\n    mini_app_bridge: Option<TelegramMiniAppBridge>,\n}",
    );
    replace_if_present(
        &mut source,
        "            attached_clients: BTreeSet::new(),\n        })",
        "            attached_clients: BTreeSet::new(),\n            mini_app_bridge: None,\n        })",
    );
    let offset_marker = "    #[must_use]\n    pub const fn next_update_offset(&self) -> Option<i64> {\n";
    if !source.contains("with_mini_app_bridge") {
        let method = "    #[must_use]\n    pub fn with_mini_app_bridge(mut self, bridge: TelegramMiniAppBridge) -> Self {\n        self.mini_app_bridge = Some(bridge);\n        self\n    }\n\n";
        replace_required(&mut source, offset_marker, &format!("{method}{offset_marker}"));
    }
    let deliver_marker = "    pub fn deliver_pending(\n";
    if !source.contains("process_mini_app_command") {
        let method = r#"    pub fn process_mini_app_command(
        &mut self,
        command: TelegramMiniAppCommand,
    ) -> Result<TelegramServiceOutcome, TelegramSessionServiceError> {
        self.gateway.authorize(&command.identity)?;
        let stable_id = TelegramBindingKey::from_identity(&command.identity).stable_id();
        let binding = self
            .state
            .bindings
            .get(&stable_id)
            .ok_or(TelegramSessionServiceError::BindingNotFound)?;
        if binding.session_id.as_deref() != Some(command.session_id.as_str()) {
            return Err(TelegramSessionServiceError::SessionBindingConflict);
        }
        let envelope = FrontendCommandEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            command_id: command.command_id.clone(),
            idempotency_key: format!("telegram-mini-app:{}", command.command_id),
            frontend: FrontendKind::Telegram,
            client_id: binding.client_id.clone(),
            session_id: Some(command.session_id),
            turn_id: None,
            timestamp: command.received_at,
            command: FrontendCommand::Submit {
                text: command.transcript,
                attachment_ids: Vec::new(),
            },
        };
        envelope
            .validate()
            .map_err(|error| TelegramSessionServiceError::InvalidCommand(error.to_owned()))?;
        let acknowledgement = self.control.dispatch(envelope)?;
        Ok(TelegramServiceOutcome::Forwarded {
            acknowledgement: Box::new(acknowledgement),
        })
    }

"#;
        replace_required(&mut source, deliver_marker, &format!("{method}{deliver_marker}"));
    }
    let old_url = "                let mini_app_url = self\n                    .gateway\n                    .config()\n                    .voice\n                    .mini_app_enabled\n                    .then(|| self.gateway.config().voice.mini_app_public_url.clone())\n                    .flatten();";
    let new_url = r#"                let mini_app_url = if self.gateway.config().voice.mini_app_enabled {
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
                };"#;
    replace_if_present(&mut source, old_url, new_url);
    write(path, source);
}

fn patch_runtime() {
    let path = Path::new("src/telegram/runtime.rs");
    let mut source = read(path);
    replace_if_present(
        &mut source,
        "    sync::atomic::{AtomicBool, Ordering},\n",
        "    sync::{\n        atomic::{AtomicBool, Ordering},\n        mpsc::{Receiver, TryRecvError},\n    },\n",
    );
    replace_if_present(
        &mut source,
        "    TelegramChatKind, TelegramGatewayError, TelegramIdentity, TelegramInboundMessage,\n",
        "    TelegramChatKind, TelegramGatewayError, TelegramIdentity, TelegramInboundMessage,\n    TelegramMiniAppCommand,\n",
    );
    replace_if_present(
        &mut source,
        "    voice_pipeline: Option<TelegramVoicePipeline>,\n}",
        "    voice_pipeline: Option<TelegramVoicePipeline>,\n    mini_app_commands: Option<Receiver<TelegramMiniAppCommand>>,\n}",
    );
    replace_if_present(
        &mut source,
        "            voice_pipeline: None,\n        })",
        "            voice_pipeline: None,\n            mini_app_commands: None,\n        })",
    );
    let poll_marker = "    pub fn poll_once(&mut self) -> Result<Vec<TelegramServiceOutcome>, TelegramRuntimeError> {\n";
    if !source.contains("with_mini_app_commands") {
        let method = "    #[must_use]\n    pub fn with_mini_app_commands(\n        mut self,\n        commands: Receiver<TelegramMiniAppCommand>,\n    ) -> Self {\n        self.mini_app_commands = Some(commands);\n        self\n    }\n\n";
        replace_required(&mut source, poll_marker, &format!("{method}{poll_marker}"));
    }
    replace_if_present(
        &mut source,
        "        let mut outcomes = Vec::new();\n        self.flush_due_media_groups",
        "        let mut outcomes = Vec::new();\n        self.drain_mini_app_commands(&mut outcomes)?;\n        self.flush_due_media_groups",
    );
    let queue_marker = "    fn queue_media_group(\n";
    if !source.contains("fn drain_mini_app_commands") {
        let method = r#"    fn drain_mini_app_commands(
        &mut self,
        outcomes: &mut Vec<TelegramServiceOutcome>,
    ) -> Result<(), TelegramRuntimeError> {
        loop {
            let result = match self.mini_app_commands.as_ref() {
                Some(receiver) => receiver.try_recv(),
                None => return Ok(()),
            };
            match result {
                Ok(command) => outcomes.push(self.service.process_mini_app_command(command)?),
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => {
                    self.mini_app_commands = None;
                    return Ok(());
                }
            }
        }
    }

"#;
        replace_required(&mut source, queue_marker, &format!("{method}{queue_marker}"));
    }
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
