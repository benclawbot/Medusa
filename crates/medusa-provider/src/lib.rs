//! Provider-neutral model contracts and production provider adapters.

mod anthropic;
mod configured;
mod contracts;
mod http;
mod manager;
mod openai;

pub use anthropic::MiniMaxProvider;
pub use configured::ConfiguredProvider;
pub use contracts::{
    ImageSource, Message, MessageBlock, ModelProvider, ModelRequest, ModelResponse,
    ProviderCapabilities, ResponseBlock, Role, ToolDefinition, Usage,
};
pub use manager::{ProviderHealth, ProviderManager, ProviderRouteProfile, RouteRetryPolicy};
pub use openai::OpenAiProvider;

pub(crate) use http::{
    async_response_error, blocking_response_error, cancelled_provider_error, classify_status,
    provider_error, provider_response_error, run_cancellable_request, shared_async_http_client,
    shared_blocking_http_client,
};