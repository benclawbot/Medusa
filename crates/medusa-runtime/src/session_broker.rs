use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
};

use medusa_agent::session_browser::{SessionSummary, list_sessions, load_session, replay_events};
use medusa_protocol::{
    EventEnvelope,
    frontend::{AttachmentMode, FrontendKind},
};
use medusa_session_continuity::{
    AcknowledgeRequest, AttachRequest, AttachmentMode as ContinuityAttachmentMode, ClientKind,
    ContinuityError, ContinuitySession, ContinuityStore, DetachRequest, HandoffRequest,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

#[derive(Clone, Debug)]
pub struct LiveSessionBroker {
    repo: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionAttachmentRequest {
    pub session_id: String,
    pub client_id: String,
    pub frontend: FrontendKind,
    pub mode: AttachmentMode,
    pub after_cursor: Option<u64>,
    pub expected_revision: Option<u64>,
    pub command_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub occurred_at: OffsetDateTime,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionDetachRequest {
    pub session_id: String,
    pub client_id: String,
    pub expected_revision: u64,
    pub command_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub occurred_at: OffsetDateTime,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionHandoffRequest {
    pub session_id: String,
    pub from_client_id: String,
    pub to_client_id: String,
    pub expected_revision: u64,
    pub command_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub occurred_at: OffsetDateTime,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionCursorAcknowledgeRequest {
    pub session_id: String,
    pub client_id: String,
    pub expected_revision: u64,
    pub cursor: u64,
    pub command_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub occurred_at: OffsetDateTime,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SessionAttachmentView {
    pub summary: SessionSummary,
    pub continuity: ContinuitySession,
    pub replay: Vec<EventEnvelope>,
    pub next_cursor: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionBrokerStatus {
    pub summary: SessionSummary,
    pub committed_cursor: u64,
    pub continuity: Option<ContinuitySession>,
}

#[derive(Debug, Error)]
pub enum SessionBrokerError {
    #[error("session journal operation failed: {0}")]
    Journal(String),
    #[error("session continuity operation failed: {0}")]
    Continuity(#[from] ContinuityError),
    #[error("session cursor {cursor} exceeds committed cursor {committed}")]
    CursorOutOfRange { cursor: u64, committed: u64 },
    #[error("session cursor calculation overflowed")]
    CursorOverflow,
    #[error("session broker identifier cannot be empty")]
    EmptyIdentifier,
}

impl LiveSessionBroker {
    #[must_use]
    pub fn new(repo: PathBuf) -> Self {
        Self { repo }
    }

    #[must_use]
    pub fn repo(&self) -> &Path {
        &self.repo
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>, SessionBrokerError> {
        list_sessions(&self.repo).map_err(journal_error)
    }

    pub fn status(&self, session_id: &str) -> Result<SessionBrokerStatus, SessionBrokerError> {
        validate_identifier(session_id)?;
        let session = load_session(&self.repo, session_id).map_err(journal_error)?;
        let summary = summary(&session);
        let committed_cursor = event_count(&session.events)?;
        let continuity = match self.store(session_id).load() {
            Ok(continuity) => Some(continuity),
            Err(ContinuityError::Io(error)) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        Ok(SessionBrokerStatus {
            summary,
            committed_cursor,
            continuity,
        })
    }

    pub fn attach(
        &self,
        request: SessionAttachmentRequest,
    ) -> Result<SessionAttachmentView, SessionBrokerError> {
        validate_identifier(&request.session_id)?;
        validate_identifier(&request.client_id)?;
        validate_identifier(&request.command_id)?;
        let session = load_session(&self.repo, &request.session_id).map_err(journal_error)?;
        let store = self.store(&request.session_id);
        let continuity = load_or_create(&store, &request.session_id)?;
        let expected_revision = request.expected_revision.unwrap_or(continuity.revision);
        let applied = store.attach(AttachRequest {
            client_id: request.client_id,
            client_kind: client_kind(request.frontend),
            requested_mode: attachment_mode(request.mode),
            expected_revision,
            occurred_at_unix_ms: unix_millis(request.occurred_at)?,
            event_id: request.command_id,
        })?;
        let after_cursor = request.after_cursor.unwrap_or(0);
        let replay = replay_events(&self.repo, &request.session_id, after_cursor)
            .map_err(journal_error)?;
        let replay_len =
            u64::try_from(replay.len()).map_err(|_| SessionBrokerError::CursorOverflow)?;
        let next_cursor = after_cursor
            .checked_add(replay_len)
            .ok_or(SessionBrokerError::CursorOverflow)?;
        Ok(SessionAttachmentView {
            summary: summary(&session),
            continuity: applied.session().clone(),
            replay,
            next_cursor,
        })
    }

    pub fn detach(
        &self,
        request: SessionDetachRequest,
    ) -> Result<ContinuitySession, SessionBrokerError> {
        validate_identifier(&request.session_id)?;
        validate_identifier(&request.client_id)?;
        validate_identifier(&request.command_id)?;
        load_session(&self.repo, &request.session_id).map_err(journal_error)?;
        let outcome = self.store(&request.session_id).detach(DetachRequest {
            client_id: request.client_id,
            expected_revision: request.expected_revision,
            occurred_at_unix_ms: unix_millis(request.occurred_at)?,
            event_id: request.command_id,
        })?;
        Ok(outcome.session().clone())
    }

    pub fn handoff(
        &self,
        request: SessionHandoffRequest,
    ) -> Result<ContinuitySession, SessionBrokerError> {
        validate_identifier(&request.session_id)?;
        validate_identifier(&request.from_client_id)?;
        validate_identifier(&request.to_client_id)?;
        validate_identifier(&request.command_id)?;
        load_session(&self.repo, &request.session_id).map_err(journal_error)?;
        let outcome = self.store(&request.session_id).handoff(HandoffRequest {
            from_client_id: request.from_client_id,
            to_client_id: request.to_client_id,
            expected_revision: request.expected_revision,
            occurred_at_unix_ms: unix_millis(request.occurred_at)?,
            event_id: request.command_id,
        })?;
        Ok(outcome.session().clone())
    }

    pub fn acknowledge(
        &self,
        request: SessionCursorAcknowledgeRequest,
    ) -> Result<ContinuitySession, SessionBrokerError> {
        validate_identifier(&request.session_id)?;
        validate_identifier(&request.client_id)?;
        validate_identifier(&request.command_id)?;
        let session = load_session(&self.repo, &request.session_id).map_err(journal_error)?;
        let committed = event_count(&session.events)?;
        if request.cursor > committed {
            return Err(SessionBrokerError::CursorOutOfRange {
                cursor: request.cursor,
                committed,
            });
        }
        let outcome = self
            .store(&request.session_id)
            .acknowledge(AcknowledgeRequest {
                client_id: request.client_id,
                expected_revision: request.expected_revision,
                occurred_at_unix_ms: unix_millis(request.occurred_at)?,
                event_id: request.command_id,
                cursor: request.cursor,
            })?;
        Ok(outcome.session().clone())
    }

    fn store(&self, session_id: &str) -> ContinuityStore {
        ContinuityStore::new(
            self.repo
                .join(".medusa/continuity")
                .join(format!("{session_id}.json")),
        )
    }
}

fn load_or_create(
    store: &ContinuityStore,
    session_id: &str,
) -> Result<ContinuitySession, SessionBrokerError> {
    match store.load() {
        Ok(session) => Ok(session),
        Err(ContinuityError::Io(error)) if error.kind() == ErrorKind::NotFound => {
            store.create(session_id).map_err(Into::into)
        }
        Err(error) => Err(error.into()),
    }
}

fn summary(session: &medusa_agent::AgentSession) -> SessionSummary {
    SessionSummary {
        id: session.id.to_string(),
        objective: session.objective.clone(),
        created_at: session.created_at,
        updated_at: session.updated_at,
        completed: session.completed,
        waiting_for_user: session.pending_question.is_some(),
        turn: session.turn,
    }
}

fn event_count(events: &[EventEnvelope]) -> Result<u64, SessionBrokerError> {
    u64::try_from(events.len()).map_err(|_| SessionBrokerError::CursorOverflow)
}

fn client_kind(frontend: FrontendKind) -> ClientKind {
    match frontend {
        FrontendKind::Tui => ClientKind::Tui,
        FrontendKind::Desktop => ClientKind::Desktop,
        FrontendKind::Telegram => ClientKind::Telegram,
        FrontendKind::Headless => ClientKind::Headless,
        FrontendKind::Other => ClientKind::Other("frontend".to_owned()),
    }
}

fn attachment_mode(mode: AttachmentMode) -> ContinuityAttachmentMode {
    match mode {
        AttachmentMode::Owner => ContinuityAttachmentMode::Owner,
        AttachmentMode::ReadOnly => ContinuityAttachmentMode::ReadOnly,
    }
}

fn unix_millis(timestamp: OffsetDateTime) -> Result<i64, SessionBrokerError> {
    i64::try_from(timestamp.unix_timestamp_nanos() / 1_000_000)
        .map_err(|_| SessionBrokerError::CursorOverflow)
}

fn validate_identifier(value: &str) -> Result<(), SessionBrokerError> {
    if value.trim().is_empty() {
        Err(SessionBrokerError::EmptyIdentifier)
    } else {
        Ok(())
    }
}

fn journal_error(error: impl ToString) -> SessionBrokerError {
    SessionBrokerError::Journal(error.to_string())
}

#[cfg(test)]
mod tests {
    use medusa_config::Config;
    use medusa_core::MedusaResult;
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};
    use time::macros::datetime;

    use super::*;

    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
            unreachable!("session creation does not call the provider")
        }
    }

    #[test]
    fn telegram_attachment_replays_one_canonical_journal_and_acknowledges_cursor() {
        let repository = tempfile::tempdir().expect("repository");
        let engine = medusa_agent::AgentEngine::new(UnusedProvider, Config::default());
        let session = engine
            .create_session(repository.path(), "Continue one live session".to_owned())
            .expect("create session");
        let broker = LiveSessionBroker::new(repository.path().to_path_buf());
        let attached = broker
            .attach(SessionAttachmentRequest {
                session_id: session.id.to_string(),
                client_id: "telegram:42".to_owned(),
                frontend: FrontendKind::Telegram,
                mode: AttachmentMode::Owner,
                after_cursor: Some(0),
                expected_revision: None,
                command_id: "attach-1".to_owned(),
                occurred_at: datetime!(2026-07-30 18:00 UTC),
            })
            .expect("attach");
        assert_eq!(attached.replay, session.events);
        assert_eq!(
            attached.next_cursor,
            u64::try_from(session.events.len()).expect("cursor")
        );
        let acknowledged = broker
            .acknowledge(SessionCursorAcknowledgeRequest {
                session_id: session.id.to_string(),
                client_id: "telegram:42".to_owned(),
                expected_revision: attached.continuity.revision,
                cursor: attached.next_cursor,
                command_id: "ack-1".to_owned(),
                occurred_at: datetime!(2026-07-30 18:01 UTC),
            })
            .expect("acknowledge");
        assert_eq!(
            acknowledged.attachments[0].last_acknowledged_cursor,
            attached.next_cursor
        );
    }

    #[test]
    fn owner_handoff_and_detach_are_revision_checked() {
        let repository = tempfile::tempdir().expect("repository");
        let engine = medusa_agent::AgentEngine::new(UnusedProvider, Config::default());
        let session = engine
            .create_session(repository.path(), "Share a live session".to_owned())
            .expect("create session");
        let broker = LiveSessionBroker::new(repository.path().to_path_buf());
        let owner = broker
            .attach(SessionAttachmentRequest {
                session_id: session.id.to_string(),
                client_id: "tui".to_owned(),
                frontend: FrontendKind::Tui,
                mode: AttachmentMode::Owner,
                after_cursor: None,
                expected_revision: None,
                command_id: "attach-tui".to_owned(),
                occurred_at: datetime!(2026-07-30 18:00 UTC),
            })
            .expect("owner");
        let observer = broker
            .attach(SessionAttachmentRequest {
                session_id: session.id.to_string(),
                client_id: "telegram".to_owned(),
                frontend: FrontendKind::Telegram,
                mode: AttachmentMode::ReadOnly,
                after_cursor: None,
                expected_revision: Some(owner.continuity.revision),
                command_id: "attach-telegram".to_owned(),
                occurred_at: datetime!(2026-07-30 18:01 UTC),
            })
            .expect("observer");
        let handed_off = broker
            .handoff(SessionHandoffRequest {
                session_id: session.id.to_string(),
                from_client_id: "tui".to_owned(),
                to_client_id: "telegram".to_owned(),
                expected_revision: observer.continuity.revision,
                command_id: "handoff".to_owned(),
                occurred_at: datetime!(2026-07-30 18:02 UTC),
            })
            .expect("handoff");
        assert_eq!(handed_off.owner_client_id.as_deref(), Some("telegram"));
        let detached = broker
            .detach(SessionDetachRequest {
                session_id: session.id.to_string(),
                client_id: "telegram".to_owned(),
                expected_revision: handed_off.revision,
                command_id: "detach".to_owned(),
                occurred_at: datetime!(2026-07-30 18:03 UTC),
            })
            .expect("detach");
        assert!(detached.owner_client_id.is_none());
    }
}
