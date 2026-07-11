//! Provider-neutral model contracts and the MiniMax Anthropic-compatible adapter.

use std::{env, thread, time::Duration};

use medusa_config::Config;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{StatusCode, blocking::Client};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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

/// Provider-neutral message content.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
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
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
}

/// Usage accounting returned by the provider.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
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
}

/// MiniMax-M3 adapter using the Anthropic-compatible Messages API.
pub struct MiniMaxProvider {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    max_retries: u8,
}

impl MiniMaxProvider {
    /// Builds an adapter from typed model configuration and provider environment variables.
    pub fn from_config(config: &Config) -> MedusaResult<Self> {
        let api_key = env::var("MINIMAX_API_KEY").map_err(|_| {
            MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Environment,
                "missing provider credential in MINIMAX_API_KEY",
            )
        })?;
        let base_url = env::var("MINIMAX_BASE_URL")
            .unwrap_or_else(|_| "https://api.minimax.io/anthropic".into());
        let client = Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(provider_error)?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
            model: config.model.name.clone(),
            max_retries: 5,
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
}

impl ModelProvider for MiniMaxProvider {
    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        let endpoint = format!("{}/v1/messages", self.base_url);
        let body = self.request_body(request);
        let mut attempt = 0_u8;
        loop {
            let response = self
                .client
                .post(&endpoint)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send();
            match response {
                Ok(response) if response.status().is_success() => {
                    let wire: WireResponse = response.json().map_err(provider_error)?;
                    return Ok(wire.into_model_response());
                }
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().unwrap_or_default();
                    let error = classify_status(status, text);
                    if !error.retryable || attempt >= self.max_retries {
                        return Err(error);
                    }
                }
                Err(error) => {
                    if attempt >= self.max_retries {
                        return Err(provider_error(error));
                    }
                }
            }
            attempt = attempt.saturating_add(1);
            thread::sleep(Duration::from_millis(250 * u64::from(attempt)));
        }
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
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
    Thinking { #[serde(default)] thinking: String },
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

fn classify_status(status: StatusCode, body: String) -> MedusaError {
    let retryable = status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
    let category = if retryable {
        ErrorCategory::Transient
    } else if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        ErrorCategory::Policy
    } else {
        ErrorCategory::Validation
    };
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        category,
        format!("provider returned HTTP {status}: {body}"),
    )
    .with_retryable(retryable)
}

fn provider_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        format!("provider request failed: {error}"),
    )
    .with_retryable(true)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let response = wire.into_model_response();
        assert_eq!(
            response.blocks,
            vec![ResponseBlock::Text {
                text: "concise result".into()
            }]
        );
    }

    #[test]
    fn rate_limit_is_retryable() {
        assert!(classify_status(StatusCode::TOO_MANY_REQUESTS, "slow down".into()).retryable);
    }
}
