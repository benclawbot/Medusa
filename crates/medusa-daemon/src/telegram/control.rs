//! Telegram control backend over daemon IPC, with an embedded backend available only to unit tests.

use base64::{Engine as _, engine::general_purpose::STANDARD};

use crate::{
    DaemonClient, FrontendArtifactExport, FrontendArtifactKind, FrontendArtifactUpload,
    FrontendCommandAcknowledgement, FrontendControlResult, LiveSessionReplayView,
};
#[cfg(test)]
use crate::{FrontendControlError, FrontendControlPlane};
use medusa_protocol::frontend::{
    FRONTEND_PROTOCOL_VERSION, FrontendCommand, FrontendCommandEnvelope, FrontendKind,
};
use time::OffsetDateTime;

/// Shared Telegram command/artifact backend without frontend-owned runtime authority.
pub enum TelegramControl {
    #[cfg(test)]
    InProcess(Box<FrontendControlPlane>),
    Daemon(DaemonClient),
}

#[cfg(test)]
impl From<FrontendControlPlane> for TelegramControl {
    fn from(control: FrontendControlPlane) -> Self {
        Self::InProcess(Box::new(control))
    }
}

impl From<DaemonClient> for TelegramControl {
    fn from(client: DaemonClient) -> Self {
        Self::Daemon(client)
    }
}

impl TelegramControl {
    pub fn dispatch(
        &mut self,
        envelope: FrontendCommandEnvelope,
    ) -> Result<FrontendCommandAcknowledgement, TelegramControlError> {
        match self {
            #[cfg(test)]
            Self::InProcess(control) => control.dispatch(envelope).map_err(Into::into),
            Self::Daemon(client) => client
                .frontend(envelope)
                .map_err(|error| TelegramControlError::Daemon(error.to_string())),
        }
    }

    pub fn replay_events(
        &mut self,
        client_id: &str,
        session_id: &str,
        cursor: u64,
        timestamp: OffsetDateTime,
    ) -> Result<LiveSessionReplayView, TelegramControlError> {
        match self {
            #[cfg(test)]
            Self::InProcess(control) => {
                control.replay_events(client_id, cursor).map_err(Into::into)
            }
            Self::Daemon(client) => {
                let identity = format!(
                    "telegram-replay:{}:{}:{}",
                    client_id,
                    cursor,
                    timestamp.unix_timestamp_nanos()
                );
                let acknowledgement = client
                    .frontend(FrontendCommandEnvelope {
                        protocol_version: FRONTEND_PROTOCOL_VERSION,
                        command_id: identity.clone(),
                        idempotency_key: identity,
                        frontend: FrontendKind::Telegram,
                        client_id: client_id.to_owned(),
                        session_id: Some(session_id.to_owned()),
                        turn_id: None,
                        timestamp,
                        command: FrontendCommand::Replay {
                            after_cursor: cursor,
                        },
                    })
                    .map_err(|error| TelegramControlError::Daemon(error.to_string()))?;
                match acknowledgement.result {
                    FrontendControlResult::Events { replay } => Ok(replay),
                    _ => Err(TelegramControlError::Daemon(
                        "daemon returned an unexpected Telegram replay result".to_owned(),
                    )),
                }
            }
        }
    }

    pub fn ingest_attachment(
        &self,
        display_name: String,
        mime_type: Option<String>,
        bytes: Vec<u8>,
    ) -> Result<String, TelegramControlError> {
        let kind = artifact_kind(mime_type.as_deref(), &bytes);
        match self {
            #[cfg(test)]
            Self::InProcess(control) => control
                .ingest_artifact(display_name, mime_type, kind, bytes)
                .map_err(Into::into),
            Self::Daemon(client) => client
                .frontend_artifact(FrontendArtifactUpload {
                    display_name,
                    mime_type,
                    kind,
                    bytes_base64: STANDARD.encode(bytes),
                })
                .map_err(|error| TelegramControlError::Daemon(error.to_string())),
        }
    }

    pub fn export_attachment(
        &self,
        artifact_id: &str,
    ) -> Result<FrontendArtifactExport, TelegramControlError> {
        match self {
            #[cfg(test)]
            Self::InProcess(control) => control.export_attachment(artifact_id).map_err(Into::into),
            Self::Daemon(client) => client
                .frontend_artifact_export(artifact_id)
                .map_err(|error| TelegramControlError::Daemon(error.to_string())),
        }
    }
}

fn artifact_kind(mime_type: Option<&str>, bytes: &[u8]) -> FrontendArtifactKind {
    match mime_type {
        Some(value) if value.starts_with("image/") => FrontendArtifactKind::Image,
        Some(value) if value.starts_with("text/") => FrontendArtifactKind::Text,
        _ if std::str::from_utf8(bytes).is_ok() => FrontendArtifactKind::Text,
        _ => FrontendArtifactKind::File,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TelegramControlError {
    #[cfg(test)]
    #[error(transparent)]
    InProcess(#[from] FrontendControlError),
    #[error("daemon Telegram control failed: {0}")]
    Daemon(String),
}

#[cfg(test)]
mod tests {
    use std::{
        thread,
        time::{Duration, Instant},
    };

    use medusa_config::Config;

    use super::*;
    use crate::{DaemonPaths, Request, Response, spawn_with_config};

    #[test]
    fn daemon_backend_round_trips_bounded_artifacts() {
        let repository = tempfile::tempdir().expect("repository");
        let paths = DaemonPaths::for_repo(repository.path());
        let (handle, server) =
            spawn_with_config(paths.clone(), Config::default()).expect("spawn daemon");
        let client = DaemonClient::new(&paths.socket);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match client.request(Request::Ping) {
                Ok(Response::Pong) => break,
                Ok(response) => panic!("unexpected daemon readiness response: {response:?}"),
                Err(_) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("daemon did not become ready: {error}"),
            }
        }
        let control = TelegramControl::from(client);
        let artifact_id = control
            .ingest_attachment(
                "evidence.txt".to_owned(),
                Some("text/plain".to_owned()),
                b"verified evidence".to_vec(),
            )
            .expect("ingest through daemon");
        let artifact = control
            .export_attachment(&artifact_id)
            .expect("export through daemon");
        assert_eq!(artifact.display_name, "evidence.txt");
        assert_eq!(artifact.bytes, b"verified evidence");
        handle.shutdown();
        server.join().expect("join daemon").expect("daemon result");
    }

    #[test]
    fn binary_documents_are_not_misclassified_as_text() {
        assert_eq!(
            artifact_kind(Some("application/octet-stream"), &[0xff, 0x00]),
            FrontendArtifactKind::File
        );
    }
}
