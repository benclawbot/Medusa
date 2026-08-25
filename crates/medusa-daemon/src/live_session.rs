//! Daemon-owned live-session discovery and attachment broker.
//!
//! The broker holds only process-local attachment handles. Durable session truth remains in the
//! canonical runtime journal and durable continuity metadata, so daemon restarts require clients to
//! reattach rather than reconstructing a parallel transcript.

use std::{collections::BTreeMap, path::PathBuf};

use medusa_agent::session_browser::{SessionSummary, list_sessions};
use medusa_protocol::{
    EventEnvelope,
    frontend::{FrontendEventEnvelope, FrontendKind, project_event},
    validate_session_id,
};
use medusa_runtime::attachment::session::{
    AttachmentMode, ClientKind, ContinuitySession, RuntimeAttachRequest, RuntimeSessionAttachment,
};
use medusa_runtime::{RuntimeController, RuntimeError};
use medusa_session_continuity::{ContinuityError, ContinuityStore};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

/// Durable session metadata exposed to daemon frontends.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LiveSessionSummary {
    pub id: String,
    pub objective: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub completed: bool,
    pub waiting_for_user: bool,
    pub turn: u32,
}

impl From<SessionSummary> for LiveSessionSummary {
    fn from(value: SessionSummary) -> Self {
        Self {
            id: value.id,
            objective: value.objective,
            created_at: value.created_at,
            updated_at: value.updated_at,
            completed: value.completed,
            waiting_for_user: value.waiting_for_user,
            turn: value.turn,
        }
    }
}

/// One frontend-scoped replay batch over an authoritative journal range.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LiveSessionReplayView {
    pub session_id: String,
    pub client_id: String,
    pub frontend: FrontendKind,
    pub after_cursor: u64,
    pub next_cursor: u64,
    pub events: Vec<FrontendEventEnvelope>,
}

/// Current daemon view of one attached frontend client.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LiveSessionAttachmentView {
    pub session: LiveSessionSummary,
    pub client_id: String,
    pub client_kind: ClientKind,
    pub frontend: FrontendKind,
    pub mode: AttachmentMode,
    pub continuity_revision: u64,
    pub acknowledged_cursor: u64,
    pub replay_cursor: u64,
    pub owner_client_id: Option<String>,
    pub replay: Vec<FrontendEventEnvelope>,
}

/// Process-local broker over journal-backed runtime attachments.
pub struct LiveSessionBroker {
    repo: PathBuf,
    attachments: BTreeMap<String, RuntimeSessionAttachment>,
}

impl LiveSessionBroker {
    #[must_use]
    pub fn new(repo: PathBuf) -> Self {
        Self {
            repo,
            attachments: BTreeMap::new(),
        }
    }

    /// Lists every durable session known for this repository.
    pub fn list_sessions(&self) -> Result<Vec<LiveSessionSummary>, LiveSessionBrokerError> {
        list_sessions(&self.repo)
            .map(|sessions| sessions.into_iter().map(Into::into).collect())
            .map_err(|error| LiveSessionBrokerError::Session(error.to_string()))
    }

