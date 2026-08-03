use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use ulid::Ulid;

use crate::{ContractError, Result};

pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct AttemptId(String);

impl AttemptId {
    #[must_use]
    pub fn new() -> Self {
        Self(Ulid::new().to_string())
    }

    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        Ulid::from_string(&value).map_err(|_| {
            ContractError::Validation("attempt ID must be a canonical ULID".to_owned())
        })?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for AttemptId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for AttemptId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RequestDigest(String);

impl RequestDigest {
    pub fn from_canonical<T: Serialize>(value: &T) -> Result<Self> {
        let canonical = canonical_json(value)?;
        Ok(Self(hex::encode(Sha256::digest(canonical.as_bytes()))))
    }

    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ContractError::Validation(
                "request digest must be a 64-character hexadecimal SHA-256 value".to_owned(),
            ));
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_IDEMPOTENCY_KEY_BYTES {
            return Err(ContractError::Validation(format!(
                "idempotency key must contain 1..={MAX_IDEMPOTENCY_KEY_BYTES} bytes"
            )));
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
        {
            return Err(ContractError::Validation(
                "idempotency key contains unsupported characters".to_owned(),
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn fingerprint(&self) -> String {
        hex::encode(Sha256::digest(self.0.as_bytes()))
    }
}

impl fmt::Display for IdempotencyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrustedHost {
    scheme: String,
    host: String,
    port: Option<u16>,
}

impl TrustedHost {
    pub fn parse(origin: &str) -> Result<Self> {
        let (scheme, remainder) = origin.split_once("://").ok_or_else(|| {
            ContractError::Validation("trusted host must be an absolute origin".to_owned())
        })?;
        if !matches!(scheme, "https" | "http") {
            return Err(ContractError::Validation(
                "trusted host scheme must be http or https".to_owned(),
            ));
        }
        let authority = remainder.split('/').next().unwrap_or_default();
        if authority.is_empty() || authority.contains('@') {
            return Err(ContractError::Validation(
                "trusted host authority is invalid".to_owned(),
            ));
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if port.bytes().all(|byte| byte.is_ascii_digit()) => (
                host.to_ascii_lowercase(),
                Some(u16::from_str(port).map_err(|_| {
                    ContractError::Validation("trusted host port is invalid".to_owned())
                })?),
            ),
            _ => (authority.to_ascii_lowercase(), None),
        };
        if host.is_empty() {
            return Err(ContractError::Validation(
                "trusted host name cannot be empty".to_owned(),
            ));
        }
        Ok(Self {
            scheme: scheme.to_owned(),
            host,
            port,
        })
    }

    pub fn permits(&self, url: &str) -> Result<bool> {
        Ok(self == &Self::parse(url)?)
    }
}

fn canonical_json<T: Serialize>(value: &T) -> Result<String> {
    let value = canonicalize(serde_json::to_value(value)?);
    Ok(serde_json::to_string(&value)?)
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, canonicalize(value)))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn identities_are_unique_and_digests_are_canonical() {
        let first = AttemptId::new();
        let second = AttemptId::new();
        assert_ne!(first, second);
        assert_eq!(AttemptId::parse(first.to_string()).unwrap(), first);
        let one = RequestDigest::from_canonical(&json!({"b": 2, "a": 1})).unwrap();
        let two = RequestDigest::from_canonical(&json!({"a": 1, "b": 2})).unwrap();
        assert_eq!(one, two);
    }

    #[test]
    fn keys_and_hosts_fail_closed() {
        assert!(IdempotencyKey::parse("release:repo:create-1").is_ok());
        assert!(IdempotencyKey::parse("contains space").is_err());
        let github = TrustedHost::parse("https://api.github.com").unwrap();
        assert!(github.permits("https://api.github.com/repos/x/y").unwrap());
        assert!(!github.permits("https://evil.example/repos/x/y").unwrap());
        assert!(TrustedHost::parse("https://token@api.github.com").is_err());
    }
}
