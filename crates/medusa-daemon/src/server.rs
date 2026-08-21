// Keep the existing daemon server implementation intact and wrap only the client surface.
// Frontend commands are idempotent by protocol, so a bounded retry can recover a Windows
// read-timeout after the daemon has already accepted and durably created the session.

// Keep the external test module visible to repository reachability checks. The actual test build
// includes `server_base.rs` below, where the original `mod tests;` remains authoritative.
#[cfg(any())]
#[path = "server/tests.rs"]
mod architecture_tests_reference;

#[cfg(test)]
include!("server_base.rs");

#[cfg(not(test))]
mod base {
    include!("server_base.rs");
}

#[cfg(not(test))]
pub use base::{
    ServerHandle, serve, serve_with_config, serve_with_limits, spawn, spawn_with_config,
    spawn_with_limits,
};
#[cfg(not(test))]
pub(crate) use base::{lock_jobs, persist_jobs};

#[cfg(not(test))]
use std::{path::PathBuf, thread, time::Duration};

#[cfg(not(test))]
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
#[cfg(not(test))]
use medusa_protocol::frontend::{FrontendCommand, FrontendCommandEnvelope};

#[cfg(not(test))]
use crate::{
    FrontendArtifactExport,
    frontend_control::FrontendCommandAcknowledgement,
    protocol::{FrontendArtifactUpload, FrontendCredentialUpdate, Request, Response},
};

#[cfg(not(test))]
const FRONTEND_REQUEST_ATTEMPTS: usize = 4;
#[cfg(not(test))]
const FRONTEND_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Local IPC client with bounded recovery for idempotent frontend commands.
#[cfg(not(test))]
#[derive(Clone, Debug)]
pub struct DaemonClient {
    inner: base::DaemonClient,
}

#[cfg(not(test))]
impl DaemonClient {
    #[must_use]
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            inner: base::DaemonClient::new(socket),
        }
    }

    pub fn request(&self, request: Request) -> MedusaResult<Response> {
        let attempts = if frontend_request_is_retryable(&request) {
            FRONTEND_REQUEST_ATTEMPTS
        } else {
            1
        };
        let mut last_error = None;
        for attempt in 0..attempts {
            match self.inner.request(request.clone()) {
                Ok(response) => return Ok(response),
                Err(error)
                    if attempt + 1 < attempts && is_transient_daemon_transport_error(&error) =>
                {
                    last_error = Some(error);
                    thread::sleep(FRONTEND_RETRY_DELAY);
                }
                Err(error) => return Err(normalize_daemon_transport_error(error)),
            }
        }
        Err(normalize_daemon_transport_error(last_error.unwrap_or_else(
            || {
                MedusaError::new(
                    ErrorCode::DependencyUnavailable,
                    ErrorCategory::Environment,
                    "daemon frontend request exhausted its retry budget",
                )
            },
        )))
    }

    pub fn frontend(
        &self,
        envelope: FrontendCommandEnvelope,
    ) -> MedusaResult<FrontendCommandAcknowledgement> {
        match self.request(Request::Frontend { envelope })? {
            Response::Frontend { acknowledgement } => Ok(acknowledgement),
            Response::Error { code, message } => Err(frontend_request_error(code, message)),
            response => Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                format!("daemon returned an unexpected frontend response: {response:?}"),
            )),
        }
    }

    pub fn frontend_artifact(&self, upload: FrontendArtifactUpload) -> MedusaResult<String> {
        match self.request(Request::FrontendArtifact { upload })? {
            Response::FrontendArtifact { artifact_id } => Ok(artifact_id),
            Response::Error { code, message } => Err(frontend_request_error(code, message)),
            response => Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                format!("daemon returned an unexpected artifact response: {response:?}"),
            )),
        }
    }

    pub fn frontend_artifact_export(
        &self,
        artifact_id: &str,
    ) -> MedusaResult<FrontendArtifactExport> {
        match self.request(Request::FrontendArtifactExport {
            artifact_id: artifact_id.to_owned(),
        })? {
            Response::FrontendArtifactExport { artifact } => Ok(artifact),
            Response::Error { code, message } => Err(frontend_request_error(code, message)),
            response => Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                format!("daemon returned an unexpected artifact export response: {response:?}"),
            )),
        }
    }

    pub fn frontend_credential(&self, update: FrontendCredentialUpdate) -> MedusaResult<()> {
        match self.request(Request::FrontendCredential { update })? {
            Response::Ack => Ok(()),
            Response::Error { code, message } => Err(frontend_request_error(code, message)),
            response => Err(MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                format!("daemon returned an unexpected credential response: {response:?}"),
            )),
        }
    }
}

#[cfg(not(test))]
fn frontend_request_is_retryable(request: &Request) -> bool {
    let Request::Frontend { envelope } = request else {
        return false;
    };
    !matches!(
        &envelope.command,
        FrontendCommand::ListSessions
            | FrontendCommand::Replay { .. }
            | FrontendCommand::PollTransient
            | FrontendCommand::ShowSessionActions
            | FrontendCommand::ShowStatus
    )
}

#[cfg(not(test))]
fn is_transient_daemon_transport_error(error: &MedusaError) -> bool {
    error.category == ErrorCategory::Environment
        && matches!(
            error.code,
            ErrorCode::DependencyUnavailable | ErrorCode::PersistenceFailed
        )
}

#[cfg(not(test))]
fn normalize_daemon_transport_error(mut error: MedusaError) -> MedusaError {
    if error.category == ErrorCategory::Environment && error.code == ErrorCode::PersistenceFailed {
        error.code = ErrorCode::DependencyUnavailable;
        error.message = format!("daemon transport error: {}", error.message);
    }
    if error.category == ErrorCategory::Environment
        && error.code == ErrorCode::DependencyUnavailable
    {
        error.retryable = true;
    }
    error
}

#[cfg(not(test))]
fn frontend_request_error(code: String, message: String) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Environment,
        format!("daemon frontend request failed ({code}): {message}"),
    )
}
