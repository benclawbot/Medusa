//! Serialized frontend command routing over the daemon live-session broker.
//!
//! Frontends send the shared protocol envelope. This control plane validates and deduplicates the
//! command, delegates attachment and replay to `LiveSessionBroker`, and routes mutations through the
//! existing `RuntimeController`. It does not create a second transcript or policy implementation.

use std::{collections::BTreeMap, path::{Path, PathBuf}};

use medusa_config::Config;
use medusa_protocol::frontend::{
    ApprovalDecision, AttachmentMode as FrontendAttachmentMode, FrontendCommand,
    FrontendCommandEnvelope, FrontendKind,
};
use medusa_runtime::{
    RecoveryActionRequest, RecoveryOperation, RuntimeController, RuntimeEvent, SubmitDisposition,
    attachment::session::{AttachmentMode, ClientKind, RuntimeAttachRequest},
    commands::{
        Effort, ModelCommand, ModelConfiguration, SlashCommand, TeamCommand, parse_slash_command,
    },
    frontend::{
        SessionActionAdmission, SessionActionRequest, SessionActionSnapshot,
        session_action_snapshot,
    },
    prompt::PromptDraft,
    recovery_action_context,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    artifact_store::{
        FrontendArtifactExport, FrontendArtifactInput, FrontendArtifactStore,
        FrontendArtifactStoreError,
    },
    live_session::{
        LiveSessionAttachmentView, LiveSessionBroker, LiveSessionBrokerError,
        LiveSessionReplayView, LiveSessionSummary,
    },
    protocol::FrontendArtifactKind,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FrontendTransientEvent {
    RecoveryAvailable {
        recovery: serde_json::Value,
    },
    RecoveryCompleted {
        record: serde_json::Value,
        audit_path: String,
    },
    Settings {
        model: String,
        effort: String,
        plan_mode: bool,
        credential_configured: bool,
    },
    ConfigurationChanged {
        revision: u64,
        active_profile: String,
        changed_keys: Vec<String>,
        origin: String,
        apply_timing: String,
    },
    Notice {
        title: String,
        details: Vec<String>,
    },
    NewSession,
    Progress {
        turn: u32,
    },
    Failed {
        message: String,
    },
}

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
        replay: LiveSessionReplayView,
    },
    Transient {
        events: Vec<FrontendTransientEvent>,
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
    SessionActionAccepted {
        session_id: String,
        admission: SessionActionAdmission,
    },
    SessionActions {
        session_id: String,
        snapshot: SessionActionSnapshot,
    },
    CancellationRequested {
        session_id: String,
        requested: bool,
    },
    CommandAccepted {
        session_id: String,
        command: String,
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

pub struct FrontendControlPlane {
    repo: PathBuf,
    config: Config,
    broker: LiveSessionBroker,
    controllers: BTreeMap<String, RuntimeController>,
    control_clients: BTreeMap<String, String>,
    acknowledgements: BTreeMap<String, CachedAcknowledgement>,
    credentials: BTreeMap<String, String>,
    artifacts: FrontendArtifactStore,
}

impl FrontendControlPlane {
    #[must_use]
    pub fn new(repo: PathBuf, config: Config) -> Self {
        Self {
            broker: LiveSessionBroker::new(repo.clone()),
            artifacts: FrontendArtifactStore::new(repo.join(".medusa/frontend-artifacts")),
            repo,
            config,
            controllers: BTreeMap::new(),
            control_clients: BTreeMap::new(),
            acknowledgements: BTreeMap::new(),
            credentials: BTreeMap::new(),
        }
    }

    pub fn replay_events(
        &self,
        client_id: &str,
        cursor: u64,
    ) -> Result<LiveSessionReplayView, FrontendControlError> {
        self.broker.replay(client_id, cursor).map_err(Into::into)
    }

    pub fn ingest_attachment(
        &self,
        display_name: String,
        mime_type: Option<String>,
        bytes: Vec<u8>,
    ) -> Result<String, FrontendControlError> {
        let kind = if mime_type
            .as_deref()
            .is_some_and(|value| value.starts_with("image/"))
        {
            FrontendArtifactKind::Image
        } else {
            FrontendArtifactKind::Text
        };
        self.ingest_artifact(display_name, mime_type, kind, bytes)
    }

    pub fn ingest_artifact(
        &self,
        display_name: String,
        mime_type: Option<String>,
        kind: FrontendArtifactKind,
        bytes: Vec<u8>,
    ) -> Result<String, FrontendControlError> {
        self.artifacts
            .ingest(FrontendArtifactInput {
                display_name,
                mime_type,
                kind,
                bytes,
            })
            .map_err(Into::into)
    }

    pub fn update_credential(
        &mut self,
        provider: String,
        credential: String,
    ) -> Result<(), FrontendControlError> {
        let provider = provider.trim().to_ascii_lowercase();
        if provider.is_empty() || credential.trim().is_empty() {
            return Err(FrontendControlError::InvalidCredentialUpdate);
        }
        self.credentials.insert(provider, credential);
        Ok(())
    }

    pub fn export_attachment(
        &self,
        artifact_id: &str,
    ) -> Result<FrontendArtifactExport, FrontendControlError> {
        self.artifacts.export(artifact_id).map_err(Into::into)
    }

    pub fn dispatch(
        &mut self,
        envelope: FrontendCommandEnvelope,
    ) -> Result<FrontendCommandAcknowledgement, FrontendControlError> {
        envelope
            .validate()
            .map_err(FrontendControlError::InvalidEnvelope)?;
        let cacheable = command_is_cacheable(&envelope.command);
        let command_fingerprint = cacheable.then(|| fingerprint(&envelope)).transpose()?;
        if cacheable {
            if let Some(cached) = self.acknowledgements.get(&envelope.idempotency_key) {
                if Some(&cached.command_fingerprint) == command_fingerprint.as_ref() {
                    return Ok(cached.acknowledgement.clone());
                }
                return Err(FrontendControlError::IdempotencyConflict(
                    envelope.idempotency_key,
                ));
            }
        }

        let session_id = command_session_id(&envelope);
        let result = self.execute(&envelope)?;
        let acknowledgement = FrontendCommandAcknowledgement {
            command_id: envelope.command_id.clone(),
            idempotency_key: envelope.idempotency_key.clone(),
            session_id,
            result,
        };
        if let Some(command_fingerprint) = command_fingerprint {
            self.acknowledgements.insert(
                envelope.idempotency_key,
                CachedAcknowledgement {
                    command_fingerprint,
                    acknowledgement: acknowledgement.clone(),
                },
            );
        }
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
                if *mode == FrontendAttachmentMode::Owner {
                    if !self.controllers.contains_key(session_id) {
                        return Err(FrontendControlError::RuntimeNotActive(session_id.clone()));
                    }
                    self.control_clients
                        .insert(session_id.clone(), envelope.client_id.clone());
                }
                let attachment = self.attach_frontend(
                    session_id,
                    envelope,
                    after_cursor.unwrap_or_default(),
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
            FrontendCommand::Replay { after_cursor } => {
                let replay = self.replay_events(&envelope.client_id, *after_cursor)?;
                Ok(FrontendControlResult::Events { replay })
            }
            FrontendCommand::PollTransient => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                let events = self.poll_transient(&session_id)?;
                Ok(FrontendControlResult::Transient { events })
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
                    let attachment = self.attach_frontend(
                        session_id,
                        envelope,
                        0,
                        format!("{}:frontend", envelope.command_id),
                    )?;
                    self.control_clients
                        .insert(session_id.clone(), envelope.client_id.clone());
                    return Ok(FrontendControlResult::RuntimeReady { attachment });
                }

                let daemon_client_id = format!("daemon-runtime:{session_id}");
                self.broker.attach_current(RuntimeAttachRequest {
                    session_id: session_id.clone(),
                    client_id: daemon_client_id.clone(),
                    client_kind: ClientKind::Daemon,
                    requested_mode: AttachmentMode::Owner,
                    expected_revision: 0,
                    cursor: 0,
                    occurred_at_unix_ms: timestamp_unix_ms(envelope.timestamp),
                    event_id: format!("{}:daemon-owner", envelope.command_id),
                })?;
                let attachment = self.attach_frontend(
                    session_id,
                    envelope,
                    0,
                    format!("{}:frontend", envelope.command_id),
                )?;
                let controller = self.broker.resume_owner(&daemon_client_id)?;
                self.configure_controller(&controller, current_effort(&self.config))?;
                self.controllers.insert(session_id.clone(), controller);
                self.control_clients
                    .insert(session_id.clone(), envelope.client_id.clone());
                Ok(FrontendControlResult::RuntimeReady { attachment })
            }
            FrontendCommand::CreateSession {
                repository_profile: _,
                objective,
                attachment_ids,
            } => {
                let attachments = self.artifacts.resolve(attachment_ids)?;
                let text = objective.clone().unwrap_or_default();
                if text.trim().is_empty() && attachments.is_empty() {
                    return Err(FrontendControlError::ObjectiveRequired);
                }
                let controller =
                    RuntimeController::start_with_config(self.repo.clone(), self.config.clone());
                self.configure_controller(&controller, current_effort(&self.config))?;
                let disposition = controller.submit(PromptDraft {
                    text,
                    attachments,
                    revision: 0,
                })?;
                let session_id = controller
                    .active_session_id()
                    .ok_or(FrontendControlError::RuntimeDidNotAcceptSession)?;
                self.broker.attach_current(RuntimeAttachRequest {
                    session_id: session_id.clone(),
                    client_id: format!("daemon-runtime:{session_id}"),
                    client_kind: ClientKind::Daemon,
                    requested_mode: AttachmentMode::Owner,
                    expected_revision: 0,
                    cursor: 0,
                    occurred_at_unix_ms: timestamp_unix_ms(envelope.timestamp),
                    event_id: format!("{}:daemon-owner", envelope.command_id),
                })?;
                self.attach_frontend(
                    &session_id,
                    envelope,
                    0,
                    format!("{}:frontend", envelope.command_id),
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
                let attachments = self.artifacts.resolve(attachment_ids)?;
                self.submit_draft(
                    envelope,
                    PromptDraft {
                        text: text.clone(),
                        attachments,
                        revision: 0,
                    },
                )
            }
            FrontendCommand::SubmitSessionAction {
                kind,
                delivery_policy,
                wake_policy,
                expected_session_revision,
                payload,
            } => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                let admission =
                    self.controller(&session_id)?
                        .submit_session_action(SessionActionRequest {
                            idempotency_key: envelope.idempotency_key.clone(),
                            source: frontend_source(envelope.frontend, &envelope.client_id),
                            target_session_id: session_id.clone(),
                            expected_session_revision: *expected_session_revision,
                            kind: *kind,
                            delivery_policy: *delivery_policy,
                            wake_policy: *wake_policy,
                            payload: payload.clone(),
                        })?;
                Ok(FrontendControlResult::SessionActionAccepted {
                    session_id,
                    admission,
                })
            }
            FrontendCommand::ShowSessionActions => {
                let session_id = required_session_id(envelope)?;
                let replay = self.replay_events(&envelope.client_id, 0)?;
                if replay.session_id != session_id {
                    return Err(FrontendControlError::InvalidCommand(
                        "frontend attachment does not belong to requested action session"
                            .to_owned(),
                    ));
                }
                let snapshot = session_action_snapshot(&self.repo, &session_id)?;
                Ok(FrontendControlResult::SessionActions {
                    session_id,
                    snapshot,
                })
            }
            FrontendCommand::AnswerQuestion { answer, .. } => self.submit_text(envelope, answer),
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
            FrontendCommand::NewSession => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                let controller = self
                    .controllers
                    .remove(&session_id)
                    .ok_or_else(|| FrontendControlError::RuntimeNotActive(session_id.clone()))?;
                if let Err(error) = controller.run_command(SlashCommand::New) {
                    self.controllers.insert(session_id.clone(), controller);
                    return Err(error.into());
                }
                drop(controller);
                self.control_clients.remove(&session_id);
                let _ = self.broker.detach(
                    &envelope.client_id,
                    timestamp_unix_ms(envelope.timestamp),
                    format!("{}:detach", envelope.command_id),
                );
                Ok(FrontendControlResult::CommandAccepted {
                    session_id,
                    command: "new_session".to_owned(),
                })
            }
            FrontendCommand::RunCommand { input } => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                let command = parse_slash_command(input)
                    .map_err(|error| FrontendControlError::InvalidCommand(error.to_string()))?
                    .ok_or_else(|| {
                        FrontendControlError::InvalidCommand(
                            "runtime command must be a slash command".to_owned(),
                        )
                    })?;
                match command {
                    SlashCommand::New => {
                        return Err(FrontendControlError::UnsupportedCommand(
                            "use the shared new-session command",
                        ));
                    }
                    SlashCommand::Model(ModelCommand::SetApiKey(_)) => {
                        return Err(FrontendControlError::UnsupportedCommand(
                            "credentials use the non-durable local credential channel",
                        ));
                    }
                    command => self.controller(&session_id)?.run_command(command)?,
                }
                Ok(FrontendControlResult::CommandAccepted {
                    session_id,
                    command: "run_command".to_owned(),
                })
            }
            FrontendCommand::PreviewSelectiveRevert { mutation_id } => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                let preview = medusa_agent::preview_session_selective_revert(
                    &self.repo,
                    &session_id,
                    mutation_id,
                )
                .map_err(|error| FrontendControlError::InvalidCommand(error.to_string()))?;
                let command = serde_json::to_string(&serde_json::json!({
                    "type": "selective_revert_preview",
                    "mutation_id": preview.mutation_id,
                    "path": preview.path,
                    "start_byte": preview.start_byte,
                    "remove_len": preview.remove_len,
                    "restore_len": preview.restore_len,
                }))
                .map_err(|error| FrontendControlError::InvalidCommand(error.to_string()))?;
                Ok(FrontendControlResult::CommandAccepted {
                    session_id,
                    command,
                })
            }
            FrontendCommand::ApplySelectiveRevert { mutation_id } => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                let outcome = medusa_agent::apply_session_selective_revert(
                    &self.repo,
                    &session_id,
                    mutation_id,
                    &envelope.command_id,
                    "frontend-control",
                )
                .map_err(|error| FrontendControlError::InvalidCommand(error.to_string()))?;
                let review_fingerprint = repository_review_fingerprint(&self.repo)?;
                let command = serde_json::to_string(&serde_json::json!({
                    "type": "selective_revert_applied",
                    "mutation_id": mutation_id,
                    "inverse_mutation_ids": outcome.mutation_ids,
                    "verification_invalidated": true,
                    "review_fingerprint": review_fingerprint,
                    "review_refresh_required": false,
                }))
                .map_err(|error| FrontendControlError::InvalidCommand(error.to_string()))?;
                Ok(FrontendControlResult::CommandAccepted {
                    session_id,
                    command,
                })
            }
            FrontendCommand::RecoveryAction {
                operation,
                checkpoint_id,
                confirmed_destructive_effects,
            } => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                let request = RecoveryActionRequest {
                    session_id: session_id.clone(),
                    operation: parse_recovery_operation(operation)?,
                    checkpoint_id: checkpoint_id.clone(),
                    confirmed_destructive_effects: *confirmed_destructive_effects,
                };
                let (view, preflight) = recovery_action_context(&self.repo, &request)
                    .map_err(|error| FrontendControlError::Recovery(error.to_string()))?;
                self.controller(&session_id)?
                    .execute_recovery(view, request, preflight)?;
                Ok(FrontendControlResult::CommandAccepted {
                    session_id,
                    command: "recovery_action".to_owned(),
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
            FrontendCommand::ConfigureModel { provider, model } => {
                let provider = provider
                    .clone()
                    .unwrap_or_else(|| self.config.model.provider.clone());
                let effort = current_effort(&self.config);
                let configuration = self.model_configuration(&provider, model, effort);
                if let Some(session_id) = envelope.session_id.as_deref() {
                    self.authorize_control(session_id, &envelope.client_id)?;
                    self.controller(session_id)?
                        .configure_model(configuration)?;
                }
                self.config.model.provider = provider;
                self.config.model.name = model.clone();
                self.config.model.protocol = protocol_for_provider(&self.config.model.provider);
                Ok(FrontendControlResult::CommandAccepted {
                    session_id: envelope.session_id.clone().unwrap_or_default(),
                    command: "configure_model".to_owned(),
                })
            }
            FrontendCommand::SetEffort { effort } => {
                let effort = parse_effort(effort)?;
                let configuration = self.model_configuration(
                    &self.config.model.provider,
                    &self.config.model.name,
                    effort,
                );
                if let Some(session_id) = envelope.session_id.as_deref() {
                    self.authorize_control(session_id, &envelope.client_id)?;
                    self.controller(session_id)?
                        .configure_model(configuration)?;
                }
                self.config.agent.max_turns = turns_for_effort(effort);
                Ok(FrontendControlResult::CommandAccepted {
                    session_id: envelope.session_id.clone().unwrap_or_default(),
                    command: "set_effort".to_owned(),
                })
            }
            FrontendCommand::SetPlanMode { enabled } => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                self.controller(&session_id)?
                    .run_command(SlashCommand::Plan {
                        task: (!enabled).then(|| "off".to_owned()),
                    })?;
                Ok(FrontendControlResult::CommandAccepted {
                    session_id,
                    command: "set_plan_mode".to_owned(),
                })
            }
            FrontendCommand::SteerWorker {
                worker_id,
                instruction,
            } => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                self.controller(&session_id)?
                    .run_command(SlashCommand::Team(TeamCommand::Steer {
                        worker_id: worker_id.clone(),
                        instruction: instruction.clone(),
                    }))?;
                Ok(FrontendControlResult::CommandAccepted {
                    session_id,
                    command: "steer_worker".to_owned(),
                })
            }
            FrontendCommand::CancelWorker { worker_id } => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                self.controller(&session_id)?
                    .run_command(SlashCommand::Team(TeamCommand::StopWorker {
                        worker_id: worker_id.clone(),
                    }))?;
                Ok(FrontendControlResult::CommandAccepted {
                    session_id,
                    command: "cancel_worker".to_owned(),
                })
            }
            FrontendCommand::StopTeam => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                self.controller(&session_id)?
                    .run_command(SlashCommand::Team(TeamCommand::StopTeam))?;
                Ok(FrontendControlResult::CommandAccepted {
                    session_id,
                    command: "stop_team".to_owned(),
                })
            }
        }
    }

    fn attach_frontend(
        &mut self,
        session_id: &str,
        envelope: &FrontendCommandEnvelope,
        cursor: u64,
        event_id: String,
    ) -> Result<LiveSessionAttachmentView, FrontendControlError> {
        self.broker
            .attach_current(RuntimeAttachRequest {
                session_id: session_id.to_owned(),
                client_id: envelope.client_id.clone(),
                client_kind: client_kind(envelope.frontend),
                requested_mode: AttachmentMode::ReadOnly,
                expected_revision: 0,
                cursor,
                occurred_at_unix_ms: timestamp_unix_ms(envelope.timestamp),
                event_id,
            })
            .map_err(Into::into)
    }

    fn submit_text(
        &mut self,
        envelope: &FrontendCommandEnvelope,
        text: &str,
    ) -> Result<FrontendControlResult, FrontendControlError> {
        self.submit_draft(
            envelope,
            PromptDraft {
                text: text.to_owned(),
                ..PromptDraft::default()
            },
        )
    }

    fn submit_draft(
        &mut self,
        envelope: &FrontendCommandEnvelope,
        draft: PromptDraft,
    ) -> Result<FrontendControlResult, FrontendControlError> {
        let session_id = required_session_id(envelope)?;
        self.authorize_control(&session_id, &envelope.client_id)?;
        let disposition = self.controller(&session_id)?.submit(draft)?;
        Ok(FrontendControlResult::SubmissionAccepted {
            session_id,
            queued: disposition == SubmitDisposition::Queued,
        })
    }

    fn poll_transient(
        &self,
        session_id: &str,
    ) -> Result<Vec<FrontendTransientEvent>, FrontendControlError> {
        let controller = self.controller(session_id)?;
        let mut events = Vec::new();
        while events.len() < 200 {
            let Some(event) = controller.try_event()? else {
                break;
            };
            if let Some(event) = map_transient_event(event)? {
                events.push(event);
            }
        }
        Ok(events)
    }

    fn configure_controller(
        &self,
        controller: &RuntimeController,
        effort: Effort,
    ) -> Result<(), FrontendControlError> {
        controller
            .configure_model(self.model_configuration(
                &self.config.model.provider,
                &self.config.model.name,
                effort,
            ))
            .map_err(Into::into)
    }

    fn model_configuration(
        &self,
        provider: &str,
        model: &str,
        effort: Effort,
    ) -> ModelConfiguration {
        ModelConfiguration {
            provider: provider.to_owned(),
            model: model.to_owned(),
            effort,
            api_key: self.credentials.get(provider).cloned(),
        }
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

    fn controller(&self, session_id: &str) -> Result<&RuntimeController, FrontendControlError> {
        self.controllers
            .get(session_id)
            .ok_or_else(|| FrontendControlError::RuntimeNotActive(session_id.to_owned()))
    }
}

