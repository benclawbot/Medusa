//! Versioned provider, authentication, cancellation, and external-operation contracts.
//!
//! This crate is transport-neutral. Provider adapters, OAuth lifecycles, GitHub backends,
//! frontends, and durable runtime state consume these types instead of inventing readiness,
//! capability, mutation, or receipt state independently.

mod auth;
mod identity;
mod operation;
mod provider;

pub use auth::{
    AuthenticationMethod, CredentialState, OAuthLifecycleReceipt, OAuthStage, OAuthStageReceipt,
    PinnedOAuthComponent,
};
pub use identity::{
    AttemptId, IdempotencyKey, MAX_IDEMPOTENCY_KEY_BYTES, RequestDigest, TrustedHost,
};
pub use operation::{
    ExternalOperation, ExternalTransport, OperationEnvelope, OperationLifecycle, OperationReceipt,
    RateLimitReceipt, ReconciliationState,
};
pub use provider::{
    CancellationReceipt, CancellationState, ProviderCapabilitySet, ReadinessCheck, ReadinessStage,
    RouteIdentity, RouteReadiness,
};

pub const SCHEMA_VERSION: u16 = 1;

pub type Result<T> = std::result::Result<T, ContractError>;

#[derive(Debug, thiserror::Error)]
pub enum ContractError {
    #[error("external contract validation failed: {0}")]
    Validation(String),
    #[error("external contract serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}
