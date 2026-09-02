//! Provider-neutral model contracts and production provider adapters.

mod anthropic;
mod configured;
mod contracts;
mod health_store;
mod hedge_acceptance;
mod hedge_runtime;
mod hedging;
mod http;
mod manager;
mod model_discovery;
mod openai;
mod openai_streaming;
mod openai_transport;
mod reasoning_exchange;
mod route_latency;
mod route_metrics_store;
mod streaming;
mod streaming_tool_calls;
mod verification_bridge;
mod verified_routing;

#[derive(Debug, Default, serde::Deserialize)]
struct OpenAiPromptTokenDetails {
    #[serde(default)]
    cached_tokens: u64,
}

pub use anthropic::MiniMaxProvider;
pub use configured::ConfiguredProvider;
pub(crate) use contracts::split_dynamic_system_context;
pub(crate) use contracts::strip_hidden_reasoning;
pub use contracts::{
    DYNAMIC_SYSTEM_CONTEXT_MARKER, ImageSource, Message, MessageBlock, ModelProvider, ModelRequest,
    ModelResponse, ProviderAttemptDescriptor, ProviderAttemptKind, ProviderCapabilities,
    ProviderExecutionPhase, ResponseBlock, Role, ToolDefinition, Usage,
};
pub use health_store::ProviderHealthStore;
pub use hedge_acceptance::{
    HEDGE_ACCEPTANCE_MIN_SAMPLES, HedgeLatencyAcceptance, assess_hedge_latency_acceptance,
};
pub use hedging::{HedgeDecision, HedgePolicy, hedge_decision};
pub use manager::{ProviderHealth, ProviderManager, ProviderRouteProfile, RouteRetryPolicy};
pub use model_discovery::{ModelDiscoveryError, discover_models};
pub use openai::OpenAiProvider;
pub use openai_streaming::OpenAiStreamAccumulator;
pub use reasoning_exchange::{
    Alternative, Assumption, AssumptionStatus, ContinuationDisposition, ContinuationModelBinding,
    Decision, EvidenceRef, HandoffPolicy, HandoffSource, HandoffTarget, HandoffTransfer,
    HandoffTrustState, MAX_HANDOFF_EVIDENCE_ITEMS, MAX_HANDOFF_LIST_ITEMS, MAX_HANDOFF_TEXT_BYTES,
    ProviderContinuationCapabilities, ProviderContinuationState, REASONING_HANDOFF_SCHEMA_VERSION,
    ReasoningExchangeRequest, ReasoningHandoffV1, RouteIdentity, VerificationResult,
};
pub use route_latency::{
    RouteLatencyPolicy, RouteLatencyStats, expected_latency_ms, latency_aware_route_order,
};
pub use route_metrics_store::ProviderRouteLatencyStore;
pub use streaming::{ProviderStreamEvent, ProviderStreamTranscript, SequencedStreamEvent};
pub use streaming_tool_calls::StreamingToolCallAssembler;
#[doc(hidden)]
pub use verification_bridge::{
    clear_pending_route_verification, mark_pending_route_mutation,
    record_pending_route_verification,
};
pub use verified_routing::{
    ExcludedVerifiedRoute, RouteSelectionReceipt, VerifiedRouteContext, VerifiedRouteDecision,
    VerifiedRouteEvidence, VerifiedRoutingObjective, VerifiedRoutingPolicy, select_verified_route,
    select_verified_route_with_latency_policy,
};

pub(crate) use http::{
    async_response_error, async_response_json, blocking_response_error, blocking_response_json,
    cancelled_provider_error, provider_error, provider_response_error, run_cancellable_request,
    shared_async_http_client, shared_blocking_http_client,
};

#[cfg(test)]
extern crate self as tempfile;

#[cfg(test)]
#[doc(hidden)]
pub struct TempDir(std::path::PathBuf);

#[cfg(test)]
impl TempDir {
    #[doc(hidden)]
    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

#[cfg(test)]
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
#[doc(hidden)]
pub fn tempdir() -> std::io::Result<TempDir> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!(
        "medusa-provider-health-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::SeqCst),
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(TempDir(path))
}
