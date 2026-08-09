use std::sync::atomic::{AtomicBool, Ordering};

use medusa_core::MedusaResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ProviderStreamEvent, cancelled_provider_error};

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

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
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
