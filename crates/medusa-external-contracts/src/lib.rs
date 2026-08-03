//! Versioned provider, authentication, cancellation, and external-operation contracts.
//!
//! This crate is intentionally transport-neutral. Provider adapters, OAuth lifecycles,
//! GitHub backends, frontends, and durable runtime state consume these types instead of
//! inventing readiness, capability, or mutation state independently.

use std::{collections::BTreeMap, fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use ulid::Ulid;

pub const SCHEMA_VERSION: u16 = 1;
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;

pub type Result<T> = std::result::Result<T, ContractError>;

#[derive(Debug, thiserror::Error)]
pub enum ContractError {
    #[error("external contract validation failed: {0}")]
    Validation(String),
    #[error("external contract serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

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

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessStage {
    ProfileSaved,
    SecretPresent,
    EndpointReachable,
    Authenticated,
    CapabilityAvailable,
    LiveRequestVerified,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReadinessCheck {
    pub stage: ReadinessStage,
    pub ready: bool,
    pub checked_at: OffsetDateTime,
    pub reason: Option<String>,
}

impl ReadinessCheck {
    #[must_use]
    pub fn ready(stage: ReadinessStage) -> Self {
        Self {
            stage,
            ready: true,
            checked_at: OffsetDateTime::now_utc(),
            reason: None,
        }
    }

    #[must_use]
    pub fn unavailable(stage: ReadinessStage, reason: impl Into<String>) -> Self {
        Self {
            stage,
            ready: false,
            checked_at: OffsetDateTime::now_utc(),
            reason: Some(reason.into()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RouteIdentity {
    pub route_id: String,
    pub provider: String,
    pub model: String,
    pub protocol: String,
    pub endpoint_origin: String,
    pub auth_source: String,
}

impl RouteIdentity {
    pub fn validate(&self) -> Result<()> {
        for (field, value) in [
            ("route_id", &self.route_id),
            ("provider", &self.provider),
            ("model", &self.model),
            ("protocol", &self.protocol),
            ("endpoint_origin", &self.endpoint_origin),
            ("auth_source", &self.auth_source),
        ] {
            if value.trim().is_empty() {
                return Err(ContractError::Validation(format!(
                    "route identity field {field} cannot be empty"
                )));
            }
        }
        if self.endpoint_origin.contains('@') {
            return Err(ContractError::Validation(
                "endpoint origin cannot contain embedded credentials".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderCapabilitySet {
    pub image_input: bool,
    pub tool_calling: bool,
    pub streaming_text: bool,
    pub streaming_audio: bool,
    pub cancellation: bool,
    pub supported_image_media_types: Vec<String>,
    pub max_image_bytes: Option<u64>,
    pub max_images_per_request: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RouteReadiness {
    pub schema_version: u16,
    pub identity: RouteIdentity,
    pub capabilities: ProviderCapabilitySet,
    pub checks: Vec<ReadinessCheck>,
}

impl RouteReadiness {
    pub fn new(
        identity: RouteIdentity,
        capabilities: ProviderCapabilitySet,
        checks: Vec<ReadinessCheck>,
    ) -> Result<Self> {
        let report = Self {
            schema_version: SCHEMA_VERSION,
            identity,
            capabilities,
            checks,
        };
        report.validate()?;
        Ok(report)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ContractError::Validation(
                "route readiness schema is unsupported".to_owned(),
            ));
        }
        self.identity.validate()?;
        let mut previous = None;
        for check in &self.checks {
            if previous.is_some_and(|stage| stage >= check.stage) {
                return Err(ContractError::Validation(
                    "readiness checks must be strictly ordered and unique".to_owned(),
                ));
            }
            if !check.ready && check.reason.as_deref().is_none_or(str::is_empty) {
                return Err(ContractError::Validation(
                    "unavailable readiness checks require an actionable reason".to_owned(),
                ));
            }
            previous = Some(check.stage);
        }
        if self.capabilities.streaming_text
            && !self.stage_ready(ReadinessStage::CapabilityAvailable)
        {
            return Err(ContractError::Validation(
                "streaming cannot be advertised before capability availability is verified"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn stage_ready(&self, stage: ReadinessStage) -> bool {
        self.checks
            .iter()
            .find(|check| check.stage == stage)
            .is_some_and(|check| check.ready)
    }

    #[must_use]
    pub fn ready_for_requests(&self) -> bool {
        self.stage_ready(ReadinessStage::Authenticated)
            && self.stage_ready(ReadinessStage::CapabilityAvailable)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationState {
    NotRequested,
    Requested,
    TransportInterrupted,
    ProviderAcknowledged,
    TimedOut,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CancellationReceipt {
    pub state: CancellationState,
    pub requested_at: Option<OffsetDateTime>,
    pub completed_at: Option<OffsetDateTime>,
    pub bounded_within_ms: Option<u64>,
}

impl CancellationReceipt {
    pub fn validate(&self) -> Result<()> {
        if self.state != CancellationState::NotRequested && self.requested_at.is_none() {
            return Err(ContractError::Validation(
                "requested cancellation requires a request timestamp".to_owned(),
            ));
        }
        if matches!(
            self.state,
            CancellationState::TransportInterrupted | CancellationState::ProviderAcknowledged
        ) && self.completed_at.is_none()
        {
            return Err(ContractError::Validation(
                "completed cancellation requires a completion timestamp".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalTransport {
    DirectHttps,
    GitHubCli,
    NativeCli,
    LocalSocket,
    WebSocket,
    WebRtc,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationLifecycle {
    Requested,
    Authorized,
    Dispatched,
    Accepted,
    Completed,
    Persisted,
    Uncertain,
    Reconciled,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationState {
    NotRequired,
    Pending,
    FoundExisting,
    ConfirmedAbsent,
    Inconclusive,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExternalOperation {
    RepositoryMetadata {
        repository: String,
    },
    RepositoryCreate {
        owner: String,
        name: String,
        private: bool,
        description: Option<String>,
    },
    IssueCreate {
        repository: String,
        title: String,
        body: String,
    },
    PullRequestMerge {
        repository: String,
        number: u64,
        strategy: String,
        expected_head: Option<String>,
    },
    WorkflowRerun {
        repository: String,
        run_id: u64,
        failed_jobs_only: bool,
    },
    ArtifactDownload {
        repository: String,
        artifact_id: u64,
        destination: String,
        max_bytes: u64,
    },
    Custom {
        namespace: String,
        version: u16,
        payload: Value,
    },
}

impl ExternalOperation {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::RepositoryMetadata { repository }
            | Self::IssueCreate { repository, .. }
            | Self::PullRequestMerge { repository, .. }
            | Self::WorkflowRerun { repository, .. }
            | Self::ArtifactDownload { repository, .. } => validate_repository(repository),
            Self::RepositoryCreate { owner, name, .. } => {
                validate_segment("owner", owner)?;
                validate_segment("repository name", name)
            }
            Self::Custom {
                namespace,
                version,
                ..
            } => {
                validate_segment("custom operation namespace", namespace)?;
                if *version == 0 {
                    return Err(ContractError::Validation(
                        "custom operation version must be non-zero".to_owned(),
                    ));
                }
                Ok(())
            }
        }
    }

    #[must_use]
    pub fn is_non_idempotent_create(&self) -> bool {
        matches!(self, Self::RepositoryCreate { .. } | Self::IssueCreate { .. })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OperationEnvelope {
    pub schema_version: u16,
    pub attempt_id: AttemptId,
    pub request_digest: RequestDigest,
    pub idempotency_key: Option<IdempotencyKey>,
    pub operation: ExternalOperation,
    pub requested_at: OffsetDateTime,
}

impl OperationEnvelope {
    pub fn new(operation: ExternalOperation, idempotency_key: Option<IdempotencyKey>) -> Result<Self> {
        operation.validate()?;
        let request_digest = RequestDigest::from_canonical(&operation)?;
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            attempt_id: AttemptId::new(),
            request_digest,
            idempotency_key,
            operation,
            requested_at: OffsetDateTime::now_utc(),
        })
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ContractError::Validation(
                "operation envelope schema is unsupported".to_owned(),
            ));
        }
        AttemptId::parse(self.attempt_id.0.clone())?;
        self.operation.validate()?;
        let expected = RequestDigest::from_canonical(&self.operation)?;
        if self.request_digest != expected {
            return Err(ContractError::Validation(
                "operation request digest does not match the canonical typed operation".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RateLimitReceipt {
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    pub reset_epoch_seconds: Option<i64>,
    pub retry_after_seconds: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OperationReceipt {
    pub schema_version: u16,
    pub attempt_id: AttemptId,
    pub request_digest: RequestDigest,
    pub idempotency_key_fingerprint: Option<String>,
    pub lifecycle: OperationLifecycle,
    pub reconciliation: ReconciliationState,
    pub transport: ExternalTransport,
    pub backend: String,
    pub host: String,
    pub authenticated_identity: Option<String>,
    pub resource_id: Option<String>,
    pub resource_url: Option<String>,
    pub http_status: Option<u16>,
    pub request_id: Option<String>,
    pub retry_count: u32,
    pub rate_limit: RateLimitReceipt,
    pub cancellation: CancellationReceipt,
    pub redacted: bool,
    pub persisted: bool,
    pub metadata: BTreeMap<String, Value>,
    pub completed_at: OffsetDateTime,
}

impl OperationReceipt {
    pub fn for_envelope(
        envelope: &OperationEnvelope,
        transport: ExternalTransport,
        backend: impl Into<String>,
        host: impl Into<String>,
        lifecycle: OperationLifecycle,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            attempt_id: envelope.attempt_id.clone(),
            request_digest: envelope.request_digest.clone(),
            idempotency_key_fingerprint: envelope
                .idempotency_key
                .as_ref()
                .map(IdempotencyKey::fingerprint),
            lifecycle,
            reconciliation: ReconciliationState::NotRequired,
            transport,
            backend: backend.into(),
            host: host.into(),
            authenticated_identity: None,
            resource_id: None,
            resource_url: None,
            http_status: None,
            request_id: None,
            retry_count: 0,
            rate_limit: RateLimitReceipt::default(),
            cancellation: CancellationReceipt {
                state: CancellationState::NotRequested,
                requested_at: None,
                completed_at: None,
                bounded_within_ms: None,
            },
            redacted: true,
            persisted: false,
            metadata: BTreeMap::new(),
            completed_at: OffsetDateTime::now_utc(),
        }
    }

    pub fn validate_against(&self, envelope: &OperationEnvelope) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ContractError::Validation(
                "operation receipt schema is unsupported".to_owned(),
            ));
        }
        if self.attempt_id != envelope.attempt_id
            || self.request_digest != envelope.request_digest
        {
            return Err(ContractError::Validation(
                "operation receipt is not bound to the submitted attempt".to_owned(),
            ));
        }
        if self.backend.trim().is_empty() || self.host.trim().is_empty() {
            return Err(ContractError::Validation(
                "operation receipt requires backend and host identity".to_owned(),
            ));
        }
        if !self.redacted {
            return Err(ContractError::Validation(
                "durable operation receipts must pass redaction".to_owned(),
            ));
        }
        if self.lifecycle == OperationLifecycle::Persisted && !self.persisted {
            return Err(ContractError::Validation(
                "persisted lifecycle requires durable persistence confirmation".to_owned(),
            ));
        }
        if envelope.operation.is_non_idempotent_create()
            && self.lifecycle == OperationLifecycle::Uncertain
            && self.reconciliation == ReconciliationState::NotRequired
        {
            return Err(ContractError::Validation(
                "uncertain create operations require reconciliation before retry".to_owned(),
            ));
        }
        self.cancellation.validate()
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
        let candidate = Self::parse(url)?;
        Ok(self == &candidate)
    }
}

fn canonical_json<T: Serialize>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value)?;
    let canonical = canonicalize(value);
    Ok(serde_json::to_string(&canonical)?)
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

fn validate_repository(repository: &str) -> Result<()> {
    let (owner, name) = repository.split_once('/').ok_or_else(|| {
        ContractError::Validation("repository must use owner/name form".to_owned())
    })?;
    validate_segment("repository owner", owner)?;
    validate_segment("repository name", name)
}

fn validate_segment(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty()
        || value.contains('/')
        || value.chars().any(char::is_whitespace)
    {
        return Err(ContractError::Validation(format!(
            "{label} must be one non-empty path segment"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue_operation(body: &str) -> ExternalOperation {
        ExternalOperation::IssueCreate {
            repository: "octo/repo".to_owned(),
            title: "Title".to_owned(),
            body: body.to_owned(),
        }
    }

    #[test]
    fn canonical_digest_changes_with_semantic_body() {
        let first = RequestDigest::from_canonical(&issue_operation("one")).unwrap();
        let second = RequestDigest::from_canonical(&issue_operation("two")).unwrap();
        assert_ne!(first, second);
        assert_eq!(first, RequestDigest::from_canonical(&issue_operation("one")).unwrap());
    }

    #[test]
    fn attempt_ids_are_unique_and_parseable() {
        let first = AttemptId::new();
        let second = AttemptId::new();
        assert_ne!(first, second);
        assert_eq!(AttemptId::parse(first.to_string()).unwrap(), first);
    }

    #[test]
    fn idempotency_keys_are_bounded_and_safe() {
        let key = IdempotencyKey::parse("release:repo:create-1").unwrap();
        assert_eq!(key.fingerprint().len(), 64);
        assert!(IdempotencyKey::parse("").is_err());
        assert!(IdempotencyKey::parse("contains space").is_err());
        assert!(IdempotencyKey::parse("x".repeat(MAX_IDEMPOTENCY_KEY_BYTES + 1)).is_err());
    }

    #[test]
    fn readiness_cannot_claim_unverified_streaming() {
        let result = RouteReadiness::new(
            RouteIdentity {
                route_id: "primary".to_owned(),
                provider: "minimax".to_owned(),
                model: "MiniMax-M3".to_owned(),
                protocol: "anthropic".to_owned(),
                endpoint_origin: "https://api.minimax.io".to_owned(),
                auth_source: "api-key".to_owned(),
            },
            ProviderCapabilitySet {
                streaming_text: true,
                ..ProviderCapabilitySet::default()
            },
            vec![ReadinessCheck::ready(ReadinessStage::ProfileSaved)],
        );
        assert!(result.is_err());
    }

    #[test]
    fn readiness_separates_saved_auth_and_live_verification() {
        let report = RouteReadiness::new(
            RouteIdentity {
                route_id: "primary".to_owned(),
                provider: "openai".to_owned(),
                model: "gpt".to_owned(),
                protocol: "openai".to_owned(),
                endpoint_origin: "https://api.openai.com".to_owned(),
                auth_source: "oauth".to_owned(),
            },
            ProviderCapabilitySet::default(),
            vec![
                ReadinessCheck::ready(ReadinessStage::ProfileSaved),
                ReadinessCheck::ready(ReadinessStage::SecretPresent),
                ReadinessCheck::unavailable(
                    ReadinessStage::EndpointReachable,
                    "gateway is not running",
                ),
            ],
        )
        .unwrap();
        assert!(!report.ready_for_requests());
        assert!(!report.stage_ready(ReadinessStage::Authenticated));
    }

    #[test]
    fn uncertain_creates_require_reconciliation() {
        let envelope = OperationEnvelope::new(issue_operation("body"), None).unwrap();
        let mut receipt = OperationReceipt::for_envelope(
            &envelope,
            ExternalTransport::DirectHttps,
            "github-rest",
            "github.com",
            OperationLifecycle::Uncertain,
        );
        assert!(receipt.validate_against(&envelope).is_err());
        receipt.reconciliation = ReconciliationState::Pending;
        assert!(receipt.validate_against(&envelope).is_ok());
    }

    #[test]
    fn receipts_are_bound_to_attempt_and_digest() {
        let first = OperationEnvelope::new(issue_operation("one"), None).unwrap();
        let second = OperationEnvelope::new(issue_operation("two"), None).unwrap();
        let receipt = OperationReceipt::for_envelope(
            &first,
            ExternalTransport::GitHubCli,
            "gh",
            "github.com",
            OperationLifecycle::Completed,
        );
        assert!(receipt.validate_against(&first).is_ok());
        assert!(receipt.validate_against(&second).is_err());
    }

    #[test]
    fn trusted_hosts_reject_credentials_and_cross_host_tokens() {
        let github = TrustedHost::parse("https://api.github.com").unwrap();
        assert!(github.permits("https://api.github.com/repos/x/y").unwrap());
        assert!(!github.permits("https://evil.example/repos/x/y").unwrap());
        assert!(TrustedHost::parse("https://token@api.github.com").is_err());
    }

    #[test]
    fn cancellation_receipts_require_bounded_completion_evidence() {
        let receipt = CancellationReceipt {
            state: CancellationState::TransportInterrupted,
            requested_at: Some(OffsetDateTime::now_utc()),
            completed_at: None,
            bounded_within_ms: Some(100),
        };
        assert!(receipt.validate().is_err());
    }
}
