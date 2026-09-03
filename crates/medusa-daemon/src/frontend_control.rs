//! Frontend command authority wrapper.
//!
//! The inner control plane owns protocol execution. This layer owns the process-local controller
//! capability so a caller cannot replace an active session's controlling `client_id` merely by
//! submitting another owner/resume envelope.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use medusa_config::Config;
use medusa_protocol::frontend::{
    AttachmentMode as FrontendAttachmentMode, FrontendCommand, FrontendCommandEnvelope,
};

use crate::{
    artifact_store::FrontendArtifactExport, live_session::LiveSessionReplayView,
    protocol::FrontendArtifactKind,
};

#[path = "frontend_control_inner.rs"]
mod base;

pub use base::{
    FrontendCommandAcknowledgement, FrontendControlError, FrontendControlResult,
    FrontendShutdownHandle, FrontendTransientEvent,
};

pub struct FrontendControlPlane {
    inner: base::FrontendControlPlane,
    control_clients: BTreeMap<String, String>,
    applied_control_transitions: BTreeSet<String>,
}

impl FrontendControlPlane {
    #[must_use]
    pub fn new(repo: PathBuf, config: Config) -> Self {
        Self {
            inner: base::FrontendControlPlane::new(repo, config),
            control_clients: BTreeMap::new(),
            applied_control_transitions: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn shutdown_handle(&self) -> FrontendShutdownHandle {
        self.inner.shutdown_handle()
    }

    pub fn replay_events(
        &self,
        client_id: &str,
        cursor: u64,
    ) -> Result<LiveSessionReplayView, FrontendControlError> {
        self.inner.replay_events(client_id, cursor)
    }

    pub fn ingest_attachment(
        &self,
        display_name: String,
        mime_type: Option<String>,
        bytes: Vec<u8>,
    ) -> Result<String, FrontendControlError> {
        self.inner.ingest_attachment(display_name, mime_type, bytes)
    }

    pub fn ingest_artifact(
        &self,
        display_name: String,
        mime_type: Option<String>,
        kind: FrontendArtifactKind,
        bytes: Vec<u8>,
    ) -> Result<String, FrontendControlError> {
        self.inner
            .ingest_artifact(display_name, mime_type, kind, bytes)
    }

    pub fn update_credential(
        &mut self,
        provider: String,
        credential: String,
    ) -> Result<(), FrontendControlError> {
        self.inner.update_credential(provider, credential)
    }

    pub fn export_attachment(
        &self,
        artifact_id: &str,
    ) -> Result<FrontendArtifactExport, FrontendControlError> {
        self.inner.export_attachment(artifact_id)
    }

    pub fn dispatch(
        &mut self,
        envelope: FrontendCommandEnvelope,
    ) -> Result<FrontendCommandAcknowledgement, FrontendControlError> {
        self.validate_control_claim(&envelope)?;
        let client_id = envelope.client_id.clone();
        let idempotency_key = envelope.idempotency_key.clone();
        let command = envelope.command.clone();
        let acknowledgement = self.inner.dispatch(envelope)?;
        self.record_successful_control_transition(
            &idempotency_key,
            &client_id,
            &command,
            &acknowledgement,
        );
        Ok(acknowledgement)
    }

    fn validate_control_claim(
        &self,
        envelope: &FrontendCommandEnvelope,
    ) -> Result<(), FrontendControlError> {
        match &envelope.command {
            FrontendCommand::Attach {
                session_id,
                mode: FrontendAttachmentMode::Owner,
                ..
            } => validate_control_client(
                &self.control_clients,
                session_id,
                &envelope.client_id,
                false,
            ),
            FrontendCommand::ResumeSession { session_id } => validate_control_client(
                &self.control_clients,
                session_id,
                &envelope.client_id,
                true,
            ),
            _ => Ok(()),
        }
    }

    fn record_successful_control_transition(
        &mut self,
        idempotency_key: &str,
        client_id: &str,
        command: &FrontendCommand,
        acknowledgement: &FrontendCommandAcknowledgement,
    ) {
        if !self
            .applied_control_transitions
            .insert(idempotency_key.to_owned())
        {
            return;
        }

        match command {
            FrontendCommand::CreateSession { .. } => {
                if let FrontendControlResult::SubmissionAccepted { session_id, .. } =
                    &acknowledgement.result
                {
                    self.control_clients
                        .insert(session_id.clone(), client_id.to_owned());
                }
            }
            FrontendCommand::ResumeSession { session_id } => {
                self.control_clients
                    .entry(session_id.clone())
                    .or_insert_with(|| client_id.to_owned());
            }
            FrontendCommand::Detach => {
                if let FrontendControlResult::Detached { session_id, .. } = &acknowledgement.result
                    && self.control_clients.get(session_id).map(String::as_str) == Some(client_id)
                {
                    self.control_clients.remove(session_id);
                }
            }
            FrontendCommand::NewSession => {
                if let FrontendControlResult::CommandAccepted { session_id, .. } =
                    &acknowledgement.result
                {
                    self.control_clients.remove(session_id);
                }
            }
            FrontendCommand::Attach { .. }
            | FrontendCommand::ListSessions
            | FrontendCommand::Replay { .. }
            | FrontendCommand::PollTransient
            | FrontendCommand::Poll { .. }
            | FrontendCommand::AcknowledgeCursor { .. }
            | FrontendCommand::Submit { .. }
            | FrontendCommand::SubmitSessionAction { .. }
            | FrontendCommand::ShowSessionActions
            | FrontendCommand::AnswerQuestion { .. }
            | FrontendCommand::ResolveApproval { .. }
            | FrontendCommand::CancelTurn
            | FrontendCommand::RunCommand { .. }
            | FrontendCommand::PreviewSelectiveRevert { .. }
            | FrontendCommand::ApplySelectiveRevert { .. }
            | FrontendCommand::RecoveryAction { .. }
            | FrontendCommand::ShowStatus
            | FrontendCommand::ConfigureModel { .. }
            | FrontendCommand::SetEffort { .. }
            | FrontendCommand::SetPlanMode { .. }
            | FrontendCommand::SteerWorker { .. }
            | FrontendCommand::CancelWorker { .. }
            | FrontendCommand::StopTeam => {}
        }
    }
}

fn validate_control_client(
    control_clients: &BTreeMap<String, String>,
    session_id: &str,
    client_id: &str,
    allow_unclaimed: bool,
) -> Result<(), FrontendControlError> {
    match control_clients.get(session_id) {
        Some(authorized) if authorized == client_id => Ok(()),
        Some(_) => Err(FrontendControlError::ReadOnlyClient(client_id.to_owned())),
        None if allow_unclaimed => Ok(()),
        None => Err(FrontendControlError::RuntimeNotActive(
            session_id.to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn established_control_client_cannot_be_replaced() {
        let control_clients = BTreeMap::from([("session-a".to_owned(), "owner-a".to_owned())]);
        assert!(validate_control_client(&control_clients, "session-a", "owner-a", false).is_ok());
        assert!(matches!(
            validate_control_client(&control_clients, "session-a", "attacker", true),
            Err(FrontendControlError::ReadOnlyClient(client)) if client == "attacker"
        ));
    }

    #[test]
    fn resume_can_reestablish_control_only_when_unclaimed() {
        let control_clients = BTreeMap::new();
        assert!(validate_control_client(&control_clients, "session-a", "desktop-a", true).is_ok());
        assert!(matches!(
            validate_control_client(&control_clients, "session-a", "desktop-a", false),
            Err(FrontendControlError::RuntimeNotActive(session)) if session == "session-a"
        ));
    }

    #[test]
    fn cached_authority_transition_cannot_revoke_new_controller() {
        let repository = tempfile::tempdir().expect("repository");
        let mut plane = FrontendControlPlane::new(repository.path().to_path_buf(), Config::default());
        plane
            .control_clients
            .insert("session-a".to_owned(), "owner-a".to_owned());
        let acknowledgement = FrontendCommandAcknowledgement {
            command_id: "command-a".to_owned(),
            idempotency_key: "idempotency-a".to_owned(),
            session_id: Some("session-a".to_owned()),
            result: FrontendControlResult::CommandAccepted {
                session_id: "session-a".to_owned(),
                command: "new".to_owned(),
            },
        };

        plane.record_successful_control_transition(
            "idempotency-a",
            "owner-a",
            &FrontendCommand::NewSession,
            &acknowledgement,
        );
        assert!(!plane.control_clients.contains_key("session-a"));

        plane
            .control_clients
            .insert("session-a".to_owned(), "owner-b".to_owned());
        plane.record_successful_control_transition(
            "idempotency-a",
            "owner-a",
            &FrontendCommand::NewSession,
            &acknowledgement,
        );
        assert_eq!(
            plane.control_clients.get("session-a").map(String::as_str),
            Some("owner-b")
        );
    }
}