fn map_transient_event(
    event: RuntimeEvent,
) -> Result<Option<FrontendTransientEvent>, FrontendControlError> {
    let event = match event {
        RuntimeEvent::RecoveryAvailable(recovery) => {
            Some(FrontendTransientEvent::RecoveryAvailable {
                recovery: serde_json::to_value(recovery)?,
            })
        }
        RuntimeEvent::Settings {
            model,
            effort,
            plan_mode,
            credential_configured,
            ..
        } => Some(FrontendTransientEvent::Settings {
            model,
            effort,
            plan_mode,
            credential_configured,
        }),
        RuntimeEvent::ConfigurationChanged(change) => {
            Some(FrontendTransientEvent::ConfigurationChanged {
                revision: change.revision,
                active_profile: change.active_profile,
                changed_keys: change.changed_keys,
                origin: change.origin.label().to_owned(),
                apply_timing: change.apply_timing.label().to_owned(),
            })
        }
        RuntimeEvent::Notice { title, details } => {
            Some(FrontendTransientEvent::Notice { title, details })
        }
        RuntimeEvent::NewSession => Some(FrontendTransientEvent::NewSession),
        RuntimeEvent::Progress { turn } => Some(FrontendTransientEvent::Progress { turn }),
        RuntimeEvent::Failed(message) if is_unjournaled_publication_failure(&message) => {
            Some(FrontendTransientEvent::Failed { message })
        }
        RuntimeEvent::RecoveryCompleted(receipt) => {
            Some(FrontendTransientEvent::RecoveryCompleted {
                record: serde_json::to_value(receipt.record)?,
                audit_path: receipt.audit_path.to_string_lossy().into_owned(),
            })
        }
        RuntimeEvent::Started
        | RuntimeEvent::AssistantText(_)
        | RuntimeEvent::Activity(_)
        | RuntimeEvent::Team(_)
        | RuntimeEvent::Plan(_)
        | RuntimeEvent::Question(_)
        | RuntimeEvent::Usage { .. }
        | RuntimeEvent::Compacted { .. }
        | RuntimeEvent::Completed { .. }
        | RuntimeEvent::TurnFinished
        | RuntimeEvent::Cancelled
        | RuntimeEvent::Failed(_) => None,
    };
    Ok(event)
}

