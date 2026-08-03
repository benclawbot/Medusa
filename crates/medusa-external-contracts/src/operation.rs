use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::{
    AttemptId, CancellationReceipt, CancellationState, ContractError, IdempotencyKey,
    RequestDigest, Result, SCHEMA_VERSION,
};

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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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
                namespace, version, ..
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
        matches!(
            self,
            Self::RepositoryCreate { .. } | Self::IssueCreate { .. }
        )
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
    pub fn new(
        operation: ExternalOperation,
        idempotency_key: Option<IdempotencyKey>,
    ) -> Result<Self> {
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
        AttemptId::parse(self.attempt_id.to_string())?;
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
        if self.attempt_id != envelope.attempt_id || self.request_digest != envelope.request_digest
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

fn validate_repository(repository: &str) -> Result<()> {
    let (owner, name) = repository.split_once('/').ok_or_else(|| {
        ContractError::Validation("repository must use owner/name form".to_owned())
    })?;
    validate_segment("repository owner", owner)?;
    validate_segment("repository name", name)
}

fn validate_segment(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.contains('/') || value.chars().any(char::is_whitespace) {
        return Err(ContractError::Validation(format!(
            "{label} must be one non-empty path segment"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(body: &str) -> ExternalOperation {
        ExternalOperation::IssueCreate {
            repository: "octo/repo".to_owned(),
            title: "Title".to_owned(),
            body: body.to_owned(),
        }
    }

    #[test]
    fn semantic changes alter_request_identity() {
        let one = OperationEnvelope::new(issue("one"), None).unwrap();
        let two = OperationEnvelope::new(issue("two"), None).unwrap();
        assert_ne!(one.request_digest, two.request_digest);
    }

    #[test]
    fn uncertain_creates_require_reconciliation() {
        let envelope = OperationEnvelope::new(issue("body"), None).unwrap();
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
    fn receipts_are_bound_to_exact_attempts() {
        let first = OperationEnvelope::new(issue("one"), None).unwrap();
        let second = OperationEnvelope::new(issue("two"), None).unwrap();
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
}
