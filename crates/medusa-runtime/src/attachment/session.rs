//! Durable multi-client attachment and replay over the canonical session journal.
//!
//! The continuity store owns only client attachment metadata and ownership. The canonical
//! `AgentSession` snapshot and execution event history always come from the journal.

use std::{
    io,
    path::{Path, PathBuf},
};

use medusa_agent::{
    AgentSession,
    session_browser::{load_session, replay_events},
};
use medusa_protocol::EventEnvelope;
use medusa_session_continuity::{
    AttachRequest, ContinuityError, ContinuityStore, CursorAckRequest, DetachRequest,
    HandoffRequest,
};

pub use medusa_session_continuity::{AttachmentMode, ClientKind, ContinuitySession};

use crate::{RuntimeController, RuntimeError};

/// Idempotent request to attach one frontend client to a durable runtime session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeAttachRequest {
    pub session_id: String,
    pub client_id: String,
    pub client_kind: ClientKind,
    pub requested_mode: AttachmentMode,
    pub expected_revision: u64,
    pub cursor: u64,
    pub occurred_at_unix_ms: i64,
    pub event_id: String,
}

/// A client attachment bound to one authoritative journal-backed session.
pub struct RuntimeSessionAttachment {
    repo: PathBuf,
    client_id: String,
    mode: AttachmentMode,
    pub session: AgentSession,
    pub continuity: ContinuitySession,
    pub replay: Vec<EventEnvelope>,
}

impl RuntimeSessionAttachment {
    /// Attaches a client, loads the latest committed session snapshot, and replays from `cursor`.
    pub fn attach(repo: PathBuf, request: RuntimeAttachRequest) -> Result<Self, RuntimeError> {
        validate_request(&request)?;
        let session = load_session(&repo, &request.session_id).map_err(RuntimeError::agent)?;
        let store = continuity_store(&repo, &request.session_id);
        initialize_continuity(&store, &request.session_id)?;
        let outcome = store
            .attach(AttachRequest {
                client_id: request.client_id.clone(),
                client_kind: request.client_kind,
                requested_mode: request.requested_mode,
                expected_revision: request.expected_revision,
                journal_cursor: request.cursor,
                occurred_at_unix_ms: request.occurred_at_unix_ms,
                event_id: request.event_id,
            })
            .map_err(RuntimeError::agent)?;
        let continuity = outcome.session().clone();
        validate_continuity_identity(&continuity, &request.session_id)?;
        let attachment = continuity
            .attachments
            .iter()
            .find(|attachment| attachment.client_id == request.client_id)
            .ok_or_else(|| RuntimeError::agent("continuity attach did not retain the client"))?;
        let replay_cursor = request.cursor.max(attachment.journal_cursor);
        let replay = replay_events(&repo, &request.session_id, replay_cursor)
            .map_err(RuntimeError::agent)?;
        Ok(Self {
            repo,
            client_id: request.client_id,
            mode: attachment.mode,
            session,
            continuity,
            replay,
        })
    }

    /// Returns new committed execution events from a zero-based journal cursor.
    pub fn replay_from(&self, cursor: u64) -> Result<Vec<EventEnvelope>, RuntimeError> {
        replay_events(&self.repo, &self.session.id.to_string(), cursor).map_err(RuntimeError::agent)
    }

    /// Acknowledges the highest canonical journal cursor observed by this client.
    pub fn acknowledge_cursor(
        &mut self,
        cursor: u64,
        occurred_at_unix_ms: i64,
        event_id: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        let client_id = self.client_id.clone();
        let event_id = event_id.into();
        let store = continuity_store(&self.repo, &self.session.id.to_string());
        let request = |expected_revision| CursorAckRequest {
            client_id: client_id.clone(),
            expected_revision,
            cursor,
            occurred_at_unix_ms,
            event_id: event_id.clone(),
        };
        let outcome = match store.acknowledge_cursor(request(self.continuity.revision)) {
            Ok(outcome) => outcome,
            Err(ContinuityError::StaleRevision { expected, actual })
                if expected.checked_add(1) == Some(actual) =>
            {
                self.refresh_continuity()?;
                store
                    .acknowledge_cursor(request(self.continuity.revision))
                    .map_err(RuntimeError::agent)?
            }
            Err(error) => return Err(RuntimeError::agent(error)),
        };
        self.continuity = outcome.session().clone();
        Ok(())
    }