fn is_unjournaled_publication_failure(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("journal")
        && (message.contains("publish")
            || message.contains("persist")
            || message.contains("commit"))
}

fn command_is_cacheable(command: &FrontendCommand) -> bool {
    !matches!(
        command,
        FrontendCommand::ListSessions
            | FrontendCommand::Replay { .. }
            | FrontendCommand::PollTransient
            | FrontendCommand::ShowSessionActions
            | FrontendCommand::ShowStatus
    )
}

fn parse_effort(value: &str) -> Result<Effort, FrontendControlError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" => Ok(Effort::Low),
        "medium" => Ok(Effort::Medium),
        "high" => Ok(Effort::High),
        "auto" => Ok(Effort::Auto),
        _ => Err(FrontendControlError::InvalidEffort(value.to_owned())),
    }
}

fn current_effort(config: &Config) -> Effort {
    match config.agent.max_turns {
        0..=99 => Effort::Low,
        100..=299 => Effort::Medium,
        _ => Effort::High,
    }
}

fn turns_for_effort(effort: Effort) -> u32 {
    match effort {
        Effort::Low => 64,
        Effort::Medium | Effort::Auto => 200,
        Effort::High => 500,
    }
}

fn protocol_for_provider(provider: &str) -> String {
    match provider {
        "anthropic" | "anthropic-compatible" => "anthropic".to_owned(),
        _ => "openai".to_owned(),
    }
}