    /// Attaches using the latest durable continuity revision.
    ///
    /// The daemon serializes calls to this method. A concurrent external writer still produces a
    /// normal revision conflict rather than being overwritten.
    pub fn attach_current(
        &mut self,
        mut request: RuntimeAttachRequest,
    ) -> Result<LiveSessionAttachmentView, LiveSessionBrokerError> {
        validate_session_id(&request.session_id).map_err(invalid_session_id)?;
        let store = ContinuityStore::new(
            self.repo
                .join(".medusa/continuity")
                .join(format!("{}.json", request.session_id)),
        );
        request.expected_revision = match store.load() {
            Ok(continuity) => continuity.revision,
            Err(ContinuityError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(LiveSessionBrokerError::Session(error.to_string())),
        };
        self.attach(request)
    }

    /// Attaches or refreshes one frontend client without allowing an implicit session switch.
    pub fn attach(
        &mut self,
        request: RuntimeAttachRequest,
    ) -> Result<LiveSessionAttachmentView, LiveSessionBrokerError> {
        validate_session_id(&request.session_id).map_err(invalid_session_id)?;
        if let Some(existing) = self.attachments.get(&request.client_id)
            && existing.session.id.to_string() != request.session_id
        {
            return Err(LiveSessionBrokerError::ClientBoundToDifferentSession {
                client_id: request.client_id,
                session_id: existing.session.id.to_string(),
            });
        }
        let client_id = request.client_id.clone();
        let attachment = RuntimeSessionAttachment::attach(self.repo.clone(), request)?;
        let view = attachment_view(&attachment)?;
        self.attachments.insert(client_id, attachment);
        Ok(view)
    }

    /// Replays canonical events for one attached client from an explicit durable cursor.
    pub fn replay(
        &self,
        client_id: &str,
        cursor: u64,
    ) -> Result<LiveSessionReplayView, LiveSessionBrokerError> {
        let attachment = self.attachment(client_id)?;
        let client_kind = attachment
            .continuity
            .attachments
            .iter()
            .find(|candidate| candidate.client_id == client_id)
            .map(|candidate| candidate.client_kind.clone())
            .ok_or_else(|| LiveSessionBrokerError::ClientNotAttached(client_id.to_owned()))?;
        let replay = attachment.replay_from(cursor)?;
        Ok(replay_view(attachment, &client_kind, cursor, replay))
    }

    /// Records the highest canonical journal cursor observed by one client.
    pub fn acknowledge_cursor(
        &mut self,
        client_id: &str,
        cursor: u64,
        occurred_at_unix_ms: i64,
        event_id: impl Into<String>,
    ) -> Result<LiveSessionAttachmentView, LiveSessionBrokerError> {
        let attachment = self.attachment_mut(client_id)?;
        attachment.acknowledge_cursor(cursor, occurred_at_unix_ms, event_id)?;
        attachment_view(attachment)
    }

    /// Hands mutable ownership to another already attached client and refreshes all local views.
    pub fn handoff(
        &mut self,
        from_client_id: &str,
        to_client_id: &str,
        occurred_at_unix_ms: i64,
        event_id: impl Into<String>,
    ) -> Result<Vec<LiveSessionAttachmentView>, LiveSessionBrokerError> {
        if !self.attachments.contains_key(to_client_id) {
            return Err(LiveSessionBrokerError::ClientNotAttached(
                to_client_id.to_owned(),
            ));
        }
        let session_id = self.attachment(from_client_id)?.session.id.to_string();
        if self.attachment(to_client_id)?.session.id.to_string() != session_id {
            return Err(LiveSessionBrokerError::HandoffAcrossSessions);
        }
        self.attachment_mut(from_client_id)?.refresh_continuity()?;
        self.attachment_mut(from_client_id)?.handoff(
            to_client_id.to_owned(),
            occurred_at_unix_ms,
            event_id,
        )?;
        for attachment in self.attachments.values_mut() {
            if attachment.session.id.to_string() == session_id {
                attachment.refresh_continuity()?;
            }
        }
        self.views_for_session(&session_id)
    }

    /// Detaches one client without cancelling or mutating the authoritative session.
    pub fn detach(
        &mut self,
        client_id: &str,
        occurred_at_unix_ms: i64,
        event_id: impl Into<String>,
    ) -> Result<ContinuitySession, LiveSessionBrokerError> {
        let attachment = self
            .attachments
            .remove(client_id)
            .ok_or_else(|| LiveSessionBrokerError::ClientNotAttached(client_id.to_owned()))?;
        attachment
            .detach(occurred_at_unix_ms, event_id)
            .map_err(Into::into)
    }

    /// Transfers an owner attachment into the production runtime controller.
    pub fn resume_owner(
        &mut self,
        client_id: &str,
    ) -> Result<RuntimeController, LiveSessionBrokerError> {
        let attachment = self
            .attachments
            .remove(client_id)
            .ok_or_else(|| LiveSessionBrokerError::ClientNotAttached(client_id.to_owned()))?;
        attachment.into_controller().map_err(Into::into)
    }

    fn attachment(
        &self,
        client_id: &str,
    ) -> Result<&RuntimeSessionAttachment, LiveSessionBrokerError> {
        self.attachments
            .get(client_id)
            .ok_or_else(|| LiveSessionBrokerError::ClientNotAttached(client_id.to_owned()))
    }

    fn attachment_mut(
        &mut self,
        client_id: &str,
    ) -> Result<&mut RuntimeSessionAttachment, LiveSessionBrokerError> {
        self.attachments
            .get_mut(client_id)
            .ok_or_else(|| LiveSessionBrokerError::ClientNotAttached(client_id.to_owned()))
    }

    fn views_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<LiveSessionAttachmentView>, LiveSessionBrokerError> {
        self.attachments
            .values()
            .filter(|attachment| attachment.session.id.to_string() == session_id)
            .map(attachment_view)
            .collect()
    }
}

fn attachment_view(
    attachment: &RuntimeSessionAttachment,
) -> Result<LiveSessionAttachmentView, LiveSessionBrokerError> {
    let metadata = attachment
        .continuity
        .attachments
        .iter()
        .find(|candidate| candidate.client_id == attachment.client_id())
        .ok_or_else(|| {
            LiveSessionBrokerError::ClientNotAttached(attachment.client_id().to_owned())
        })?;
    let frontend = frontend_kind(&metadata.client_kind);
    let replay_cursor = attachment
        .replay
        .last()
        .map_or(metadata.journal_cursor, |event| event.sequence);
    Ok(LiveSessionAttachmentView {
        session: LiveSessionSummary {
            id: attachment.session.id.to_string(),
            objective: attachment.session.objective.clone(),
            created_at: attachment.session.created_at,
            updated_at: attachment.session.updated_at,
            completed: attachment.session.completed,
            waiting_for_user: attachment.session.pending_question.is_some(),
            turn: attachment.session.turn,
        },
        client_id: attachment.client_id().to_owned(),
        client_kind: metadata.client_kind.clone(),
        frontend,
        mode: attachment.mode(),
        continuity_revision: attachment.continuity.revision,
        acknowledged_cursor: metadata.journal_cursor,
        replay_cursor,
        owner_client_id: attachment.continuity.owner_client_id.clone(),
        replay: project_replay(&attachment.replay, frontend),
    })
}

fn replay_view(
    attachment: &RuntimeSessionAttachment,
    client_kind: &ClientKind,
    after_cursor: u64,
    replay: Vec<EventEnvelope>,
) -> LiveSessionReplayView {
    let frontend = frontend_kind(client_kind);
    let next_cursor = replay.last().map_or(after_cursor, |event| event.sequence);
    LiveSessionReplayView {
        session_id: attachment.session.id.to_string(),
        client_id: attachment.client_id().to_owned(),
        frontend,
        after_cursor,
        next_cursor,
        events: project_replay(&replay, frontend),
    }
}

fn project_replay(replay: &[EventEnvelope], frontend: FrontendKind) -> Vec<FrontendEventEnvelope> {
    replay
        .iter()
        .filter_map(|event| project_event(event, event.sequence, frontend))
        .collect()
}

const fn frontend_kind(client_kind: &ClientKind) -> FrontendKind {
    match client_kind {
        ClientKind::Tui => FrontendKind::Tui,
        ClientKind::Desktop => FrontendKind::Desktop,
        ClientKind::Telegram => FrontendKind::Telegram,
        ClientKind::Daemon | ClientKind::Other(_) => FrontendKind::Other,
    }
}

#[derive(Debug, Error)]
pub enum LiveSessionBrokerError {
    #[error("live-session client {0} is not attached")]
    ClientNotAttached(String),
    #[error("live-session client {client_id} is already bound to session {session_id}")]
    ClientBoundToDifferentSession {
        client_id: String,
        session_id: String,
    },
    #[error("live-session ownership cannot be handed across sessions")]
    HandoffAcrossSessions,
    #[error("durable session discovery failed: {0}")]
    Session(String),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

fn invalid_session_id(reason: &str) -> LiveSessionBrokerError {
    LiveSessionBrokerError::Session(format!("invalid live-session session identifier: {reason}"))
}

#[cfg(test)]
mod tests {
    use medusa_agent::{AgentEngine, record_session_event};
    use medusa_config::Config;
    use medusa_core::MedusaResult;
    use medusa_protocol::{Actor, EventPayload, frontend::FrontendKind};
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};
    use serde_json::json;

