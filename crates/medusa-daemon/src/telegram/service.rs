//! Durable Telegram chat/topic bindings and shared-control-plane routing.
//!
//! This service owns only Telegram transport state: update offsets, chat/topic bindings, delivery
//! cursors, and per-binding display/voice preferences. Session truth and command authorization stay
//! in the daemon frontend control plane and production runtime.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use medusa_protocol::frontend::FrontendCommand;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    FrontendCommandAcknowledgement, FrontendControlError, FrontendControlPlane,
    FrontendControlResult,
};

use super::{
    TelegramGateway, TelegramGatewayError, TelegramIdentity, TelegramInboundAction,
    TelegramInboundMessage, TelegramVoiceMode, ToolProgressMode,
    bot_api::TelegramUpdateCursor,
};

const TELEGRAM_SERVICE_SCHEMA_VERSION: u32 = 1;
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
    pub session_id: Option<String>,
    pub acknowledged_cursor: u64,
    pub tool_progress: ToolProgressMode,
    pub voice_mode: TelegramVoiceMode,
    pub last_update_id: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct TelegramServiceState {
    schema_version: u32,
    next_update_offset: Option<i64>,
    bindings: BTreeMap<String, TelegramSessionBinding>,
}

impl Default for TelegramServiceState {
    fn default() -> Self {
        Self {
            schema_version: TELEGRAM_SERVICE_SCHEMA_VERSION,
            next_update_offset: None,
            bindings: BTreeMap::new(),
        }
    }
}

impl TelegramServiceState {
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
        for (stable_id, binding) in &self.bindings {
            if stable_id != &binding.key.stable_id()
                || binding.client_id.trim().is_empty()
                || binding
                    .session_id
                    .as_deref()
                    .is_some_and(|session_id| session_id.trim().is_empty())
                || binding.last_update_id.is_some_and(|update_id| update_id < 0)
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
        acknowledgement: FrontendCommandAcknowledgement,
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
            state.validate()?;
            state
        } else {
            TelegramServiceState::default()
        };
        Ok(Self {
            path,
            gateway,
            control,
            state,
        })
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

    /// Processes one normalized Telegram message and persists transport state only after success.
    pub fn process_message(
        &mut self,
        update_id: i64,
        mut message: TelegramInboundMessage,
    ) -> Result<TelegramServiceOutcome, TelegramSessionServiceError> {
        if update_id < 0 {
            return Err(TelegramSessionServiceError::InvalidUpdateOffset);
        }
        let key = TelegramBindingKey::from_identity(&message.identity);
        let stable_id = key.stable_id();
        let existing = self.state.bindings.get(&stable_id).cloned();
        if existing
            .as_ref()
            .and_then(|binding| binding.last_update_id)
            .is_some_and(|last| update_id < last)
        {
            return Err(TelegramSessionServiceError::StaleUpdate(update_id));
        }
        if message.attached_session_id.is_none() {
            message.attached_session_id = existing
                .as_ref()
                .and_then(|binding| binding.session_id.clone());
        }

        let action = self.gateway.map_message(&message)?;
        let outcome = match action {
            TelegramInboundAction::Forward(envelope) => {
                let command = envelope.command.clone();
                let acknowledgement = self.control.dispatch(envelope)?;
                let binding = self.binding_after_acknowledgement(
                    key,
                    existing,
                    update_id,
                    &command,
                    &acknowledgement,
                )?;
                match binding {
                    Some(binding) => {
                        self.state.bindings.insert(stable_id, binding);
                    }
                    None => {
                        self.state.bindings.remove(&stable_id);
                    }
                }
                TelegramServiceOutcome::Forwarded { acknowledgement }
            }
            TelegramInboundAction::SetToolProgress(mode) => {
                let binding = ensure_binding(key, existing, update_id);
                self.state.bindings.insert(
                    stable_id,
                    TelegramSessionBinding {
                        tool_progress: mode,
                        ..binding
                    },
                );
                TelegramServiceOutcome::ToolProgressUpdated { mode }
            }
            TelegramInboundAction::SetVoiceMode(mode) => {
                let binding = ensure_binding(key, existing, update_id);
                self.state.bindings.insert(
                    stable_id,
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
                    .map_or(self.gateway.config().voice.mode, |binding| binding.voice_mode),
            },
            TelegramInboundAction::StartLiveVoice => TelegramServiceOutcome::StartLiveVoice,
            TelegramInboundAction::Help => TelegramServiceOutcome::Help,
        };

        self.acknowledge_update(update_id)?;
        self.persist()?;
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
        let entry = self
            .state
            .bindings
            .get_mut(&binding.key.stable_id())
            .ok_or(TelegramSessionServiceError::BindingNotFound)?;
        entry.acknowledged_cursor = entry.acknowledged_cursor.max(cursor);
        self.persist()?;
        Ok(acknowledgement)
    }

    fn binding_after_acknowledgement(
        &self,
        key: TelegramBindingKey,
        existing: Option<TelegramSessionBinding>,
        update_id: i64,
        command: &FrontendCommand,
        acknowledgement: &FrontendCommandAcknowledgement,
    ) -> Result<Option<TelegramSessionBinding>, TelegramSessionServiceError> {
        if matches!(command, FrontendCommand::Detach) {
            return Ok(None);
        }
        let mut binding = ensure_binding(key, existing, update_id);
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
            | FrontendControlResult::Status { session_id, .. }
            | FrontendControlResult::Events { session_id, .. } => Some(session_id.clone()),
            FrontendControlResult::Sessions { .. }
            | FrontendControlResult::Detached { .. } => None,
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

    fn persist(&self) -> Result<(), TelegramSessionServiceError> {
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

fn ensure_binding(
    key: TelegramBindingKey,
    existing: Option<TelegramSessionBinding>,
    update_id: i64,
) -> TelegramSessionBinding {
    existing.map_or_else(
        || TelegramSessionBinding {
            client_id: format!("telegram:{}", key.stable_id()),
            key,
            session_id: None,
            acknowledged_cursor: 0,
            tool_progress: ToolProgressMode::New,
            voice_mode: TelegramVoiceMode::Off,
            last_update_id: Some(update_id),
        },
        |mut binding| {
            binding.last_update_id = Some(update_id);
            binding
        },
    )
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
        let control =
            FrontendControlPlane::new(repository.path().to_path_buf(), Config::default());
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

        let control =
            FrontendControlPlane::new(repository.path().to_path_buf(), Config::default());
        let reloaded =
            TelegramSessionService::load(&state_path, gateway(), control).expect("reload");
        let binding = reloaded.binding(&identity()).expect("binding");
        assert_eq!(binding.session_id.as_deref(), Some(session.id.as_str()));
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
        let control =
            FrontendControlPlane::new(repository.path().to_path_buf(), Config::default());
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