fn parse_recovery_operation(value: &str) -> Result<RecoveryOperation, FrontendControlError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "inspect" => Ok(RecoveryOperation::Inspect),
        "resume" => Ok(RecoveryOperation::Resume),
        "restorecheckpoint" | "restore_checkpoint" | "restore-checkpoint" => {
            Ok(RecoveryOperation::RestoreCheckpoint)
        }
        "retryverification" | "retry_verification" | "retry-verification" => {
            Ok(RecoveryOperation::RetryVerification)
        }
        "abandon" => Ok(RecoveryOperation::Abandon),
        _ => Err(FrontendControlError::InvalidRecoveryOperation(
            value.to_owned(),
        )),
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

fn required_session_id(envelope: &FrontendCommandEnvelope) -> Result<String, FrontendControlError> {
    command_session_id(envelope).ok_or(FrontendControlError::SessionRequired)
}

fn frontend_source(kind: FrontendKind, client_id: &str) -> String {
    let kind = match kind {
        FrontendKind::Tui => "tui",
        FrontendKind::Desktop => "desktop",
        FrontendKind::Telegram => "telegram",
        FrontendKind::Headless => "headless",
        FrontendKind::Other => "other",
    };
    format!("frontend:{kind}:{client_id}")
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

fn repository_review_fingerprint(repo: &Path) -> Result<String, FrontendControlError> {
    let output = std::process::Command::new("git")
        .args(["diff", "--binary", "--no-ext-diff", "--", "."])
        .current_dir(repo)
        .output()
        .map_err(|error| FrontendControlError::InvalidCommand(error.to_string()))?;
    if !output.status.success() {
        return Err(FrontendControlError::InvalidCommand(
            "could not refresh repository review fingerprint after selective revert".to_owned(),
        ));
    }
    Ok(hex::encode(Sha256::digest(&output.stdout)))
}

#[derive(Debug, Error)]
pub enum FrontendControlError {
    #[error("invalid frontend command envelope: {0}")]
    InvalidEnvelope(&'static str),
    #[error("frontend idempotency key {0} was reused for a different command")]
    IdempotencyConflict(String),
    #[error("frontend command requires a session id")]
    SessionRequired,
    #[error("create-session requires text or an attachment")]
    ObjectiveRequired,
    #[error("runtime did not expose a durable session after accepting the objective")]
    RuntimeDidNotAcceptSession,
    #[error("runtime for session {0} is not active")]
    RuntimeNotActive(String),
    #[error("invalid effort level {0}")]
    InvalidEffort(String),
    #[error("invalid recovery operation {0}")]
    InvalidRecoveryOperation(String),
    #[error("invalid runtime command: {0}")]
    InvalidCommand(String),
    #[error("frontend credential update is invalid")]
    InvalidCredentialUpdate,
    #[error("frontend client {0} is attached read-only for runtime control")]
    ReadOnlyClient(String),
    #[error("unsupported frontend command: {0}")]
    UnsupportedCommand(&'static str),
    #[error("recovery context failed: {0}")]
    Recovery(String),
    #[error(transparent)]
    Artifact(#[from] FrontendArtifactStoreError),
    #[error(transparent)]
    Broker(#[from] LiveSessionBrokerError),
    #[error(transparent)]
    Runtime(#[from] medusa_runtime::RuntimeError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polling_commands_do_not_grow_the_idempotency_cache() {
        assert!(!command_is_cacheable(&FrontendCommand::Replay {
            after_cursor: 0
        }));
        assert!(!command_is_cacheable(&FrontendCommand::PollTransient));
        assert!(!command_is_cacheable(&FrontendCommand::ShowSessionActions));
        assert!(command_is_cacheable(&FrontendCommand::CancelTurn));
    }

    #[test]
    fn transient_terminal_projection_suppresses_canonical_state() {
        assert!(
            map_transient_event(RuntimeEvent::TurnFinished)
                .expect("map")
                .is_none()
        );
        assert!(matches!(
            map_transient_event(RuntimeEvent::Failed(
                "journal publication failed after commit".to_owned()
            ))
            .expect("map"),
            Some(FrontendTransientEvent::Failed { .. })
        ));
    }

    #[test]
    fn repository_review_fingerprint_refreshes_when_review_diff_changes() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(directory.path())
            .status()
            .expect("git init");
        std::fs::write(directory.path().join("value.txt"), "base\n").expect("fixture");
        std::process::Command::new("git")
            .args(["add", "value.txt"])
            .current_dir(directory.path())
            .status()
            .expect("git add");
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Medusa Test",
                "-c",
                "user.email=medusa@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ])
            .current_dir(directory.path())
            .status()
            .expect("git commit");

        let clean = repository_review_fingerprint(directory.path()).expect("clean fingerprint");
        std::fs::write(directory.path().join("value.txt"), "changed\n").expect("change");
        let changed =
            repository_review_fingerprint(directory.path()).expect("changed fingerprint");
        assert_ne!(clean, changed);
    }
}
