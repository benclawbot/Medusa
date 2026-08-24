use std::sync::atomic::{AtomicBool, Ordering};

use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ProviderContinuationCapabilities, ProviderStreamEvent, ReasoningExchangeRequest,
    cancelled_provider_error,
};

/// Removes provider-emitted private reasoning wrappers from visible text.
pub(crate) fn strip_hidden_reasoning(text: &str) -> String {
    const OPEN_TAGS: [&str; 2] = ["<think", "<analysis"];
    const CLOSE_TAGS: [&str; 2] = ["</think", "</analysis"];

    let lower = text.to_ascii_lowercase();
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    loop {
        let next_open = OPEN_TAGS
            .iter()
            .filter_map(|tag| lower[cursor..].find(tag).map(|offset| cursor + offset))
            .min();
        let Some(open_start) = next_open else {
            output.push_str(&text[cursor..]);
            break;
        };
        output.push_str(&text[cursor..open_start]);
        let Some(open_end) = lower[open_start..]
            .find('>')
            .map(|offset| open_start + offset + 1)
        else {
            break;
        };
        let next_close = CLOSE_TAGS
            .iter()
            .filter_map(|tag| lower[open_end..].find(tag).map(|offset| open_end + offset))
            .min();
        let Some(close_start) = next_close else {
            break;
        };
        let Some(close_end) = lower[close_start..]
            .find('>')
            .map(|offset| close_start + offset + 1)
        else {
            break;
        };
        cursor = close_end;
    }
    output.trim().to_owned()
}

/// Strict tool definition sent to the model.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Conversation role.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// Provider-neutral image source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
    AttachmentRef { attachment_id: String },
}

/// Provider-neutral message content.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageBlock {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
        #[serde(skip_serializing_if = "Option::is_none")]
        alt_text: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

/// Provider-neutral conversation message.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<MessageBlock>,
}

/// Execution phase used by routing policy without contaminating provider request payloads.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderExecutionPhase {
    #[default]
    Default,
    Planning,
    Implementation,
    HighRiskReview,
    Repair,
    Summarization,
    Formatting,
}

/// Why one physical provider route is being attempted for an effective request.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAttemptKind {
    Primary,
    Retry,
    Failover,
    HedgePrimary,
    HedgeSecondary,
}

/// Sanitized route identity persisted before a physical provider invocation.
///
/// Endpoint URLs and authentication sources are deliberately excluded so an audit record cannot
/// become a credential or signed-URL side channel.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderAttemptDescriptor {
    pub route_id: String,
    pub provider: String,
    pub model: String,
    pub protocol: String,
    pub tool_calling: bool,
    pub streaming: bool,
    pub max_retries: u8,
    pub retry_base_delay_ms: u64,
    pub retry_max_delay_ms: u64,
    pub retry_jitter_ms: u64,
    pub route_ordinal: usize,
    pub retry_ordinal: u8,
    pub kind: ProviderAttemptKind,
    pub conditional: bool,
    pub conditional_launch_after_ms: Option<u64>,
}

/// One model request.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ModelRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub max_tokens: u32,
    pub temperature_milli: u16,
}

/// A returned response block. Thinking blocks are intentionally omitted.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

/// Usage accounting returned by the provider.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

/// Explicit provider feature contract used before request submission.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderCapabilities {
    pub image_input: bool,
    pub supported_image_media_types: Vec<String>,
    pub max_image_bytes: Option<u64>,
    pub max_images_per_request: Option<u32>,
    pub tool_calling: bool,
    pub streaming: bool,
}

/// Provider response stripped of private hidden reasoning.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ModelResponse {
    pub response_id: Option<String>,
    pub stop_reason: Option<String>,
    pub blocks: Vec<ResponseBlock>,
    pub usage: Usage,
}

/// Pluggable provider interface used by orchestration.
pub trait ModelProvider {
    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse>;

    /// Completes a request with an explicitly validated visible reasoning handoff.
    ///
    /// The handoff is appended as a bounded user-role message. Opaque provider continuation
    /// state is rejected here and must be consumed by a provider adapter that owns its wire
    /// protocol. This default keeps existing providers and callers source-compatible.
    fn complete_with_exchange(
        &self,
        exchange: &ReasoningExchangeRequest,
    ) -> MedusaResult<ModelResponse> {
        let request = exchange.visible_request()?;
        self.complete(&request)
    }

    fn complete_streaming_with_exchange(
        &self,
        exchange: &ReasoningExchangeRequest,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        let request = exchange.visible_request()?;
        self.complete_streaming(&request, sink)
    }

    fn complete_cancellable_with_exchange(
        &self,
        exchange: &ReasoningExchangeRequest,
        cancel: &AtomicBool,
    ) -> MedusaResult<ModelResponse> {
        let request = exchange.visible_request()?;
        self.complete_cancellable(&request, cancel)
    }

