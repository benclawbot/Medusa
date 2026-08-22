use std::{
    collections::HashMap,
    env,
    sync::{Arc, Mutex, atomic::AtomicBool},
};

use medusa_config::{Config, model_capabilities};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{Client as AsyncClient, blocking::Client as BlockingClient};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    MessageBlock, ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities, ResponseBlock,
    Usage, async_response_error, async_response_json, blocking_response_error,
    blocking_response_json, provider_error, run_cancellable_request, shared_async_http_client,
    shared_blocking_http_client,
};

type WireHistory = Arc<Mutex<HashMap<String, Arc<Vec<Value>>>>>;

/// Anthropic Messages API adapter for MiniMax, Anthropic, and compatible providers.
#[derive(Clone)]
pub struct MiniMaxProvider {
    blocking_client: BlockingClient,
    async_client: AsyncClient,
    base_url: String,
    api_key: String,
    model: String,
    capabilities: ProviderCapabilities,
    wire_history: WireHistory,
}

impl MiniMaxProvider {
    /// Builds an adapter from typed model configuration and provider environment variables.
    pub fn from_config(config: &Config) -> MedusaResult<Self> {
        Self::from_config_with_api_key(config, None)
    }

    /// Builds an adapter with an optional session-only credential supplied by an interactive client.
    pub fn from_config_with_api_key(
        config: &Config,
        session_api_key: Option<String>,
    ) -> MedusaResult<Self> {
        let settings = provider_settings(&config.model.provider)?;
        let api_key = session_api_key
            .or_else(|| env::var(settings.api_key_env).ok())
            .ok_or_else(|| {
                MedusaError::new(
                    ErrorCode::DependencyUnavailable,
                    ErrorCategory::Environment,
                    format!("missing provider credential in {}", settings.api_key_env),
                )
            })?;
        let base_url = config
            .model
            .base_url
            .clone()
            .or_else(|| env::var(settings.base_url_env).ok())
            .unwrap_or_else(|| settings.default_base_url.to_owned());
        let mut capabilities = (settings.capabilities)();
        let registry_capabilities = model_capabilities(&config.model.provider, &config.model.name);
        capabilities.image_input = capabilities.image_input && registry_capabilities.image_input;
        capabilities.tool_calling = config.model.tool_calling && registry_capabilities.tool_calling;
        capabilities.streaming = false;
        Ok(Self {
            blocking_client: shared_blocking_http_client()?,
            async_client: shared_async_http_client()?,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
            model: config.model.name.clone(),
            capabilities,
            wire_history: Arc::default(),
        })
    }