    use super::*;

    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("session creation does not call the provider")
        }
    }

    fn request(
        session_id: &str,
        client_id: &str,
        client_kind: ClientKind,
        mode: AttachmentMode,
        expected_revision: u64,
        cursor: u64,
        event_id: &str,
    ) -> RuntimeAttachRequest {
        RuntimeAttachRequest {
            session_id: session_id.to_owned(),
            client_id: client_id.to_owned(),
            client_kind,
            requested_mode: mode,
            expected_revision,
            cursor,
            occurred_at_unix_ms: 30_000 + i64::try_from(expected_revision).unwrap_or(i64::MAX),
            event_id: event_id.to_owned(),
        }
    }

    #[test]
    fn two_clients_observe_one_transcript_and_durable_cursor() {
        let repository = tempfile::tempdir().expect("repository");
        let session = AgentEngine::new(UnusedProvider, Config::default())
            .create_session(repository.path(), "One shared transcript".to_owned())
            .expect("session");
        let session_id = session.id.to_string();
        let mut broker = LiveSessionBroker::new(repository.path().to_path_buf());
        let owner = broker
            .attach(request(
                &session_id,
                "tui-owner",
                ClientKind::Tui,
                AttachmentMode::Owner,
                0,
                0,
                "attach-owner",
            ))
            .expect("owner");
        let observer = broker
            .attach(request(
                &session_id,
                "telegram-observer",
                ClientKind::Telegram,
                AttachmentMode::ReadOnly,
                owner.continuity_revision,
                0,
                "attach-observer",
            ))
            .expect("observer");
        assert_eq!(owner.session.id, observer.session.id);
        assert_eq!(owner.frontend, FrontendKind::Tui);
        assert_eq!(observer.frontend, FrontendKind::Telegram);
        assert_eq!(owner.replay_cursor, observer.replay_cursor);
        assert_eq!(owner.replay.len(), observer.replay.len());
        for (owner_event, observer_event) in owner.replay.iter().zip(&observer.replay) {
            assert_eq!(owner_event.cursor, observer_event.cursor);
            assert_eq!(owner_event.event, observer_event.event);
            assert!(owner_event.event_id.ends_with(":tui"));
            assert!(observer_event.event_id.ends_with(":telegram"));
        }

        let cursor = observer.replay_cursor;
        let acknowledged = broker
            .acknowledge_cursor("telegram-observer", cursor, 30_002, "ack-observer")
            .expect("acknowledge");
        drop(broker);

        let mut restarted = LiveSessionBroker::new(repository.path().to_path_buf());
        let reattached = restarted
            .attach(request(
                &session_id,
                "telegram-observer",
                ClientKind::Telegram,
                AttachmentMode::ReadOnly,
                acknowledged.continuity_revision,
                0,
                "reattach-observer",
            ))
            .expect("reattach");
        assert_eq!(reattached.acknowledged_cursor, cursor);
        assert_eq!(reattached.replay_cursor, cursor);
        assert!(reattached.replay.is_empty());
    }

    #[test]
    fn replay_cursor_advances_through_non_presentable_events() {
        let repository = tempfile::tempdir().expect("repository");
        let mut session = AgentEngine::new(UnusedProvider, Config::default())
            .create_session(repository.path(), "Hidden replay event".to_owned())
            .expect("session");
        record_session_event(
            &mut session,
            Actor::Coordinator,
            EventPayload::AssistantMessageRecorded {
                message: json!({
                    "role": "user",
                    "content": [{"type": "text", "text": "not frontend-visible"}],
                }),
            },
        )
        .expect("persist hidden event");
        let session_id = session.id.to_string();
        let mut broker = LiveSessionBroker::new(repository.path().to_path_buf());
        let attached = broker
            .attach(request(
                &session_id,
                "desktop-observer",
                ClientKind::Desktop,
                AttachmentMode::ReadOnly,
                0,
                1,
                "attach-hidden",
            ))
            .expect("attach");
        assert_eq!(attached.frontend, FrontendKind::Desktop);
        assert_eq!(attached.acknowledged_cursor, 1);
        assert_eq!(attached.replay_cursor, 2);
        assert!(attached.replay.is_empty());

        let replay = broker.replay("desktop-observer", 1).expect("replay");
        assert_eq!(replay.frontend, FrontendKind::Desktop);
        assert_eq!(replay.after_cursor, 1);
        assert_eq!(replay.next_cursor, 2);
        assert!(replay.events.is_empty());
        let acknowledged = broker
            .acknowledge_cursor("desktop-observer", replay.next_cursor, 30_003, "ack-hidden")
            .expect("ack hidden cursor");
        assert_eq!(acknowledged.acknowledged_cursor, 2);
    }

    #[test]
    fn handoff_changes_owner_without_forking_the_session() {
        let repository = tempfile::tempdir().expect("repository");
        let session = AgentEngine::new(UnusedProvider, Config::default())
            .create_session(repository.path(), "Handoff one runtime".to_owned())
            .expect("session");
        let session_id = session.id.to_string();
        let mut broker = LiveSessionBroker::new(repository.path().to_path_buf());
        let owner = broker
            .attach(request(
                &session_id,
                "desktop-owner",
                ClientKind::Desktop,
                AttachmentMode::Owner,
                0,
                0,
                "attach-desktop",
            ))
            .expect("owner");
        broker
            .attach(request(
                &session_id,
                "telegram-owner",
                ClientKind::Telegram,
                AttachmentMode::ReadOnly,
                owner.continuity_revision,
                0,
                "attach-telegram",
            ))
            .expect("telegram");

        let views = broker
            .handoff(
                "desktop-owner",
                "telegram-owner",
                31_000,
                "handoff-telegram",
            )
            .expect("handoff");
        assert!(views.iter().all(|view| view.session.id == session_id));
        assert_eq!(
            views
                .iter()
                .find(|view| view.client_id == "desktop-owner")
                .expect("desktop")
                .mode,
            AttachmentMode::ReadOnly
        );
        assert_eq!(
            views
                .iter()
                .find(|view| view.client_id == "telegram-owner")
                .expect("telegram")
                .mode,
            AttachmentMode::Owner
        );
        drop(broker.resume_owner("telegram-owner").expect("controller"));
    }

    #[test]
    fn one_client_cannot_switch_sessions_without_detaching() {
        let repository = tempfile::tempdir().expect("repository");
        let engine = AgentEngine::new(UnusedProvider, Config::default());
        let first = engine
            .create_session(repository.path(), "First session".to_owned())
            .expect("first");
        let second = engine
            .create_session(repository.path(), "Second session".to_owned())
            .expect("second");
        let mut broker = LiveSessionBroker::new(repository.path().to_path_buf());
        broker
            .attach(request(
                &first.id.to_string(),
                "telegram-42",
                ClientKind::Telegram,
                AttachmentMode::ReadOnly,
                0,
                0,
                "attach-first",
            ))
            .expect("first attach");
        let error = broker
            .attach(request(
                &second.id.to_string(),
                "telegram-42",
                ClientKind::Telegram,
                AttachmentMode::ReadOnly,
                0,
                0,
                "attach-second",
            ))
            .expect_err("session switch must fail");
        assert!(error.to_string().contains("already bound"));
    }

    #[test]
    fn rejects_malicious_session_ids_before_continuity_path_access() {
        let repository = tempfile::tempdir().expect("repository");
        let mut broker = LiveSessionBroker::new(repository.path().to_path_buf());
        let error = broker
            .attach_current(request(
                "../../outside",
                "malicious-client",
                ClientKind::Daemon,
                AttachmentMode::ReadOnly,
                0,
                0,
                "attach-malicious",
            ))
            .expect_err("path traversal session id must be rejected");
        assert!(matches!(
            error,
            LiveSessionBrokerError::Session(reason)
                if reason.contains("invalid live-session session identifier")
        ));
        assert!(!repository.path().join(".medusa").exists());
    }
}