    fn complete_streaming_cancellable_with_exchange(
        &self,
        exchange: &ReasoningExchangeRequest,
        cancel: &AtomicBool,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        let request = exchange.visible_request()?;
        self.complete_streaming_cancellable(&request, cancel, sink)
    }

    /// Streams provider-neutral events when the route supports incremental delivery.
    ///
    /// The default preserves compatibility for non-streaming routes by producing only a terminal
    /// event after the ordinary completion call. Callers must check `capabilities().streaming`
    /// before relying on incremental delivery or time-to-first-event measurements.
    fn complete_streaming(
        &self,
        request: &ModelRequest,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        let response = self.complete(request)?;
        sink(ProviderStreamEvent::Completed {
            response: response.clone(),
        })?;
        Ok(response)
    }

    /// Streams provider-neutral events while preserving cooperative cancellation.
    /// Streaming-capable providers should override this so cancellation reaches the socket.
    fn complete_streaming_cancellable(
        &self,
        request: &ModelRequest,
        cancel: &AtomicBool,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        if cancel.load(Ordering::SeqCst) {
            return Err(cancelled_provider_error());
        }
        let response = self.complete_streaming(request, sink)?;
        if cancel.load(Ordering::SeqCst) {
            return Err(cancelled_provider_error());
        }
        Ok(response)
    }

    /// Streams with an explicit execution phase for phase-aware route selection.
    /// Providers that do not route internally can ignore the phase and preserve existing behavior.
    fn complete_streaming_cancellable_for_phase(
        &self,
        request: &ModelRequest,
        _phase: ProviderExecutionPhase,
        cancel: &AtomicBool,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        self.complete_streaming_cancellable(request, cancel, sink)
    }

    /// Streams while allowing route-managing providers to durably certify each physical attempt
    /// before it starts. Direct providers use the already-persisted logical request manifest and
    /// therefore do not emit an additional nested attempt by default.
    fn complete_streaming_cancellable_for_phase_with_attempts(
        &self,
        request: &ModelRequest,
        phase: ProviderExecutionPhase,
        cancel: &AtomicBool,
        _before_attempt: &mut dyn FnMut(&ProviderAttemptDescriptor) -> MedusaResult<()>,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        self.complete_streaming_cancellable_for_phase(request, phase, cancel, sink)
    }

    fn complete_cancellable(
        &self,
        request: &ModelRequest,
        cancel: &AtomicBool,
    ) -> MedusaResult<ModelResponse> {
        if cancel.load(Ordering::SeqCst) {
            return Err(cancelled_provider_error());
        }
        let response = self.complete(request)?;
        if cancel.load(Ordering::SeqCst) {
            return Err(cancelled_provider_error());
        }
        Ok(response)
    }

    /// Completes with an explicit execution phase for phase-aware route selection.
    /// Providers that do not route internally can ignore the phase and preserve existing behavior.
    fn complete_cancellable_for_phase(
        &self,
        request: &ModelRequest,
        _phase: ProviderExecutionPhase,
        cancel: &AtomicBool,
    ) -> MedusaResult<ModelResponse> {
        self.complete_cancellable(request, cancel)
    }

    /// Completes while allowing route-managing providers to durably certify each physical attempt
    /// before it starts. The callback must fail closed: returning an error prevents invocation.
    fn complete_cancellable_for_phase_with_attempts(
        &self,
        request: &ModelRequest,
        phase: ProviderExecutionPhase,
        cancel: &AtomicBool,
        _before_attempt: &mut dyn FnMut(&ProviderAttemptDescriptor) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        self.complete_cancellable_for_phase(request, phase, cancel)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    /// Native continuation is opt-in and fail-closed by default. Provider adapters may advertise
    /// a reviewed protocol contract without exposing the opaque payload to provider-neutral code.
    fn continuation_capabilities(&self) -> ProviderContinuationCapabilities {
        ProviderContinuationCapabilities::default()
    }

    /// Returns metadata for the most recently completed provider execution.
    fn execution_status(&self) -> Option<Value> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_reasoning_tags_are_removed_without_losing_visible_text() {
        assert_eq!(
            strip_hidden_reasoning("before <think>private</think> after"),
            "before  after"
        );
        assert_eq!(strip_hidden_reasoning("<analysis>private"), "");
        assert_eq!(
            strip_hidden_reasoning("<THINK>private</THINK>answer"),
            "answer"
        );
    }

    #[test]
    fn image_block_serializes_as_structured_content() {
        let value = serde_json::to_value(MessageBlock::Image {
            source: ImageSource::Base64 {
                media_type: "image/png".to_owned(),
                data: "AAEC".to_owned(),
            },
            alt_text: Some("test screenshot".to_owned()),
        })
        .expect("serialize image");
        assert_eq!(value["type"], "image");
        assert_eq!(value["source"]["type"], "base64");
        assert_eq!(value["source"]["media_type"], "image/png");
    }
}
