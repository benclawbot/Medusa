//! Durable Telegram chat/topic bindings and shared-control-plane routing.
//!
//! This service owns only Telegram transport state: update offsets, chat/topic bindings, delivery
//! cursors, and per-binding display/voice preferences. Session truth and command authorization stay
//! in the daemon frontend control plane and production runtime.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use medusa_protocol::frontend::{
    AttachmentMode, FRONTEND_PROTOCOL_VERSION, FrontendCommand, FrontendCommandEnvelope,
    FrontendEvent, FrontendEventEnvelope, FrontendKind,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::bot_api::TelegramOutboundFile;

use crate::{
    FrontendCommandAcknowledgement, FrontendControlError, FrontendControlPlane,
    FrontendControlResult, LiveSessionReplayView,
};

use super::{
    TelegramAction, TelegramChatKind, TelegramDeliveryState, TelegramGateway, TelegramGatewayError,
    TelegramIdentity, TelegramInboundAction, TelegramInboundMessage, TelegramMessageSlot,
    TelegramMiniAppBridge, TelegramMiniAppCommand, TelegramRenderer, TelegramVoiceMode,
    TelegramVoicePipeline, ToolProgressMode,
    bot_api::{TelegramBotApiClient, TelegramUpdateCursor},
    callback::CallbackStore,
    delivery::execute_actions,
};

const LEGACY_TELEGRAM_SERVICE_SCHEMA_VERSION: u32 = 1;
const PREVIOUS_TELEGRAM_SERVICE_SCHEMA_VERSION: u32 = 2;
const TELEGRAM_SERVICE_SCHEMA_VERSION: u32 = 3;
const MAX_BINDINGS: usize = 4_096;

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramBindingKey {
    pub chat_id: i64,
    pub topic_id: Option<i64>,
    pub user_id: i64,
}

impl TelegramBindingKey {
    #[must_use]
    pub const fn from_identity(identity: &TelegramIdentity) -> Self {
        Self {
            chat_id: identity.chat_id,
            topic_id: identity.topic_id,
            user_id: identity.user_id,
        }
    }

