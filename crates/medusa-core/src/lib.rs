//! Core identifiers and structured errors shared by Medusa crates.

use std::{collections::BTreeMap, fmt};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ulid::Ulid;

pub mod learning_policy;
pub mod repository_mutation;
pub mod storage;

/// Creates a child-process command that never opens a visible console window on Windows.
///
/// Medusa is primarily a GUI/daemon application. Every subprocess launched for repository
/// inspection, verification, or background tooling must use this constructor so Windows does
/// not create a new `cmd.exe` window for each short-lived command.
pub fn hidden_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let program = program.as_ref();
    #[cfg(windows)]
    let mut command = std::process::Command::new(program);
    #[cfg(not(windows))]
    let command = std::process::Command::new(program);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    command
}

macro_rules! typed_id {
    ($name:ident, $prefix:literal) => {
        #[doc = concat!("Stable ", stringify!($name), " identifier.")]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates a new sortable identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(format!(concat!($prefix, "-{}"), Ulid::new()))
            }

            /// Parses and validates a prefixed ULID.
            pub fn parse(value: impl Into<String>) -> Result<Self, &'static str> {
                let value = value.into();
                let Some(raw) = value.strip_prefix(concat!($prefix, "-")) else {
                    return Err("identifier has the wrong prefix");
                };
                Ulid::from_string(raw).map_err(|_| "identifier contains an invalid ULID")?;
                Ok(Self(value))
            }

            /// Returns the identifier as text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

typed_id!(SessionId, "ses");
typed_id!(EventId, "evt");
typed_id!(CorrelationId, "cor");

/// Stable error category.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Validation,
    Policy,
    Environment,
    Execution,
    Transient,
    Persistence,
    Internal,
}

/// Stable machine-readable error code.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Error, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    #[error("invalid configuration")]
    InvalidConfiguration,
    #[error("invalid input")]
    InvalidInput,
    #[error("incompatible protocol version")]
    IncompatibleProtocol,
    #[error("invalid event")]
    InvalidEvent,
    #[error("checksum mismatch")]
    ChecksumMismatch,
    #[error("policy denied")]
    PolicyDenied,
    #[error("sandbox unavailable")]
    SandboxUnavailable,
    #[error("dependency unavailable")]
    DependencyUnavailable,
    #[error("tool execution failed")]
    ToolExecutionFailed,
    #[error("persistence failed")]
    PersistenceFailed,
    #[error("internal invariant failed")]
    InternalInvariant,
}

/// Structured transport-safe error.
#[derive(Clone, Debug, Deserialize, Eq, Error, PartialEq, Serialize)]
#[error("{code}: {message}")]
pub struct MedusaError {
    pub code: ErrorCode,
    pub message: String,
    pub category: ErrorCategory,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<String>,
}

impl MedusaError {
    /// Constructs a structured error.
    #[must_use]
    pub fn new(code: ErrorCode, category: ErrorCategory, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            category,
            retryable: false,
            context: BTreeMap::new(),
            artifact_refs: Vec::new(),
        }
    }

    /// Marks whether retrying materially identical input may succeed.
    #[must_use]
    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}

/// I/O kinds that indicate a transient failure: retrying materially
/// identical input may succeed.
fn is_transient_io_kind(kind: std::io::ErrorKind) -> bool {
    use std::io::ErrorKind as Kind;
    matches!(
        kind,
        Kind::TimedOut
            | Kind::Interrupted
            | Kind::ConnectionReset
            | Kind::ConnectionAborted
            | Kind::WouldBlock
    )
}

impl From<std::io::Error> for MedusaError {
    fn from(error: std::io::Error) -> Self {
        if is_transient_io_kind(error.kind()) {
            return Self::new(
                ErrorCode::PersistenceFailed,
                ErrorCategory::Transient,
                error.to_string(),
            )
            .with_retryable(true);
        }
        Self::new(
            ErrorCode::PersistenceFailed,
            ErrorCategory::Environment,
            error.to_string(),
        )
    }
}

impl From<serde_json::Error> for MedusaError {
    fn from(error: serde_json::Error) -> Self {
        // An I/O failure underneath the JSON layer is transient and
        // retryable. A corrupt frame (syntax/data error) means the bytes
        // violate the expected schema: it is a validation failure, and
        // retrying the same bytes cannot succeed, so it stays non-retryable.
        if error.is_io() {
            return Self::new(
                ErrorCode::PersistenceFailed,
                ErrorCategory::Transient,
                error.to_string(),
            )
            .with_retryable(true);
        }
        Self::new(
            ErrorCode::InvalidInput,
            ErrorCategory::Validation,
            error.to_string(),
        )
    }
}

/// Result alias for Medusa operations.
pub type MedusaResult<T> = Result<T, MedusaError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_round_trip() {
        let id = SessionId::new();
        assert_eq!(SessionId::parse(id.to_string()).expect("generated ID"), id);
    }

    #[test]
    fn wrong_prefix_is_rejected() {
        assert_eq!(
            SessionId::parse("evt-01ARZ3NDEKTSV4RRFFQ69G5FAV"),
            Err("identifier has the wrong prefix")
        );
    }

    #[test]
    fn structured_error_round_trips() {
        let original = MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Transient,
            "provider unavailable",
        )
        .with_retryable(true);
        let encoded = serde_json::to_string(&original).expect("serialize");
        assert_eq!(
            serde_json::from_str::<MedusaError>(&encoded).expect("deserialize"),
            original
        );
    }

    #[test]
    fn transient_io_errors_are_retryable() {
        for kind in [
            std::io::ErrorKind::TimedOut,
            std::io::ErrorKind::Interrupted,
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::ConnectionAborted,
            std::io::ErrorKind::WouldBlock,
        ] {
            let error = MedusaError::from(std::io::Error::new(kind, "transient"));
            assert_eq!(error.category, ErrorCategory::Transient, "{kind:?}");
            assert!(error.retryable, "{kind:?}");
        }
    }

    #[test]
    fn persistent_io_errors_stay_non_retryable() {
        let error = MedusaError::from(std::io::Error::new(std::io::ErrorKind::NotFound, "gone"));
        assert_eq!(error.category, ErrorCategory::Environment);
        assert!(!error.retryable);
    }

    #[test]
    fn corrupt_frames_are_deterministic_not_transient() {
        let syntax: serde_json::Error =
            serde_json::from_str::<serde_json::Value>("{truncated").expect_err("must fail");
        assert!(!syntax.is_io());
        let error = MedusaError::from(syntax);
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert_eq!(error.category, ErrorCategory::Validation);
        assert!(!error.retryable);
    }
}
