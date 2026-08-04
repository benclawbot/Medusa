use std::{env, sync::atomic::AtomicBool};

use medusa_config::Config;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{Client as AsyncClient, blocking::Client as BlockingClient};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    MessageBlock, ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities, ResponseBlock,
    Usage, async_response_error, blocking_response_error, provider_error, run_cancellable_request,
    shared_async_http_client, shared_blocking_http_client,
};

/// Anthropic Messages API adapter for MiniMax, Anthropic, and compatible providers.
#[derive(Clone)]
pub struct MiniMaxProvider {
    blocking_client: BlockingClient,
    async_client: AsyncClient,
    base_url: String,
    api_key: String,
    model: String,
    capabilities: ProviderCapabilities,
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
        capabilities.tool_calling = config.model.tool_calling;
        capabilities.streaming = false;
        Ok(Self {
            blocking_client: shared_blocking_http_client()?,
            async_client: shared_async_http_client()?,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
            model: config.model.name.clone(),
            capabilities,
        })
    }

    fn request_body(&self, request: &ModelRequest) -> Value {
        json!({
            "model": self.model,
            "system": request.system,
            "messages": request.messages,
            "tools": request.tools,
            "max_tokens": request.max_tokens,
            "temperature": f64::from(request.temperature_milli) / 1000.0,
            "stream": false
        })
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
            let wire: WireResponse = response.json().map_err(provider_error)?;
            return Ok(wire.into_model_response());
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
            let wire: WireResponse = response.json().await.map_err(provider_error)?;
            return Ok(wire.into_model_response());
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

#[derive(Debug, Deserialize)]
struct WireResponse {
    id: Option<String>,
    stop_reason: Option<String>,
    #[serde(default)]
    content: Vec<WireBlock>,
    #[serde(default)]
    usage: WireUsage,
}

#[derive(Debug, Deserialize)]
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
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Default, Deserialize)]
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
                WireBlock::Text { text } => Some(ResponseBlock::Text { text }),
                WireBlock::ToolUse { id, name, input } => {
                    Some(ResponseBlock::ToolUse { id, name, input })
                }
                WireBlock::Thinking { thinking } => {
                    let _ = thinking;
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

    fn empty_request() -> ModelRequest {
        ModelRequest {
            system: "system".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_tokens: 100,
            temperature_milli: 0,
        }
    }

    #[test]
    fn thinking_is_not_exposed_or_persisted() {
        let wire: WireResponse = serde_json::from_value(json!({
            "id": "msg-1",
            "stop_reason": "end_turn",
            "content": [
                {"type": "thinking", "thinking": "private chain"},
                {"type": "text", "text": "concise result"}
            ],
            "usage": {"input_tokens": 10, "output_tokens": 4}
        }))
        .expect("wire response");
        assert_eq!(
            wire.into_model_response().blocks,
            vec![ResponseBlock::Text {
                text: "concise result".into()
            }]
        );
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
