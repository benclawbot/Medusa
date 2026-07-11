//! Core identifiers and structured errors shared by Medusa crates.

use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ulid::Ulid;

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
    #[error("incompatible protocol version")]
    IncompatibleProtocol,
    #[error("invalid event")]
    InvalidEvent,
    #[error("checksum mismatch")]
    ChecksumMismatch,
    #[error("policy denied")]
    PolicyDenied,
    #[error("dependency unavailable")]
    DependencyUnavailable,
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
        assert_eq!(serde_json::from_str::<MedusaError>(&encoded).expect("deserialize"), original);
    }
}
