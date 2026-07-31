//! Serialized frontend command routing over the daemon live-session broker.
//!
//! Frontends send the shared protocol envelope. This control plane validates and deduplicates the
//! command, delegates attachment and replay to `LiveSessionBroker`, and routes mutations through the
//! existing `RuntimeController`. It does not create a second transcript or policy implementation.

use std::{collections::BTreeMap, path::PathBuf};

use medusa_config::Config;
use medusa_protocol::{
    EventEnvelope,
    frontend::{
        ApprovalDecision, AttachmentMode as FrontendAttachmentMode, FrontendCommand,
        FrontendCommandEnvelope, FrontendKind,
    },
};
use medusa_runtime::{RuntimeController, SubmitDisposition, prompt::PromptDraft};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::live_session::{
    LiveSessionAttachmentView, LiveSessionBroker, LiveSessionBrokerError, LiveSessionSummary,
};
use medusa_runtime::attachment::session::{AttachmentMode, ClientKind};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FrontendControlResult {
    Sessions {
        sessions: Vec<LiveSessionSummary>,
    },
    Attached {
        attachment: LiveSessionAttachmentView,
    },
    Detached {
        session_id: String,
        continuity_revision: u64,
        owner_client_id: Option<String>,
    },
    Events {
        session_id: String,
        after_cursor: u64,
        events: Vec<EventEnvelope>,
    },
    CursorAcknowledged {
        attachment: LiveSessionAttachmentView,
    },
    RuntimeReady {
        attachment: LiveSessionAttachmentView,
    },
    SubmissionAccepted {
        session_id: String,
        queued: bool,
    },
    CancellationRequested {
        session_id: String,
        requested: bool,
    },
    Status {
        session_id: String,
        runtime_active: bool,
        busy: bool,
        journal_cursor: u64,
        latest_checkpoint_cursor: Option<u64>,
        replay_equivalent: bool,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FrontendCommandAcknowledgement {
    pub command_id: String,
    pub idempotency_key: String,
    pub session_id: Option<String>,
    pub result: FrontendControlResult,
}

#[derive(Clone)]
struct CachedAcknowledgement {
    command_fingerprint: String,
    acknowledgement: FrontendCommandAcknowledgement,
}

/// Repository-scoped daemon control plane for all attached frontend kinds.
pub struct FrontendControlPlane {
    repo: PathBuf,
    config: Config,
    broker: LiveSessionBroker,
    controllers: BTreeMap<String, RuntimeController>,
    control_clients: BTreeMap<String, String>,
    acknowledgements: BTreeMap<String, CachedAcknowledgement>,
}

impl FrontendControlPlane {
    #[must_use]
    pub fn new(repo: PathBuf, config: Config) -> Self {
        Self {
            broker: LiveSessionBroker::new(repo.clone()),
            repo,
            config,
            controllers: BTreeMap::new(),
            control_clients: BTreeMap::new(),
            acknowledgements: BTreeMap::new(),
        }
    }

    /// Validates, serializes, and idempotently acknowledges one frontend command.
    pub fn dispatch(
        &mut self,
        envelope: FrontendCommandEnvelope,
    ) -> Result<FrontendCommandAcknowledgement, FrontendControlError> {
        envelope
            .validate()
            .map_err(FrontendControlError::InvalidEnvelope)?;
        let command_fingerprint = fingerprint(&envelope)?;
        if let Some(cached) = self.acknowledgements.get(&envelope.idempotency_key) {
            if cached.command_fingerprint == command_fingerprint {
                return Ok(cached.acknowledgement.clone());
            }
            return Err(FrontendControlError::IdempotencyConflict(
                envelope.idempotency_key,
            ));
        }

        let session_id = command_session_id(&envelope);
        let result = self.execute(&envelope)?;
        let acknowledgement = FrontendCommandAcknowledgement {
            command_id: envelope.command_id.clone(),
            idempotency_key: envelope.idempotency_key.clone(),
            session_id,
            result,
        };
        self.acknowledgements.insert(
            envelope.idempotency_key,
            CachedAcknowledgement {
                command_fingerprint,
                acknowledgement: acknowledgement.clone(),
            },
        );
        Ok(acknowledgement)
    }

    fn execute(
        &mut self,
        envelope: &FrontendCommandEnvelope,
    ) -> Result<FrontendControlResult, FrontendControlError> {
        match &envelope.command {
            FrontendCommand::ListSessions => Ok(FrontendControlResult::Sessions {
                sessions: self.broker.list_sessions()?,
            }),
            FrontendCommand::Attach {
                session_id,
                mode,
                after_cursor,
            } => {
                if *mode == FrontendAttachmentMode::Owner
                    && self.controllers.contains_key(session_id)
                {
                    return Err(FrontendControlError::RuntimeAlreadyActive(
                        session_id.clone(),
                    ));
                }
                let attachment = self.broker.attach_current(
                    session_id,
                    envelope.client_id.clone(),
                    client_kind(envelope.frontend),
                    attachment_mode(*mode),
                    after_cursor.unwrap_or_default(),
                    timestamp_unix_ms(envelope.timestamp),
                    envelope.command_id.clone(),
                )?;
                Ok(FrontendControlResult::Attached { attachment })
            }
            FrontendCommand::Detach => {
                let continuity = self.broker.detach(
                    &envelope.client_id,
                    timestamp_unix_ms(envelope.timestamp),
                    envelope.command_id.clone(),
                )?;
                self.control_clients
                    .retain(|_, client_id| client_id != &envelope.client_id);
                Ok(FrontendControlResult::Detached {
                    session_id: continuity.session_id,
                    continuity_revision: continuity.revision,
                    owner_client_id: continuity.owner_client_id,
                })
            }
            FrontendCommand::AcknowledgeCursor { cursor } => {
                let attachment = self.broker.acknowledge_cursor(
                    &envelope.client_id,
                    *cursor,
                    timestamp_unix_ms(envelope.timestamp),
                    envelope.command_id.clone(),
                )?;
                Ok(FrontendControlResult::CursorAcknowledged { attachment })
            }
            FrontendCommand::ResumeSession { session_id } => {
                if self.controllers.contains_key(session_id) {
                    return Err(FrontendControlError::RuntimeAlreadyActive(
                        session_id.clone(),
                    ));
                }
                let daemon_client_id = format!("daemon-runtime:{session_id}");
                self.broker.attach_current(
                    session_id,
                    daemon_client_id.clone(),
                    ClientKind::Daemon,
                    AttachmentMode::Owner,
                    0,
                    timestamp_unix_ms(envelope.timestamp),
                    format!("{}:daemon-owner", envelope.command_id),
                )?;
                let attachment = self.broker.attach_current(
                    session_id,
                    envelope.client_id.clone(),
                    client_kind(envelope.frontend),
                    AttachmentMode::ReadOnly,
                    0,
                    timestamp_unix_ms(envelope.timestamp),
                    format!("{}:frontend", envelope.command_id),
                )?;
                let controller = self.broker.resume_owner(&daemon_client_id)?;
                self.controllers.insert(session_id.clone(), controller);
                self.control_clients
                    .insert(session_id.clone(), envelope.client_id.clone());
                Ok(FrontendControlResult::RuntimeReady { attachment })
            }
            FrontendCommand::CreateSession {
                repository_profile: _,
                objective,
            } => {
                let objective = objective
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or(FrontendControlError::ObjectiveRequired)?;
                let controller = RuntimeController::start_with_config(
                    self.repo.clone(),
                    self.config.clone(),
                );
                let disposition = controller.submit(PromptDraft {
                    text: objective.to_owned(),
                    ..PromptDraft::default()
                })?;
                let session_id = controller.active_session_id().ok_or(
                    FrontendControlError::RuntimeDidNotAcceptSession,
                )?;
                self.controllers.insert(session_id.clone(), controller);
                self.control_clients
                    .insert(session_id.clone(), envelope.client_id.clone());
                Ok(FrontendControlResult::SubmissionAccepted {
                    session_id,
                    queued: disposition == SubmitDisposition::Queued,
                })
            }
            FrontendCommand::Submit {
                text,
                attachment_ids,
            } => {
                if !attachment_ids.is_empty() {
                    return Err(FrontendControlError::UnsupportedCommand(
                        "attachment submission requires the daemon artifact store",
                    ));
                }
                self.submit_text(envelope, text)
            }
            FrontendCommand::AnswerQuestion { answer, .. } => {
                self.submit_text(envelope, answer)
            }
            FrontendCommand::ResolveApproval { decision, .. } => self.submit_text(
                envelope,
                match decision {
                    ApprovalDecision::ApproveOnce => "approve",
                    ApprovalDecision::Deny => "deny",
                },
            ),
            FrontendCommand::CancelTurn => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                let requested = self.controller(&session_id)?.cancel();
                Ok(FrontendControlResult::CancellationRequested {
                    session_id,
                    requested,
                })
            }
            FrontendCommand::ShowStatus => {
                let session_id = required_session_id(envelope)?;
                let health = medusa_runtime::execution_history::inspect(&self.repo, &session_id)?;
                let latest_checkpoint =
                    medusa_runtime::checkpoint_store::latest(&self.repo, &session_id)?;
                let controller = self.controllers.get(&session_id);
                Ok(FrontendControlResult::Status {
                    session_id,
                    runtime_active: controller.is_some(),
                    busy: controller.is_some_and(RuntimeController::is_busy),
                    journal_cursor: health.journal_cursor,
                    latest_checkpoint_cursor: latest_checkpoint
                        .map(|checkpoint| checkpoint.journal_cursor),
                    replay_equivalent: health.replay.equivalent,
                })
            }
            FrontendCommand::ConfigureModel { .. }
            | FrontendCommand::SetEffort { .. }
            | FrontendCommand::SetPlanMode { .. }
            | FrontendCommand::SteerWorker { .. }
            | FrontendCommand::CancelWorker { .. }
            | FrontendCommand::StopTeam => Err(FrontendControlError::UnsupportedCommand(
                "command mapping is not yet available in the daemon control plane",
            )),
        }
    }

    fn submit_text(
        &self,
        envelope: &FrontendCommandEnvelope,
        text: &str,
    ) -> Result<FrontendControlResult, FrontendControlError> {
        let session_id = required_session_id(envelope)?;
        self.authorize_control(&session_id, &envelope.client_id)?;
        let disposition = self.controller(&session_id)?.submit(PromptDraft {
            text: text.to_owned(),
            ..PromptDraft::default()
        })?;
        Ok(FrontendControlResult::SubmissionAccepted {
            session_id,
            queued: disposition == SubmitDisposition::Queued,
        })
    }

    fn authorize_control(
        &self,
        session_id: &str,
        client_id: &str,
    ) -> Result<(), FrontendControlError> {
        match self.control_clients.get(session_id) {
            Some(authorized) if authorized == client_id => Ok(()),
            _ => Err(FrontendControlError::ReadOnlyClient(client_id.to_owned())),
        }
    }

    fn controller(
        &self,
        session_id: &str,
    ) -> Result<&RuntimeController, FrontendControlError> {
        self.controllers
            .get(session_id)
            .ok_or_else(|| FrontendControlError::RuntimeNotActive(session_id.to_owned()))
    }
}

fn command_session_id(envelope: &FrontendCommandEnvelope) -> Option<String> {
    match &envelope.command {
        FrontendCommand::Attach { session_id, .. }
        | FrontendCommand::ResumeSession { session_id } => Some(session_id.clone()),
        FrontendCommand::CreateSession { .. } | FrontendCommand::ListSessions => None,
        _ => envelope.session_id.clone(),
    }
}

fn required_session_id(
    envelope: &FrontendCommandEnvelope,
) -> Result<String, FrontendControlError> {
    command_session_id(envelope).ok_or(FrontendControlError::SessionRequired)
}

fn client_kind(kind: FrontendKind) -> ClientKind {
    match kind {
        FrontendKind::Tui => ClientKind::Tui,
        FrontendKind::Desktop => ClientKind::Desktop,
        FrontendKind::Telegram => ClientKind::Telegram,
        FrontendKind::Headless => ClientKind::Daemon,
        FrontendKind::Other => ClientKind::Other("frontend".to_owned()),
    }
}

fn attachment_mode(mode: FrontendAttachmentMode) -> AttachmentMode {
    match mode {
        FrontendAttachmentMode::Owner => AttachmentMode::Owner,
        FrontendAttachmentMode::ReadOnly => AttachmentMode::ReadOnly,
    }
}

fn timestamp_unix_ms(timestamp: time::OffsetDateTime) -> i64 {
    let value = timestamp.unix_timestamp_nanos() / 1_000_000;
    i64::try_from(value).unwrap_or(if value.is_negative() {
        i64::MIN
    } else {
        i64::MAX
    })
}

fn fingerprint(envelope: &FrontendCommandEnvelope) -> Result<String, FrontendControlError> {
    let bytes = serde_json::to_vec(envelope)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

#[derive(Debug, Error)]
pub enum FrontendControlError {
    #[error("invalid frontend command envelope: {0}")]
    InvalidEnvelope(&'static str),
    #[error("frontend idempotency key {0} was reused for a different command")]
    IdempotencyConflict(String),
    #[error("frontend command requires a session id")]
    SessionRequired,
    #[error("create-session requires a non-empty objective")]
    ObjectiveRequired,
    #[error("runtime did not expose a durable session after accepting the objective")]
    RuntimeDidNotAcceptSession,
    #[error("runtime for session {0} is already active")]
    RuntimeAlreadyActive(String),
    #[error("runtime for session {0} is not active")]
    RuntimeNotActive(String),
    #[error("frontend client {0} is attached read-only for runtime control")]
    ReadOnlyClient(String),
    #[error("unsupported frontend command: {0}")]
    UnsupportedCommand(&'static str),
    #[error(transparent)]
    Broker(#[from] LiveSessionBrokerError),
    #[error(transparent)]
    Runtime(#[from] medusa_runtime::RuntimeError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use medusa_agent::AgentEngine;
    use medusa_core::MedusaResult;
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};
    use medusa_protocol::frontend::FRONTEND_PROTOCOL_VERSION;
    use time::macros::datetime;

    use super::*;

    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("session creation does not call the provider")
        }
    }

    fn envelope(
        command_id: &str,
        idempotency_key: &str,
        session_id: Option<&str>,
        command: FrontendCommand,
    ) -> FrontendCommandEnvelope {
        FrontendCommandEnvelope {
            protocol_version: FRONTEND_PROTOCOL_VERSION,
            command_id: command_id.to_owned(),
            idempotency_key: idempotency_key.to_owned(),
            frontend: FrontendKind::Telegram,
            client_id: "telegram-42".to_owned(),
            session_id: session_id.map(str::to_owned),
            turn_id: None,
            timestamp: datetime!(2026-07-31 00:00 UTC),
            command,
        }
    }

    #[test]
    fn list_attach_replay_and_cursor_ack_share_one_journal() {
        let repository = tempfile::tempdir().expect("repository");
        let session = AgentEngine::new(UnusedProvider, Config::default())
            .create_session(repository.path(), "Frontend control".to_owned())
            .expect("session");
        let session_id = session.id.to_string();
        let mut control =
            FrontendControlPlane::new(repository.path().to_path_buf(), Config::default());

        let listed = control
            .dispatch(envelope(
                "list-1",
                "telegram:42:list-1",
                None,
                FrontendCommand::ListSessions,
            ))
            .expect("list");
        let FrontendControlResult::Sessions { sessions } = listed.result else {
            panic!("expected sessions")
        };
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session_id);

        let attached = control
            .dispatch(envelope(
                "attach-1",
                "telegram:42:attach-1",
                None,
                FrontendCommand::Attach {
                    session_id: session_id.clone(),
                    mode: FrontendAttachmentMode::ReadOnly,
                    after_cursor: Some(0),
                },
            ))
            .expect("attach");
        let FrontendControlResult::Attached { attachment } = attached.result else {
            panic!("expected attachment")
        };
        assert_eq!(attachment.replay, session.events);

        let cursor = u64::try_from(attachment.replay.len()).expect("cursor");
        let acknowledged = control
            .dispatch(envelope(
                "ack-1",
                "telegram:42:ack-1",
                Some(&session_id),
                FrontendCommand::AcknowledgeCursor { cursor },
            ))
            .expect("acknowledge");
        let FrontendControlResult::CursorAcknowledged { attachment } = acknowledged.result else {
            panic!("expected cursor acknowledgement")
        };
        assert_eq!(attachment.acknowledged_cursor, cursor);
    }

    #[test]
    fn identical_command_is_idempotent_and_conflicting_reuse_fails_closed() {
        let repository = tempfile::tempdir().expect("repository");
        let mut control =
            FrontendControlPlane::new(repository.path().to_path_buf(), Config::default());
        let request = envelope(
            "list-1",
            "telegram:42:stable",
            None,
            FrontendCommand::ListSessions,
        );
        let first = control.dispatch(request.clone()).expect("first");
        let second = control.dispatch(request).expect("second");
        assert_eq!(first, second);

        let conflict = control
            .dispatch(envelope(
                "list-2",
                "telegram:42:stable",
                None,
                FrontendCommand::ListSessions,
            ))
            .expect_err("conflicting key");
        assert!(matches!(
            conflict,
            FrontendControlError::IdempotencyConflict(_)
        ));
    }
}
