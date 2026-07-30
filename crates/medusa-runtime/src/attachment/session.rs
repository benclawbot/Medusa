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
use medusa_session_continuity::{AttachRequest, ContinuityError, ContinuityStore};

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
        let replay = replay_events(&repo, &request.session_id, request.cursor)
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

    /// Starts the production controller only when this client is the current owner.
    pub fn into_controller(self) -> Result<RuntimeController, RuntimeError> {
        self.validate_owner()?;
        RuntimeController::start_resumed(self.repo, &self.session.id.to_string())
    }

    /// Starts the production controller with an explicit configuration for tests and embedders.
    pub fn into_controller_with_config(
        self,
        config: medusa_config::Config,
    ) -> Result<RuntimeController, RuntimeError> {
        self.validate_owner()?;
        RuntimeController::start_resumed_with_config(
            self.repo,
            &self.session.id.to_string(),
            config,
        )
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

fn initialize_continuity(
    store: &ContinuityStore,
    session_id: &str,
) -> Result<(), RuntimeError> {
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
        assert_eq!(attached.continuity.owner_client_id.as_deref(), Some("tui-1"));
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