    fn request_messages(&self, request: &ModelRequest) -> Value {
        let mut messages = json!(request.messages);
        let history = self
            .wire_history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(message_values) = messages.as_array_mut() else {
            return messages;
        };
        for message in message_values {
            if message.get("role").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
                continue;
            };
            let replay = content
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
                .filter_map(|block| block.get("id").and_then(Value::as_str))
                .find_map(|tool_use_id| history.get(tool_use_id).cloned());
            if let Some(replay) = replay {
                *content = replay.as_ref().clone();
            }
        }
        messages
    }

    fn request_body(&self, request: &ModelRequest) -> Value {
        let mut tools = json!(request.tools);
        if let Some(last) = tools.as_array_mut().and_then(|items| items.last_mut())
            && let Some(object) = last.as_object_mut()
        {
            object.insert("cache_control".to_owned(), json!({"type": "ephemeral"}));
        }
        json!({
            "model": self.model,
            "system": [{
                "type": "text",
                "text": request.system,
                "cache_control": {"type": "ephemeral"}
            }],
            "messages": self.request_messages(request),
            "tools": tools,
            "max_tokens": request.max_tokens,
            "temperature": f64::from(request.temperature_milli) / 1000.0,
            "stream": false
        })
    }

    fn cache_wire_history(&self, content: &[WireBlock]) {
        if !content
            .iter()
            .any(|block| matches!(block, WireBlock::Thinking { .. }))
        {
            return;
        }
        let tool_use_ids = content
            .iter()
            .filter_map(|block| match block {
                WireBlock::ToolUse { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        if tool_use_ids.is_empty() {
            return;
        }
        let replay = Arc::new(
            content
                .iter()
                .filter_map(WireBlock::replay_value)
                .collect::<Vec<_>>(),
        );
        let mut history = self
            .wire_history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for tool_use_id in tool_use_ids {
            history.insert(tool_use_id, replay.clone());
        }
    }

    fn model_response_from_wire(&self, wire: WireResponse) -> ModelResponse {
        self.cache_wire_history(&wire.content);
        wire.into_model_response()
    }

    fn validate_request(&self, request: &ModelRequest) -> MedusaResult<()> {
        if !request.tools.is_empty() && !self.capabilities.tool_calling {
            return Err(MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Validation,
                "selected route does not support tool calling",
            ));
        }
        let images = request
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter(|block| matches!(block, MessageBlock::Image { .. }))
            .count();
        if images == 0 {
            return Ok(());
        }
        if !self.capabilities.image_input {
            return Err(MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Validation,
                "configured MiniMax model does not declare image-input support; screenshot submission was blocked",
            ));
        }
        if self
            .capabilities
            .max_images_per_request
            .is_some_and(|limit| images > limit as usize)
        {
            return Err(MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Validation,
                format!("request contains {images} images, exceeding provider limit"),
            ));
        }
        Ok(())
    }

    fn complete_request(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.validate_request(request)?;
        let endpoint = format!("{}/v1/messages", self.base_url);
        let response = self
            .blocking_client
            .post(&endpoint)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&self.request_body(request))
            .send()
            .map_err(provider_error)?;
        if response.status().is_success() {
            let wire: WireResponse = blocking_response_json(response)?;
            return Ok(self.model_response_from_wire(wire));
        }
        Err(blocking_response_error(response))
    }

    async fn complete_request_async(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.validate_request(request)?;
        let endpoint = format!("{}/v1/messages", self.base_url);
        let response = self
            .async_client
            .post(&endpoint)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&self.request_body(request))
            .send()
            .await
            .map_err(provider_error)?;
        if response.status().is_success() {
            let wire: WireResponse = async_response_json(response).await?;
            return Ok(self.model_response_from_wire(wire));
        }
        Err(async_response_error(response).await)
    }
}

impl ModelProvider for MiniMaxProvider {
    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.complete_request(request)
    }

    fn complete_cancellable(
        &self,
        request: &ModelRequest,
        cancel: &AtomicBool,
    ) -> MedusaResult<ModelResponse> {
        run_cancellable_request(cancel, self.complete_request_async(request))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }
}

struct ProviderSettings {
    api_key_env: &'static str,
    base_url_env: &'static str,
    default_base_url: &'static str,
    capabilities: fn() -> ProviderCapabilities,
}

fn provider_settings(provider: &str) -> MedusaResult<ProviderSettings> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "minimax" => Ok(ProviderSettings {
            api_key_env: "MINIMAX_API_KEY",
            base_url_env: "MINIMAX_BASE_URL",
            default_base_url: "https://api.minimax.io/anthropic",
            capabilities: minimax_capabilities_from_environment,
        }),
        "anthropic" => Ok(ProviderSettings {
            api_key_env: "ANTHROPIC_API_KEY",
            base_url_env: "ANTHROPIC_BASE_URL",
            default_base_url: "https://api.anthropic.com",
            capabilities: anthropic_capabilities,
        }),
        "anthropic-compatible" => Ok(ProviderSettings {
            api_key_env: "MEDUSA_API_KEY",
            base_url_env: "MEDUSA_BASE_URL",
            default_base_url: "https://api.minimax.io/anthropic",
            capabilities: ProviderCapabilities::default,
        }),
        other => Err(MedusaError::new(
            ErrorCode::InvalidConfiguration,
            ErrorCategory::Validation,
            format!(
                "unsupported provider {other}; choose minimax, anthropic, or anthropic-compatible"
            ),
        )),
    }
}