    /// Hands mutable ownership to an already attached client.
    pub fn handoff(
        &mut self,
        to_client_id: impl Into<String>,
        occurred_at_unix_ms: i64,
        event_id: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        self.validate_owner()?;
        let outcome = continuity_store(&self.repo, &self.session.id.to_string())
            .handoff(HandoffRequest {
                from_client_id: self.client_id.clone(),
                to_client_id: to_client_id.into(),
                expected_revision: self.continuity.revision,
                occurred_at_unix_ms,
                event_id: event_id.into(),
            })
            .map_err(RuntimeError::agent)?;
        self.continuity = outcome.session().clone();
        self.mode = self
            .continuity
            .attachments
            .iter()
            .find(|attachment| attachment.client_id == self.client_id)
            .map_or(AttachmentMode::ReadOnly, |attachment| attachment.mode);
        Ok(())
    }

    /// Detaches this client. Detaching the owner leaves the session ownerless until an explicit
    /// owner attachment or handoff occurs.
    pub fn detach(
        self,
        occurred_at_unix_ms: i64,
        event_id: impl Into<String>,
    ) -> Result<ContinuitySession, RuntimeError> {
        let outcome = continuity_store(&self.repo, &self.session.id.to_string())
            .detach(DetachRequest {
                client_id: self.client_id,
                expected_revision: self.continuity.revision,
                occurred_at_unix_ms,
                event_id: event_id.into(),
            })
            .map_err(RuntimeError::agent)?;
        Ok(outcome.session().clone())
    }

    /// Reloads durable continuity metadata after another client changes ownership or cursor state.
    pub fn refresh_continuity(&mut self) -> Result<(), RuntimeError> {
        let continuity = continuity_store(&self.repo, &self.session.id.to_string())
            .load()
            .map_err(RuntimeError::agent)?;
        validate_continuity_identity(&continuity, &self.session.id.to_string())?;
        let mode = continuity
            .attachments
            .iter()
            .find(|attachment| attachment.client_id == self.client_id)
            .map(|attachment| attachment.mode)
            .ok_or_else(|| RuntimeError::agent("client is no longer attached"))?;
        self.mode = mode;
        self.continuity = continuity;
        Ok(())
    }

    /// Starts the production controller only when this client is the current owner.
    pub fn into_controller(self) -> Result<RuntimeController, RuntimeError> {
        self.validate_owner()?;
        let controller = RuntimeController::start_resumed(self.repo, &self.session.id.to_string())?;
        controller.recover_session_actions()?;
        Ok(controller)
    }

    /// Starts the production controller with an explicit configuration for tests and embedders.
    pub fn into_controller_with_config(
        self,
        config: medusa_config::Config,
    ) -> Result<RuntimeController, RuntimeError> {
        self.validate_owner()?;
        let controller = RuntimeController::start_resumed_with_config(
            self.repo,
            &self.session.id.to_string(),
            config,
        )?;
        controller.recover_session_actions()?;
        Ok(controller)
    }

    #[must_use]
    pub fn mode(&self) -> AttachmentMode {
        self.mode
    }

    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    fn validate_owner(&self) -> Result<(), RuntimeError> {
        if self.mode != AttachmentMode::Owner
            || self.continuity.owner_client_id.as_deref() != Some(self.client_id.as_str())
        {
            return Err(RuntimeError::InvalidCommand(format!(
                "client {} is attached read-only and cannot start the runtime controller",
                self.client_id
            )));
        }
        Ok(())
    }
}

fn validate_request(request: &RuntimeAttachRequest) -> Result<(), RuntimeError> {
    for (name, value) in [
        ("session_id", request.session_id.as_str()),
        ("client_id", request.client_id.as_str()),
        ("event_id", request.event_id.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(RuntimeError::InvalidCommand(format!(
                "runtime attachment {name} cannot be empty"
            )));
        }
    }
    Ok(())
}

fn continuity_store(repo: &Path, session_id: &str) -> ContinuityStore {
    ContinuityStore::new(
        repo.join(".medusa/continuity")
            .join(format!("{session_id}.json")),
    )
}

fn initialize_continuity(store: &ContinuityStore, session_id: &str) -> Result<(), RuntimeError> {
    match store.load() {
        Ok(session) => validate_continuity_identity(&session, session_id),
        Err(ContinuityError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            store.create(session_id).map_err(RuntimeError::agent)?;
            Ok(())
        }
        Err(error) => Err(RuntimeError::agent(error)),
    }
}