    fn stable_id(&self) -> String {
        format!(
            "{}:{}:{}",
            self.chat_id,
            self.topic_id.unwrap_or_default(),
            self.user_id
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramSessionBinding {
    pub key: TelegramBindingKey,
    pub client_id: String,
    #[serde(default)]
    pub chat_kind: TelegramChatKind,
    pub session_id: Option<String>,
    pub acknowledged_cursor: u64,
    #[serde(default)]
    pub delivered_cursor: u64,
    #[serde(default)]
    pub presentation_cursor: u64,
    pub tool_progress: ToolProgressMode,
    pub voice_mode: TelegramVoiceMode,
    pub last_update_id: Option<i64>,
    #[serde(default)]
    pub delivery: TelegramDeliveryState,
    #[serde(default)]
    pub renderer: Option<TelegramRenderer>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct TelegramServiceState {
    schema_version: u32,
    next_update_offset: Option<i64>,
    bindings: BTreeMap<String, TelegramSessionBinding>,
    #[serde(default)]
    callbacks: CallbackStore,
    #[serde(default)]
    voice_reply_bindings: BTreeSet<String>,
    #[serde(default)]
    queued_voice_commands: BTreeMap<String, BTreeSet<String>>,
}

impl Default for TelegramServiceState {
    fn default() -> Self {
        Self {
            schema_version: TELEGRAM_SERVICE_SCHEMA_VERSION,
            next_update_offset: None,
            bindings: BTreeMap::new(),
            callbacks: CallbackStore::default(),
            voice_reply_bindings: BTreeSet::new(),
            queued_voice_commands: BTreeMap::new(),
        }
    }
}

impl TelegramServiceState {
    fn migrate(mut self) -> Result<Self, TelegramSessionServiceError> {
        match self.schema_version {
            TELEGRAM_SERVICE_SCHEMA_VERSION => Ok(self),
            LEGACY_TELEGRAM_SERVICE_SCHEMA_VERSION => {
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
            version => Err(TelegramSessionServiceError::UnsupportedSchema(version)),
        }
    }

    fn record_voice_submission(
        &mut self,
        stable_id: &str,
        acknowledgement: &FrontendCommandAcknowledgement,
    ) {
        let FrontendControlResult::SubmissionAccepted { queued, .. } = &acknowledgement.result
        else {
            return;
        };
        if *queued {
            self.queued_voice_commands
                .entry(stable_id.to_owned())
                .or_default()
                .insert(acknowledgement.command_id.clone());
        } else {
            self.voice_reply_bindings.insert(stable_id.to_owned());
        }
    }

    fn activate_queued_voice_command(&mut self, stable_id: &str, command_id: &str) {
        let activated = self
            .queued_voice_commands
            .get_mut(stable_id)
            .is_some_and(|commands| commands.remove(command_id));
        if self
            .queued_voice_commands
            .get(stable_id)
            .is_some_and(BTreeSet::is_empty)
        {
            self.queued_voice_commands.remove(stable_id);
        }
        if activated {
            self.voice_reply_bindings.insert(stable_id.to_owned());
        }
    }

    fn clear_active_voice_reply(&mut self, stable_id: &str) {
        self.voice_reply_bindings.remove(stable_id);
    }

    fn clear_voice_tracking(&mut self, stable_id: &str) {
        self.voice_reply_bindings.remove(stable_id);
        self.queued_voice_commands.remove(stable_id);
    }

    fn validate(&self) -> Result<(), TelegramSessionServiceError> {
        if self.schema_version != TELEGRAM_SERVICE_SCHEMA_VERSION {
            return Err(TelegramSessionServiceError::UnsupportedSchema(
                self.schema_version,
            ));
        }
        if self.bindings.len() > MAX_BINDINGS {
            return Err(TelegramSessionServiceError::TooManyBindings);
        }
        if self.next_update_offset.is_some_and(|offset| offset < 0) {
            return Err(TelegramSessionServiceError::InvalidUpdateOffset);
        }
        if !self
            .voice_reply_bindings
            .iter()
            .all(|stable_id| self.bindings.contains_key(stable_id))
            || self
                .queued_voice_commands
                .iter()
                .any(|(stable_id, commands)| {
                    !self.bindings.contains_key(stable_id)
                        || commands.is_empty()
                        || commands
                            .iter()
                            .any(|command_id| command_id.trim().is_empty())
                })
        {
            return Err(TelegramSessionServiceError::InvalidBinding);
        }
        for (stable_id, binding) in &self.bindings {
            if stable_id != &binding.key.stable_id()
                || binding.client_id.trim().is_empty()
                || binding
                    .session_id
                    .as_deref()
                    .is_some_and(|session_id| session_id.trim().is_empty())
                || binding.acknowledged_cursor > binding.delivered_cursor
                || binding
                    .last_update_id
                    .is_some_and(|update_id| update_id < 0)
            {
                return Err(TelegramSessionServiceError::InvalidBinding);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TelegramServiceOutcome {
    Forwarded {
        acknowledgement: Box<FrontendCommandAcknowledgement>,
    },
    ToolProgressUpdated {
        mode: ToolProgressMode,
    },
    VoiceModeUpdated {
        mode: TelegramVoiceMode,
    },
    VoiceStatus {
        mode: TelegramVoiceMode,
    },
    StartLiveVoice,
    Help,
}

/// Stateful Telegram adapter over the shared daemon frontend control plane.
pub struct TelegramSessionService {
    path: PathBuf,
    gateway: TelegramGateway,
    control: FrontendControlPlane,
    state: TelegramServiceState,
    attached_clients: BTreeSet<String>,
    mini_app_bridge: Option<TelegramMiniAppBridge>,
}

impl TelegramSessionService {
    pub fn load(
        path: impl Into<PathBuf>,
        gateway: TelegramGateway,
        control: FrontendControlPlane,
    ) -> Result<Self, TelegramSessionServiceError> {
        let path = path.into();
        let state = if path.is_file() {
            let bytes = fs::read(&path)?;
            let state: TelegramServiceState = serde_json::from_slice(&bytes)?;
            let state = state.migrate()?;
            state.validate()?;
            state
        } else {
            TelegramServiceState::default()
        };
        let mut gateway = gateway;
        gateway.restore_callbacks(state.callbacks.clone());
        Ok(Self {
            path,
            gateway,
            control,
            state,
            attached_clients: BTreeSet::new(),
            mini_app_bridge: None,
        })
    }

    #[must_use]
    pub fn with_mini_app_bridge(mut self, bridge: TelegramMiniAppBridge) -> Self {
        self.mini_app_bridge = Some(bridge);
        self
    }

    #[must_use]
    pub const fn next_update_offset(&self) -> Option<i64> {
        self.state.next_update_offset
    }

    #[must_use]
    pub fn binding(&self, identity: &TelegramIdentity) -> Option<&TelegramSessionBinding> {
        self.state
            .bindings
            .get(&TelegramBindingKey::from_identity(identity).stable_id())
    }

    /// Stages one bounded Telegram attachment in the shared daemon artifact store.
    pub fn ingest_attachment(
        &self,
        display_name: String,
        mime_type: Option<String>,
        bytes: Vec<u8>,
    ) -> Result<String, TelegramSessionServiceError> {
        self.control
            .ingest_attachment(display_name, mime_type, bytes)
            .map_err(Into::into)
    }

    /// Processes one normalized Telegram message and persists transport state only after success.
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
        if update_id < 0 {
            return Err(TelegramSessionServiceError::InvalidUpdateOffset);
        }
        let previous_state = self.state.clone();
        let source_message_id = message.message_id;
        let source_chat_kind = message.identity.chat_kind;
        let key = TelegramBindingKey::from_identity(&message.identity);
        let stable_id = key.stable_id();
        let existing = self.state.bindings.get(&stable_id).cloned();
        if !transport_already_acknowledged
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
        if let TelegramInboundAction::Forward(envelope) = &mut action
            && envelope.session_id.is_none()
            && command_uses_current_binding(&envelope.command)
        {
            envelope.session_id = existing
                .as_ref()
                .and_then(|binding| binding.session_id.clone());
        }
        let outcome = match action {
            TelegramInboundAction::Forward(envelope) => {
                let command = envelope.command.clone();
                let acknowledgement = self.control.dispatch(*envelope)?;
                let binding = self.binding_after_acknowledgement(
                    key,
                    source_chat_kind,
                    existing,
                    binding_update_id,
                    &command,
                    &acknowledgement,
                )?;
                match binding {
                    Some(binding) => {
                        self.state.bindings.insert(stable_id.clone(), binding);
                    }
                    None => {
                        self.state.bindings.remove(&stable_id);
                    }
                }
                TelegramServiceOutcome::Forwarded {
                    acknowledgement: Box::new(acknowledgement),
                }
            }
            TelegramInboundAction::SetToolProgress(mode) => {
                let binding = ensure_binding(key, source_chat_kind, existing, binding_update_id);
                self.state.bindings.insert(
                    stable_id.clone(),
                    TelegramSessionBinding {
                        tool_progress: mode,
                        ..binding
                    },
                );
                TelegramServiceOutcome::ToolProgressUpdated { mode }
            }
            TelegramInboundAction::SetVoiceMode(mode) => {
                let binding = ensure_binding(key, source_chat_kind, existing, binding_update_id);
                self.state.bindings.insert(
                    stable_id.clone(),
                    TelegramSessionBinding {
                        voice_mode: mode,
                        ..binding
                    },
                );
                TelegramServiceOutcome::VoiceModeUpdated { mode }
            }
            TelegramInboundAction::VoiceStatus => TelegramServiceOutcome::VoiceStatus {
                mode: existing
                    .as_ref()
                    .map_or(self.gateway.config().voice.mode, |binding| {
                        binding.voice_mode
                    }),
            },
            TelegramInboundAction::StartLiveVoice => TelegramServiceOutcome::StartLiveVoice,
            TelegramInboundAction::Help => TelegramServiceOutcome::Help,
        };

        if voice_source && let TelegramServiceOutcome::Forwarded { acknowledgement } = &outcome {
            self.state
                .record_voice_submission(&stable_id, acknowledgement);
        }
        if matches!(
            &outcome,
            TelegramServiceOutcome::VoiceModeUpdated {
                mode: TelegramVoiceMode::Off
            }
        ) {
            self.state.clear_voice_tracking(&stable_id);
        }

        if let Some(binding) = self.state.bindings.get_mut(&stable_id) {
            binding.delivery.set_source_message(source_message_id);
            if binding.session_id.is_some() {
                self.attached_clients.insert(binding.client_id.clone());
            }
        } else {
            self.state.clear_voice_tracking(&stable_id);
            self.attached_clients
                .remove(&format!("telegram:{stable_id}"));
        }
        let persisted = if transport_already_acknowledged {
            self.persist()
        } else {
            self.acknowledge_update(update_id)
                .and_then(|()| self.persist())
        };
        if let Err(error) = persisted {
            self.state = previous_state;
            return Err(error);
        }
        Ok(outcome)
    }

    /// Persists an event delivery cursor after the shared control plane accepts its acknowledgement.
    pub fn acknowledge_delivery(
        &mut self,
        identity: &TelegramIdentity,
        cursor: u64,
        update_id: i64,
        received_at: time::OffsetDateTime,
    ) -> Result<FrontendCommandAcknowledgement, TelegramSessionServiceError> {
        let binding = self
            .binding(identity)
            .cloned()
            .ok_or(TelegramSessionServiceError::BindingNotFound)?;
        let session_id = binding
            .session_id
            .clone()
            .ok_or(TelegramSessionServiceError::SessionNotBound)?;
        let stable = format!("{}:cursor:{cursor}", binding.key.stable_id());
        let message = TelegramInboundMessage {
            identity: identity.clone(),
            message_id: update_id,
            text: "/status".to_owned(),
            attachment_ids: Vec::new(),
            attached_session_id: Some(session_id.clone()),
            received_at,
        };
        self.gateway.authorize(identity)?;
        let envelope = medusa_protocol::frontend::FrontendCommandEnvelope {
            protocol_version: medusa_protocol::frontend::FRONTEND_PROTOCOL_VERSION,
            command_id: format!("telegram-cursor-{}", digest_prefix(&stable)),
            idempotency_key: format!("telegram:{stable}"),
            frontend: medusa_protocol::frontend::FrontendKind::Telegram,
            client_id: binding.client_id.clone(),
            session_id: Some(session_id),
            turn_id: None,
            timestamp: message.received_at,
            command: FrontendCommand::AcknowledgeCursor { cursor },
        };
        let acknowledgement = self.control.dispatch(envelope)?;
        let FrontendControlResult::CursorAcknowledged { attachment } = &acknowledgement.result
        else {
            return Err(TelegramSessionServiceError::InvalidCursorAcknowledgement);
        };
        let previous_state = self.state.clone();
        let entry = self
            .state
            .bindings
            .get_mut(&binding.key.stable_id())
            .ok_or(TelegramSessionServiceError::BindingNotFound)?;
        entry.delivered_cursor = entry.delivered_cursor.max(attachment.acknowledged_cursor);
        entry.acknowledged_cursor = attachment.acknowledged_cursor;
        if let Err(error) = self.persist() {
            self.state = previous_state;
            return Err(error);
        }
        Ok(acknowledgement)
    }

    /// Resolves one signed callback through the same frontend control plane and durable binding.
    pub fn process_callback(
        &mut self,
        update_id: i64,
        identity: TelegramIdentity,
        callback_data: &str,
        received_at: time::OffsetDateTime,
    ) -> Result<FrontendCommandAcknowledgement, TelegramSessionServiceError> {
        if update_id < 0 {
            return Err(TelegramSessionServiceError::InvalidUpdateOffset);
        }
        let previous_gateway = self.gateway.clone();
        let previous_state = self.state.clone();
        let result = (|| {
            let key = TelegramBindingKey::from_identity(&identity);
            let stable_id = key.stable_id();
            let existing = self.state.bindings.get(&stable_id).cloned();
            let envelope = self
                .gateway
                .resolve_callback(&identity, callback_data, received_at)?;
            let command = envelope.command.clone();
            let acknowledgement = self.control.dispatch(envelope)?;
            if let Some(binding) = self.binding_after_acknowledgement(
                key,
                identity.chat_kind,
                existing,
                update_id,
                &command,
                &acknowledgement,
            )? {
                self.state.bindings.insert(stable_id.clone(), binding);
            }
            self.acknowledge_update(update_id)?;
            self.persist()?;
            Ok(acknowledgement)
        })();
        if result.is_err() {
            self.gateway = previous_gateway;
            self.state = previous_state;
        }
        result
    }

    /// Advances the durable Bot API cursor for an unsupported or rejected valid update.
    pub fn acknowledge_transport_update(
        &mut self,
        update_id: i64,
    ) -> Result<(), TelegramSessionServiceError> {
        let previous_state = self.state.clone();
        let result = self
            .acknowledge_update(update_id)
            .and_then(|()| self.persist());
        if result.is_err() {
            self.state = previous_state;
        }
        result
    }

    /// Replays and delivers every pending canonical event for all bound Telegram chats.
    ///
    /// Canonical cursors advance only after Bot API actions and durable delivery state succeed.
    pub fn process_mini_app_command(
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

    pub fn deliver_pending(
        &mut self,
        client: &TelegramBotApiClient,
        now: time::OffsetDateTime,
    ) -> Result<usize, TelegramSessionServiceError> {
        self.deliver_pending_with_voice(client, None, now)
    }

    pub fn deliver_pending_with_voice(
        &mut self,
        client: &TelegramBotApiClient,
        voice_pipeline: Option<&TelegramVoicePipeline>,
        now: time::OffsetDateTime,
    ) -> Result<usize, TelegramSessionServiceError> {
        let binding_ids = self.state.bindings.keys().cloned().collect::<Vec<_>>();
        let mut delivered = 0_usize;
        for stable_id in binding_ids {
            let Some(binding) = self.state.bindings.get(&stable_id).cloned() else {
                continue;
            };
            let Some(session_id) = binding.session_id.clone() else {
                continue;
            };
            let replay = self.replay_for_binding(&binding, &session_id, now)?;
            for event in &replay.events {
                if event.cursor <= binding.acknowledged_cursor {
                    continue;
                }
                self.deliver_event(client, voice_pipeline, &stable_id, event, now)?;
                delivered = delivered.saturating_add(1);
            }
            let acknowledged_cursor = self
                .state
                .bindings
                .get(&stable_id)
                .map_or(binding.acknowledged_cursor, |current| {
                    current.acknowledged_cursor
                });
            if replay.next_cursor > acknowledged_cursor {
                self.acknowledge_binding_cursor(&stable_id, &session_id, replay.next_cursor, now)?;
            }
        }
        Ok(delivered)
    }

    fn replay_for_binding(
        &mut self,
        binding: &TelegramSessionBinding,
        session_id: &str,
        now: time::OffsetDateTime,
    ) -> Result<LiveSessionReplayView, TelegramSessionServiceError> {
        if self.attached_clients.contains(&binding.client_id) {
            return self
                .control
                .replay_events(&binding.client_id, binding.acknowledged_cursor)
                .map_err(Into::into);
        }
        let stable = format!(
            "{}:attach:{}:{}",
            binding.key.stable_id(),
            session_id,
            binding.acknowledged_cursor
        );
        let acknowledgement = self.control.dispatch(FrontendCommandEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            command_id: format!("telegram-replay-{}", digest_prefix(&stable)),
            idempotency_key: format!("telegram-replay:{stable}"),
            frontend: FrontendKind::Telegram,
            client_id: binding.client_id.clone(),
            session_id: Some(session_id.to_owned()),
            turn_id: None,
            timestamp: now,
            command: FrontendCommand::Attach {
                session_id: session_id.to_owned(),
                mode: AttachmentMode::ReadOnly,
                after_cursor: Some(binding.acknowledged_cursor),
            },
        })?;
        let FrontendControlResult::Attached { attachment } = acknowledgement.result else {
            return Err(TelegramSessionServiceError::InvalidReplayAttachment);
        };
        self.attached_clients.insert(binding.client_id.clone());
        Ok(LiveSessionReplayView {
            session_id: attachment.session.id.clone(),
            client_id: attachment.client_id.clone(),
            frontend: attachment.frontend,
            after_cursor: binding.acknowledged_cursor,
            next_cursor: attachment.replay_cursor,
            events: attachment.replay,
        })
    }

    fn deliver_event(
        &mut self,
        client: &TelegramBotApiClient,
        voice_pipeline: Option<&TelegramVoicePipeline>,
        stable_id: &str,
        event: &FrontendEventEnvelope,
        now: time::OffsetDateTime,
    ) -> Result<(), TelegramSessionServiceError> {
        let original_state = self.state.clone();
        let original_gateway = self.gateway.clone();
        let mut binding = self
            .state
            .bindings
            .get(stable_id)
            .cloned()
            .ok_or(TelegramSessionServiceError::BindingNotFound)?;
        let session_id = binding
            .session_id
            .clone()
            .ok_or(TelegramSessionServiceError::SessionNotBound)?;
        let identity = TelegramIdentity {
            user_id: binding.key.user_id,
            chat_id: binding.key.chat_id,
            topic_id: binding.key.topic_id,
            chat_kind: binding.chat_kind,
            bot_mentioned: true,
        };

        if event.cursor > binding.delivered_cursor {
            match &event.event {
                FrontendEvent::Started => {
                    self.state
                        .activate_queued_voice_command(stable_id, &event.correlation_id);
                }
                FrontendEvent::Cancelled { .. } | FrontendEvent::Failed { .. } => {
                    self.state.clear_active_voice_reply(stable_id);
                }
                _ => {}
            }
            let mut display = self.gateway.config().display.clone();
            display.tool_progress = binding.tool_progress;
            let source_message_id = binding.delivery.source_message_id.unwrap_or_default();
            let mut renderer = binding
                .renderer
                .take()
                .map_or_else(|| TelegramRenderer::new(display, source_message_id), Ok)?;
            if matches!(&event.event, FrontendEvent::Started) {
                renderer.begin_turn(source_message_id);
            }
            let actions = renderer.render(event, now)?;
            let mini_app_url = if self.gateway.config().voice.mini_app_enabled {
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
            };
            execute_actions(
                client,
                &mut self.gateway,
                &self.control,
                &identity,
                &session_id,
                event.turn_id.as_deref(),
                &mut binding.delivery,
                &actions,
                mini_app_url.as_deref(),
                now,
            )?;
            self.deliver_voice_reply(
                client,
                voice_pipeline,
                stable_id,
                event,
                &identity,
                &mut binding,
                &actions,
            )?;
            binding.presentation_cursor = event.cursor;
            binding.renderer = Some(renderer);
            binding.delivered_cursor = event.cursor;
            self.state
                .bindings
                .insert(stable_id.to_owned(), binding.clone());
            if let Err(error) = self.persist() {
                self.state = original_state;
                self.gateway = original_gateway;
                return Err(error);
            }
        }

        if event.cursor > binding.acknowledged_cursor {
            self.acknowledge_binding_cursor(stable_id, &session_id, event.cursor, now)?;
        }
        Ok(())
    }

    fn acknowledge_binding_cursor(
        &mut self,
        stable_id: &str,
        session_id: &str,
        cursor: u64,
        now: time::OffsetDateTime,
    ) -> Result<(), TelegramSessionServiceError> {
        let binding = self
            .state
            .bindings
            .get(stable_id)
            .cloned()
            .ok_or(TelegramSessionServiceError::BindingNotFound)?;
        if cursor <= binding.acknowledged_cursor {
            return Ok(());
        }
        let acknowledgement = self.control.dispatch(FrontendCommandEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            command_id: format!(
                "telegram-cursor-{}",
                digest_prefix(&format!("{stable_id}:{cursor}"))
            ),
            idempotency_key: format!("telegram:{stable_id}:cursor:{cursor}"),
            frontend: FrontendKind::Telegram,
            client_id: binding.client_id,
            session_id: Some(session_id.to_owned()),
            turn_id: None,
            timestamp: now,
            command: FrontendCommand::AcknowledgeCursor { cursor },
        })?;
        let FrontendControlResult::CursorAcknowledged { attachment } = acknowledgement.result
        else {
            return Err(TelegramSessionServiceError::InvalidCursorAcknowledgement);
        };
        let entry = self
            .state
            .bindings
            .get_mut(stable_id)
            .ok_or(TelegramSessionServiceError::BindingNotFound)?;
        entry.acknowledged_cursor = attachment.acknowledged_cursor;
        entry.delivered_cursor = entry.delivered_cursor.max(cursor);
        self.persist()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn deliver_voice_reply(
        &mut self,
        client: &TelegramBotApiClient,
        voice_pipeline: Option<&TelegramVoicePipeline>,
        stable_id: &str,
        event: &FrontendEventEnvelope,
        identity: &TelegramIdentity,
        binding: &mut TelegramSessionBinding,
        actions: &[TelegramAction],
    ) -> Result<(), TelegramSessionServiceError> {
        if !matches!(&event.event, FrontendEvent::TurnFinished) {
            return Ok(());
        }
        let requested = binding.voice_mode == TelegramVoiceMode::All
            || (binding.voice_mode == TelegramVoiceMode::VoiceOnly
                && self.state.voice_reply_bindings.contains(stable_id));
        if !requested {
            return Ok(());
        }
        let pipeline = voice_pipeline.ok_or(TelegramSessionServiceError::VoiceUnavailable)?;
        let text =
            final_voice_text(actions).ok_or(TelegramSessionServiceError::VoiceReplyMissingText)?;
        let voice = pipeline.synthesize(&text)?;
        let slot = TelegramMessageSlot::Notice(format!("voice:{}", event.cursor));
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

    fn binding_after_acknowledgement(
        &self,
        key: TelegramBindingKey,
        chat_kind: TelegramChatKind,
        existing: Option<TelegramSessionBinding>,
        update_id: i64,
        command: &FrontendCommand,
        acknowledgement: &FrontendCommandAcknowledgement,
    ) -> Result<Option<TelegramSessionBinding>, TelegramSessionServiceError> {
        if matches!(command, FrontendCommand::Detach) {
            return Ok(None);
        }
        let mut binding = ensure_binding(key, chat_kind, existing, update_id);
        let result_session_id = match &acknowledgement.result {
            FrontendControlResult::Attached { attachment }
            | FrontendControlResult::RuntimeReady { attachment }
            | FrontendControlResult::CursorAcknowledged { attachment } => {
                if let FrontendControlResult::CursorAcknowledged { attachment } =
                    &acknowledgement.result
                {
                    binding.acknowledged_cursor = attachment.acknowledged_cursor;
                }
                Some(attachment.session.id.clone())
            }
            FrontendControlResult::SubmissionAccepted { session_id, .. }
            | FrontendControlResult::CancellationRequested { session_id, .. }
            | FrontendControlResult::CommandAccepted { session_id, .. }
            | FrontendControlResult::Status { session_id, .. } => Some(session_id.clone()),
            FrontendControlResult::Events { replay } => Some(replay.session_id.clone()),
            FrontendControlResult::Sessions { .. }
            | FrontendControlResult::Detached { .. }
            | FrontendControlResult::Transient { .. } => None,
        };
        if let Some(session_id) = result_session_id.or_else(|| acknowledgement.session_id.clone()) {
            if binding
                .session_id
                .as_deref()
                .is_some_and(|current| current != session_id)
                && !matches!(command, FrontendCommand::Attach { .. })
            {
                return Err(TelegramSessionServiceError::SessionBindingConflict);
            }
            binding.session_id = Some(session_id);
        }
        Ok(Some(binding))
    }

    fn acknowledge_update(&mut self, update_id: i64) -> Result<(), TelegramSessionServiceError> {
        let mut cursor = TelegramUpdateCursor::default();
        if let Some(offset) = self.state.next_update_offset {
            if offset > 0 {
                cursor.acknowledge(offset - 1)?;
            }
        }
        cursor.acknowledge(update_id)?;
        self.state.next_update_offset = cursor.next_offset();
        if let Some(binding) = self
            .state
            .bindings
            .values_mut()
            .find(|binding| binding.last_update_id == Some(update_id))
        {
            binding.last_update_id = Some(update_id);
        }
        Ok(())
    }

    fn persist(&mut self) -> Result<(), TelegramSessionServiceError> {
        self.state.callbacks = self.gateway.callback_snapshot();
        self.state.validate()?;
        let parent = self
            .path
            .parent()
            .ok_or(TelegramSessionServiceError::MissingParentDirectory)?;
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(
            ".telegram-state-{}.tmp",
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let bytes = serde_json::to_vec_pretty(&self.state)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        if let Err(error) = (|| -> std::io::Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, &self.path)?;
            sync_parent(parent)
        })() {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        Ok(())
    }
}

fn command_uses_current_binding(command: &FrontendCommand) -> bool {
    !matches!(
        command,
        FrontendCommand::CreateSession { .. }
            | FrontendCommand::ListSessions
            | FrontendCommand::ResumeSession { .. }
            | FrontendCommand::Attach { .. }
            | FrontendCommand::Detach
    )
}

fn ensure_binding(
    key: TelegramBindingKey,
    chat_kind: TelegramChatKind,
    existing: Option<TelegramSessionBinding>,
    update_id: i64,
) -> TelegramSessionBinding {
    existing.map_or_else(
        || TelegramSessionBinding {
            client_id: format!("telegram:{}", key.stable_id()),
            key,
            chat_kind,
            session_id: None,
            acknowledged_cursor: 0,
            delivered_cursor: 0,
            presentation_cursor: 0,
            tool_progress: ToolProgressMode::New,
            voice_mode: TelegramVoiceMode::Off,
            last_update_id: Some(update_id),
            delivery: TelegramDeliveryState::default(),
            renderer: None,
        },
        |mut binding| {
            binding.chat_kind = chat_kind;
            binding.last_update_id = Some(update_id);
            binding
        },
    )
}

fn final_voice_text(actions: &[TelegramAction]) -> Option<String> {
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

fn digest_prefix(value: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = hex::encode(Sha256::digest(value.as_bytes()));
    digest[..24].to_owned()
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_: &Path) -> std::io::Result<()> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum TelegramSessionServiceError {
    #[error("unsupported Telegram service state schema version {0}")]
    UnsupportedSchema(u32),
    #[error("Telegram service state contains too many bindings")]
    TooManyBindings,
    #[error("Telegram service state contains an invalid binding")]
    InvalidBinding,
    #[error("Telegram update offset is invalid")]
    InvalidUpdateOffset,
    #[error("Telegram update {0} is older than the last processed update")]
    StaleUpdate(i64),
    #[error("Telegram session binding was not found")]
    BindingNotFound,
    #[error("Telegram chat/topic is not bound to a session")]
    SessionNotBound,
    #[error("Telegram acknowledgement conflicts with the durable session binding")]
    SessionBindingConflict,
    #[error("Telegram service state path has no parent directory")]
    MissingParentDirectory,
    #[error("Telegram replay attach did not return an attachment")]
    InvalidReplayAttachment,
    #[error("Telegram cursor acknowledgement returned an unexpected result")]
    InvalidCursorAcknowledgement,
    #[error("Telegram voice pipeline is not configured")]
    VoiceUnavailable,
    #[error("Telegram final voice reply has no canonical assistant text")]
    VoiceReplyMissingText,
    #[error(transparent)]
    Voice(#[from] super::TelegramVoiceError),
    // issue-568-service-error-fixups
    #[error("Telegram frontend command is invalid: {0}")]
    InvalidCommand(String),
    #[error(transparent)]
    MiniApp(#[from] super::TelegramMiniAppError),
    #[error(transparent)]
    Gateway(#[from] TelegramGatewayError),
    #[error(transparent)]
    Control(#[from] FrontendControlError),
    #[error(transparent)]
    BotApi(#[from] super::bot_api::TelegramBotApiError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use medusa_agent::AgentEngine;
    use medusa_config::Config;
    use medusa_core::MedusaResult;
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};
    use time::macros::datetime;

    use super::*;
    use crate::telegram::{TelegramChatKind, TelegramConfig};

    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("session creation does not call the provider")
        }
    }

    fn gateway() -> TelegramGateway {
        TelegramGateway::new(TelegramConfig {
            enabled: true,
            allowed_users: BTreeSet::from([42]),
            ..TelegramConfig::default()
        })
        .expect("gateway")
    }

    fn identity() -> TelegramIdentity {
        TelegramIdentity {
            user_id: 42,
            chat_id: 42,
            topic_id: None,
            chat_kind: TelegramChatKind::Private,
            bot_mentioned: false,
        }
    }

    fn message(id: i64, text: &str) -> TelegramInboundMessage {
        TelegramInboundMessage {
            identity: identity(),
            message_id: id,
            text: text.to_owned(),
            attachment_ids: Vec::new(),
            attached_session_id: None,
            received_at: datetime!(2026-07-31 01:00 UTC),
        }
    }

    #[test]
    fn attach_binding_and_preferences_survive_service_restart() {
        let repository = tempfile::tempdir().expect("repository");
        let session = AgentEngine::new(UnusedProvider, Config::default())
            .create_session(repository.path(), "Telegram shared session".to_owned())
            .expect("session");
        let state_path = repository.path().join(".medusa/telegram/state.json");
        let control = FrontendControlPlane::new(repository.path().to_path_buf(), Config::default());
        let mut service =
            TelegramSessionService::load(&state_path, gateway(), control).expect("service");

        service
            .process_message(10, message(10, &format!("/attach {}", session.id)))
            .expect("attach");
        service
            .process_message(11, message(11, "/verbose all"))
            .expect("progress mode");
        service
            .process_message(12, message(12, "/voice tts"))
            .expect("voice mode");
        let binding = service.binding(&identity()).expect("binding");
        assert_eq!(binding.session_id.as_deref(), Some(session.id.as_str()));
        assert_eq!(binding.tool_progress, ToolProgressMode::All);
        assert_eq!(binding.voice_mode, TelegramVoiceMode::All);
        assert_eq!(service.next_update_offset(), Some(13));
        drop(service);

        let control = FrontendControlPlane::new(repository.path().to_path_buf(), Config::default());
        let reloaded =
            TelegramSessionService::load(&state_path, gateway(), control).expect("reload");
        let binding = reloaded.binding(&identity()).expect("binding");
        assert_eq!(binding.session_id.as_deref(), Some(session.id.as_str()));
        assert_eq!(binding.tool_progress, ToolProgressMode::All);
        assert_eq!(binding.voice_mode, TelegramVoiceMode::All);
        assert_eq!(reloaded.next_update_offset(), Some(13));
    }

    #[test]
    fn legacy_state_migrates_delivery_cursor_and_group_kind() {
        let repository = tempfile::tempdir().expect("repository");
        let state_path = repository.path().join(".medusa/telegram/state.json");
        std::fs::create_dir_all(state_path.parent().expect("state parent"))
            .expect("create state parent");
        let legacy = serde_json::json!({
            "schema_version": LEGACY_TELEGRAM_SERVICE_SCHEMA_VERSION,
            "next_update_offset": 8,
            "bindings": {
                "-100:0:42": {
                    "key": {
                        "chat_id": -100,
                        "topic_id": null,
                        "user_id": 42
                    },
                    "client_id": "telegram:-100:0:42",
                    "session_id": "session-1",
                    "acknowledged_cursor": 7,
                    "tool_progress": "new",
                    "voice_mode": "off",
                    "last_update_id": 7
                }
            }
        });
        std::fs::write(
            &state_path,
            serde_json::to_vec_pretty(&legacy).expect("serialize legacy state"),
        )
        .expect("write legacy state");

        let control = FrontendControlPlane::new(repository.path().to_path_buf(), Config::default());
        let mut service =
            TelegramSessionService::load(&state_path, gateway(), control).expect("migrate state");
        let group_identity = TelegramIdentity {
            user_id: 42,
            chat_id: -100,
            topic_id: None,
            chat_kind: TelegramChatKind::Supergroup,
            bot_mentioned: true,
        };
        let binding = service.binding(&group_identity).expect("migrated binding");
        assert_eq!(binding.delivered_cursor, 7);
        assert_eq!(binding.chat_kind, TelegramChatKind::Supergroup);

        service
            .acknowledge_transport_update(8)
            .expect("persist migrated state");
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state_path).expect("read migrated state"))
                .expect("parse migrated state");
        assert_eq!(
            persisted["schema_version"],
            serde_json::json!(TELEGRAM_SERVICE_SCHEMA_VERSION)
        );
        assert_eq!(
            persisted["bindings"]["-100:0:42"]["delivered_cursor"],
            serde_json::json!(7)
        );
    }

    #[test]
    fn queued_voice_submission_activates_only_when_dequeued() {
        let key = TelegramBindingKey {
            chat_id: 42,
            topic_id: None,
            user_id: 42,
        };
        let stable_id = key.stable_id();
        let mut state = TelegramServiceState::default();
        state.bindings.insert(
            stable_id.clone(),
            ensure_binding(key, TelegramChatKind::Private, None, 1),
        );
        let queued = FrontendCommandAcknowledgement {
            command_id: "voice-queued".to_owned(),
            idempotency_key: "voice-queued-key".to_owned(),
            session_id: Some("session-1".to_owned()),
            result: FrontendControlResult::SubmissionAccepted {
                session_id: "session-1".to_owned(),
                queued: true,
            },
        };
        state.record_voice_submission(&stable_id, &queued);
        assert!(!state.voice_reply_bindings.contains(&stable_id));
        assert!(
            state
                .queued_voice_commands
                .get(&stable_id)
                .is_some_and(|commands| commands.contains("voice-queued"))
        );

        state.activate_queued_voice_command(&stable_id, "other-command");
        assert!(!state.voice_reply_bindings.contains(&stable_id));
        state.activate_queued_voice_command(&stable_id, "voice-queued");
        assert!(state.voice_reply_bindings.contains(&stable_id));
        assert!(!state.queued_voice_commands.contains_key(&stable_id));

        state.clear_active_voice_reply(&stable_id);
        let immediate = FrontendCommandAcknowledgement {
            command_id: "voice-immediate".to_owned(),
            idempotency_key: "voice-immediate-key".to_owned(),
            session_id: Some("session-1".to_owned()),
            result: FrontendControlResult::SubmissionAccepted {
                session_id: "session-1".to_owned(),
                queued: false,
            },
        };
        state.record_voice_submission(&stable_id, &immediate);
        assert!(state.voice_reply_bindings.contains(&stable_id));
        state.clear_voice_tracking(&stable_id);
        assert!(state.validate().is_ok());
    }

    #[test]
    fn acknowledged_buffered_message_preserves_newer_transport_and_binding_cursors() {
        let repository = tempfile::tempdir().expect("repository");
        let state_path = repository.path().join(".medusa/telegram/state.json");
        let control = FrontendControlPlane::new(repository.path().to_path_buf(), Config::default());
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

        let control = FrontendControlPlane::new(repository.path().to_path_buf(), Config::default());
        let reloaded =
            TelegramSessionService::load(&state_path, gateway(), control).expect("reload");
        let binding = reloaded.binding(&identity()).expect("reloaded binding");
        assert_eq!(binding.last_update_id, Some(12));
        assert_eq!(binding.tool_progress, ToolProgressMode::All);
        assert_eq!(binding.voice_mode, TelegramVoiceMode::All);
        assert_eq!(reloaded.next_update_offset(), Some(13));
    }

    #[test]
    fn duplicate_update_is_idempotent_and_detach_clears_binding() {
        let repository = tempfile::tempdir().expect("repository");
        let session = AgentEngine::new(UnusedProvider, Config::default())
            .create_session(repository.path(), "Idempotent Telegram update".to_owned())
            .expect("session");
        let state_path = repository.path().join(".medusa/telegram/state.json");
        let control = FrontendControlPlane::new(repository.path().to_path_buf(), Config::default());
        let mut service =
            TelegramSessionService::load(&state_path, gateway(), control).expect("service");
        let attach = message(20, &format!("/attach {}", session.id));
        let first = service.process_message(20, attach.clone()).expect("first");
        let second = service.process_message(20, attach).expect("duplicate");
        assert_eq!(first, second);

        service
            .process_message(21, message(21, "/detach"))
            .expect("detach");
        assert!(service.binding(&identity()).is_none());
    }
}
