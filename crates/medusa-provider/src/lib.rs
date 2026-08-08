//! Provider-neutral model contracts and production provider adapters.

mod anthropic;
mod configured;
mod contracts;
mod health_store;
mod hedging;
mod http;
mod manager;
mod openai;
mod openai_streaming;
mod openai_transport;
mod route_latency;
mod route_metrics_store;
mod streaming;
mod streaming_tool_calls;

pub use anthropic::MiniMaxProvider;
pub use configured::ConfiguredProvider;
pub use contracts::{
    ImageSource, Message, MessageBlock, ModelProvider, ModelRequest, ModelResponse,
    ProviderCapabilities, ResponseBlock, Role, ToolDefinition, Usage,
};
pub use health_store::ProviderHealthStore;
pub use hedging::{HedgeDecision, HedgePolicy, hedge_decision};
pub use manager::{ProviderHealth, ProviderManager, ProviderRouteProfile, RouteRetryPolicy};
pub use openai::OpenAiProvider;
pub use openai_streaming::OpenAiStreamAccumulator;
pub use route_latency::{
    RouteLatencyPolicy, RouteLatencyStats, expected_latency_ms, latency_aware_route_order,
};
pub use route_metrics_store::ProviderRouteLatencyStore;
pub use streaming::{ProviderStreamEvent, ProviderStreamTranscript, SequencedStreamEvent};
pub use streaming_tool_calls::StreamingToolCallAssembler;

pub(crate) use http::{
    async_response_error, blocking_response_error, cancelled_provider_error, provider_error,
    provider_response_error, run_cancellable_request, shared_async_http_client,
    shared_blocking_http_client,
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