fn validate_continuity_identity(
    continuity: &ContinuitySession,
    session_id: &str,
) -> Result<(), RuntimeError> {
    if continuity.session_id != session_id {
        return Err(RuntimeError::agent(format!(
            "continuity state belongs to session {}, not {session_id}",
            continuity.session_id
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use medusa_config::Config;
    use medusa_core::MedusaResult;
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};

    use super::*;
    use medusa_agent::AgentEngine;

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
        requested_mode: AttachmentMode,
        expected_revision: u64,
        cursor: u64,
        event_id: &str,
    ) -> RuntimeAttachRequest {
        RuntimeAttachRequest {
            session_id: session_id.to_owned(),
            client_id: client_id.to_owned(),
            client_kind,
            requested_mode,
            expected_revision,
            cursor,
            occurred_at_unix_ms: 1_000 + i64::try_from(expected_revision).unwrap_or(i64::MAX),
            event_id: event_id.to_owned(),
        }
    }

    #[test]
    fn owner_attachment_starts_controller_and_replays_committed_journal() {
        let repository = tempfile::tempdir().expect("repository");
        let engine = AgentEngine::new(UnusedProvider, Config::default());
        let session = engine
            .create_session(repository.path(), "Attach frontend".to_owned())
            .expect("session");
        let snapshot = repository
            .path()
            .join(".medusa/sessions")
            .join(format!("{}.json", session.id));
        std::fs::remove_file(&snapshot).expect("remove compatibility snapshot");

        let attached = RuntimeSessionAttachment::attach(
            repository.path().to_path_buf(),
            request(
                &session.id.to_string(),
                "tui-1",
                ClientKind::Tui,
                AttachmentMode::Owner,
                0,
                0,
                "attach-tui",
            ),
        )
        .expect("attach owner");

        assert_eq!(attached.session.id, session.id);
        assert_eq!(attached.session.objective, session.objective);
        assert_eq!(attached.replay, session.events);
        assert_eq!(attached.mode(), AttachmentMode::Owner);
        assert_eq!(
            attached.continuity.owner_client_id.as_deref(),
            Some("tui-1")
        );
        assert!(snapshot.is_file());
        let controller = attached
            .into_controller_with_config(Config::default())
            .expect("owner controller");
        drop(controller);
    }

    #[test]
    fn read_only_attachment_can_replay_but_cannot_start_controller() {
        let repository = tempfile::tempdir().expect("repository");
        let engine = AgentEngine::new(UnusedProvider, Config::default());
        let session = engine
            .create_session(repository.path(), "Observe frontend".to_owned())
            .expect("session");
        let session_id = session.id.to_string();
        let owner = RuntimeSessionAttachment::attach(
            repository.path().to_path_buf(),
            request(
                &session_id,
                "desktop-owner",
                ClientKind::Desktop,
                AttachmentMode::Owner,
                0,
                0,
                "attach-owner",
            ),
        )
        .expect("owner");
        let observer = RuntimeSessionAttachment::attach(
            repository.path().to_path_buf(),
            request(
                &session_id,
                "tui-observer",
                ClientKind::Tui,
                AttachmentMode::ReadOnly,
                owner.continuity.revision,
                0,
                "attach-observer",
            ),
        )
        .expect("observer");

        assert_eq!(observer.mode(), AttachmentMode::ReadOnly);
        assert_eq!(observer.replay_from(0).expect("replay"), session.events);
        let error = observer
            .into_controller_with_config(Config::default())
            .err()
            .expect("read-only controller must fail");
        assert!(error.to_string().contains("read-only"));
    }
}

#[cfg(test)]
mod continuity_command_tests {
    use medusa_agent::AgentEngine;
    use medusa_config::Config;
    use medusa_core::MedusaResult;
    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};

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
        requested_mode: AttachmentMode,
        expected_revision: u64,
        cursor: u64,
        event_id: &str,
    ) -> RuntimeAttachRequest {
        RuntimeAttachRequest {
            session_id: session_id.to_owned(),
            client_id: client_id.to_owned(),
            client_kind,
            requested_mode,
            expected_revision,
            cursor,
            occurred_at_unix_ms: 10_000 + i64::try_from(expected_revision).unwrap_or(i64::MAX),
            event_id: event_id.to_owned(),
        }
    }

    #[test]
    fn telegram_cursor_handoff_and_detach_are_durable() {
        let repository = tempfile::tempdir().expect("repository");
        let session = AgentEngine::new(UnusedProvider, Config::default())
            .create_session(repository.path(), "Share one transcript".to_owned())
            .expect("session");
        let session_id = session.id.to_string();
        let mut owner = RuntimeSessionAttachment::attach(
            repository.path().to_path_buf(),
            request(
                &session_id,
                "tui-owner",
                ClientKind::Tui,
                AttachmentMode::Owner,
                0,
                0,
                "attach-owner",
            ),
        )
        .expect("owner");
        let mut telegram = RuntimeSessionAttachment::attach(
            repository.path().to_path_buf(),
            request(
                &session_id,
                "telegram-42",
                ClientKind::Telegram,
                AttachmentMode::ReadOnly,
                owner.continuity.revision,
                1,
                "attach-telegram",
            ),
        )
        .expect("telegram");
        assert!(telegram.replay.is_empty());
        telegram
            .acknowledge_cursor(1, 10_002, "ack-telegram-1")
            .expect("cursor ack");
        let store = continuity_store(repository.path(), &session_id);
        let after_ack = store.load().expect("continuity");
        assert_eq!(
            after_ack
                .attachments
                .iter()
                .find(|attachment| attachment.client_id == "telegram-42")
                .expect("telegram attachment")
                .journal_cursor,
            1
        );

        owner.continuity = after_ack;
        owner
            .handoff("telegram-42", 10_003, "handoff-telegram")
            .expect("handoff");
        assert_eq!(owner.mode(), AttachmentMode::ReadOnly);
        let telegram_state = store.load().expect("continuity");
        telegram.continuity = telegram_state;
        telegram.mode = AttachmentMode::Owner;
        let detached = telegram.detach(10_004, "detach-telegram").expect("detach");
        assert_eq!(detached.owner_client_id, None);
        assert!(
            detached
                .attachments
                .iter()
                .all(|attachment| attachment.client_id != "telegram-42")
        );
    }

    #[test]
    fn cursor_acknowledgement_refreshes_one_revision_frontend_race() {
        let repository = tempfile::tempdir().expect("repository");
        let session = AgentEngine::new(UnusedProvider, Config::default())
            .create_session(repository.path(), "Cursor race".to_owned())
            .expect("session");
        let session_id = session.id.to_string();
        let mut owner = RuntimeSessionAttachment::attach(
            repository.path().to_path_buf(),
            request(
                &session_id,
                "tui-owner",
                ClientKind::Tui,
                AttachmentMode::Owner,
                0,
                0,
                "attach-owner",
            ),
        )
        .expect("owner");
        let stale_revision = owner.continuity.revision;
        let _observer = RuntimeSessionAttachment::attach(
            repository.path().to_path_buf(),
            request(
                &session_id,
                "desktop-observer",
                ClientKind::Desktop,
                AttachmentMode::ReadOnly,
                stale_revision,
                0,
                "attach-observer",
            ),
        )
        .expect("observer");
        let authoritative_before_ack = continuity_store(repository.path(), &session_id)
            .load()
            .expect("continuity");
        assert_eq!(authoritative_before_ack.revision, stale_revision + 1);

        owner
            .acknowledge_cursor(1, 19_000, "ack-after-observer")
            .expect("stale cursor acknowledgement should refresh once");

        assert_eq!(owner.mode(), AttachmentMode::Owner);
        assert_eq!(
            owner
                .continuity
                .attachments
                .iter()
                .find(|attachment| attachment.client_id == "tui-owner")
                .expect("owner attachment")
                .journal_cursor,
            1
        );
        assert_eq!(owner.continuity.revision, stale_revision + 2);
    }

    #[test]
    fn cursor_acknowledgement_is_monotonic_and_idempotent() {
        let repository = tempfile::tempdir().expect("repository");
        let session = AgentEngine::new(UnusedProvider, Config::default())
            .create_session(repository.path(), "Cursor semantics".to_owned())
            .expect("session");
        let mut attachment = RuntimeSessionAttachment::attach(
            repository.path().to_path_buf(),
            request(
                &session.id.to_string(),
                "daemon-subscriber",
                ClientKind::Daemon,
                AttachmentMode::ReadOnly,
                0,
                0,
                "attach-daemon",
            ),
        )
        .expect("attach");
        attachment
            .acknowledge_cursor(1, 20_000, "ack-daemon")
            .expect("ack");
        let revision = attachment.continuity.revision;
        let store = continuity_store(repository.path(), &session.id.to_string());
        let replay = store
            .acknowledge_cursor(CursorAckRequest {
                client_id: "daemon-subscriber".to_owned(),
                expected_revision: 0,
                cursor: 1,
                occurred_at_unix_ms: 20_000,
                event_id: "ack-daemon".to_owned(),
            })
            .expect("idempotent replay");
        assert_eq!(replay.session().revision, revision);
        let error = store
            .acknowledge_cursor(CursorAckRequest {
                client_id: "daemon-subscriber".to_owned(),
                expected_revision: revision,
                cursor: 0,
                occurred_at_unix_ms: 20_001,
                event_id: "ack-regression".to_owned(),
            })
            .expect_err("cursor regression");
        assert!(error.to_string().contains("regressed"));
    }
}