fn minimax_capabilities_from_environment() -> ProviderCapabilities {
    let image_input = env::var("MINIMAX_IMAGE_INPUT")
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes"));
    if image_input {
        ProviderCapabilities {
            image_input: true,
            supported_image_media_types: vec![
                "image/png".to_owned(),
                "image/jpeg".to_owned(),
                "image/webp".to_owned(),
                "image/gif".to_owned(),
            ],
            max_image_bytes: Some(20 * 1024 * 1024),
            max_images_per_request: Some(10),
            tool_calling: true,
            streaming: false,
        }
    } else {
        ProviderCapabilities {
            tool_calling: true,
            streaming: false,
            ..ProviderCapabilities::default()
        }
    }
}

fn anthropic_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        image_input: true,
        supported_image_media_types: vec![
            "image/png".to_owned(),
            "image/jpeg".to_owned(),
            "image/webp".to_owned(),
            "image/gif".to_owned(),
        ],
        max_image_bytes: Some(20 * 1024 * 1024),
        max_images_per_request: Some(20),
        tool_calling: true,
        streaming: false,
    }
}

#[derive(Clone, Debug, Deserialize)]
struct WireResponse {
    id: Option<String>,
    stop_reason: Option<String>,
    #[serde(default)]
    content: Vec<WireBlock>,
    #[serde(default)]
    usage: WireUsage,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

impl WireBlock {
    fn replay_value(&self) -> Option<Value> {
        match self {
            Self::Text { text } => Some(json!({"type": "text", "text": text})),
            Self::ToolUse { id, name, input } => Some(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input
            })),
            Self::Thinking {
                thinking,
                signature,
            } => {
                let mut block = json!({"type": "thinking", "thinking": thinking});
                if let Some(signature) = signature
                    && let Some(object) = block.as_object_mut()
                {
                    object.insert("signature".to_owned(), Value::String(signature.clone()));
                }
                Some(block)
            }
            Self::Unknown => None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

impl WireResponse {
    fn into_model_response(self) -> ModelResponse {
        let blocks = self
            .content
            .into_iter()
            .filter_map(|block| match block {
                WireBlock::Text { text } => {
                    let text = crate::strip_hidden_reasoning(&text);
                    (!text.is_empty()).then_some(ResponseBlock::Text { text })
                }
                WireBlock::ToolUse { id, name, input } => {
                    Some(ResponseBlock::ToolUse { id, name, input })
                }
                WireBlock::Thinking {
                    thinking,
                    signature,
                } => {
                    let _ = (thinking, signature);
                    None
                }
                WireBlock::Unknown => None,
            })
            .collect();
        ModelResponse {
            response_id: self.id,
            stop_reason: self.stop_reason,
            blocks,
            usage: Usage {
                input_tokens: self.usage.input_tokens,
                output_tokens: self.usage.output_tokens,
                cache_read_input_tokens: self.usage.cache_read_input_tokens,
                cache_creation_input_tokens: self.usage.cache_creation_input_tokens,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, Role};

    fn empty_request() -> ModelRequest {
        ModelRequest {
            system: "system".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_tokens: 100,
            temperature_milli: 0,
        }
    }

    fn test_provider() -> MiniMaxProvider {
        MiniMaxProvider {
            blocking_client: shared_blocking_http_client().expect("blocking client"),
            async_client: shared_async_http_client().expect("async client"),
            base_url: "https://example.invalid".to_owned(),
            api_key: "test".to_owned(),
            model: "test-model".to_owned(),
            capabilities: anthropic_capabilities(),
            wire_history: Arc::default(),
        }
    }

    #[test]
    fn request_marks_stable_system_and_tool_prefix_for_native_caching() {
        let provider = test_provider();
        let mut request = empty_request();
        request.tools.push(crate::ToolDefinition {
            name: "fs_read".to_owned(),
            description: "read".to_owned(),
            input_schema: json!({"type": "object"}),
        });
        let body = provider.request_body(&request);
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn thinking_is_not_exposed_or_persisted() {
        let provider = test_provider();
        let wire: WireResponse = serde_json::from_value(json!({
            "id": "msg-1",
            "stop_reason": "end_turn",
            "content": [
                {
                    "type": "thinking",
                    "thinking": "private chain",
                    "signature": "private signature"
                },
                {"type": "text", "text": "concise result"}
            ],
            "usage": {"input_tokens": 10, "output_tokens": 4}
        }))
        .expect("wire response");
        let response = provider.model_response_from_wire(wire);
        assert_eq!(
            response.blocks,
            vec![ResponseBlock::Text {
                text: "concise result".into()
            }]
        );
        let public = serde_json::to_string(&response).expect("serialize public response");
        assert!(!public.contains("private chain"));
        assert!(!public.contains("private signature"));
    }

    #[test]
    fn thinking_and_signature_are_replayed_only_in_provider_wire_history() {
        let provider = test_provider();
        let wire: WireResponse = serde_json::from_value(json!({
            "id": "msg-2",
            "stop_reason": "tool_use",
            "content": [
                {
                    "type": "thinking",
                    "thinking": "opaque reasoning",
                    "signature": "opaque signature"
                },
                {"type": "text", "text": "I will inspect the file."},
                {
                    "type": "tool_use",
                    "id": "tool-1",
                    "name": "fs_read",
                    "input": {"path": "src/lib.rs"}
                }
            ],
            "usage": {"input_tokens": 20, "output_tokens": 8}
        }))
        .expect("wire response");
        let response = provider.model_response_from_wire(wire);
        assert_eq!(
            response.blocks,
            vec![
                ResponseBlock::Text {
                    text: "I will inspect the file.".into()
                },
                ResponseBlock::ToolUse {
                    id: "tool-1".into(),
                    name: "fs_read".into(),
                    input: json!({"path": "src/lib.rs"})
                }
            ]
        );
        let public = serde_json::to_string(&response).expect("serialize public response");
        assert!(!public.contains("opaque reasoning"));
        assert!(!public.contains("opaque signature"));

        let mut request = empty_request();
        request.messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![
                    MessageBlock::Text {
                        text: "I will inspect the file.".into(),
                    },
                    MessageBlock::ToolUse {
                        id: "tool-1".into(),
                        name: "fs_read".into(),
                        input: json!({"path": "src/lib.rs"}),
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![MessageBlock::ToolResult {
                    tool_use_id: "tool-1".into(),
                    content: "file contents".into(),
                    is_error: false,
                }],
            },
        ];
        let body = provider.clone().request_body(&request);
        assert_eq!(
            body["messages"][0]["content"],
            json!([
                {
                    "type": "thinking",
                    "thinking": "opaque reasoning",
                    "signature": "opaque signature"
                },
                {"type": "text", "text": "I will inspect the file."},
                {
                    "type": "tool_use",
                    "id": "tool-1",
                    "name": "fs_read",
                    "input": {"path": "src/lib.rs"}
                }
            ])
        );
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_result");
    }

    #[test]
    fn configured_streaming_never_exceeds_wire_support() {
        let mut config = Config::default();
        config.model.provider = "anthropic".to_owned();
        config.model.protocol = "anthropic".to_owned();
        config.model.streaming = true;
        let provider =
            MiniMaxProvider::from_config_with_api_key(&config, Some("session-key".to_owned()))
                .expect("anthropic provider");
        assert!(!provider.capabilities().streaming);
        assert_eq!(
            provider.request_body(&empty_request())["stream"],
            Value::Bool(false)
        );
    }
}
