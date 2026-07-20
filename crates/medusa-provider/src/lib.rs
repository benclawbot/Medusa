//! Provider-neutral model contracts and the MiniMax Anthropic-compatible adapter.

mod manager;
use std::{env, sync::OnceLock, thread, time::Duration};

use medusa_config::Config;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{StatusCode, blocking::Client};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use manager::{ProviderHealth, ProviderManager};
const PROVIDER_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_PROVIDER_RETRIES: u8 = 2;

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

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
}

/// Anthropic Messages API adapter for MiniMax, Anthropic, and compatible providers.
pub struct MiniMaxProvider {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    max_retries: u8,
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
        let base_url = env::var(settings.base_url_env)
            .unwrap_or_else(|_| settings.default_base_url.to_owned());
        let client = shared_http_client()?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
            model: config.model.name.clone(),
            max_retries: MAX_PROVIDER_RETRIES,
            capabilities: (settings.capabilities)(),
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
}

fn shared_http_client() -> MedusaResult<Client> {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(PROVIDER_REQUEST_TIMEOUT)
        .tcp_nodelay(true)
        .pool_max_idle_per_host(8)
        .build()
        .map_err(provider_error)?;
    let _ = CLIENT.set(client.clone());
    Ok(CLIENT.get().cloned().unwrap_or(client))
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

impl ModelProvider for MiniMaxProvider {
    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.validate_request(request)?;
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

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
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
        }
    } else {
        ProviderCapabilities::default()
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

fn provider_error(error: reqwest::Error) -> MedusaError {
    let message = if error.is_connect() {
        let endpoint = error
            .url()
            .map_or_else(|| "the configured endpoint".to_owned(), ToString::to_string);
        format!(
            "provider endpoint is unavailable at {endpoint}; start the local or gateway service, configure a reachable provider with `medusa config`, or configure model.fallback_providers: {error}"
        )
    } else {
        format!("provider request failed: {error}")
    };
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Transient,
        message,
    )
    .with_retryable(true)
}
fn provider_response_error(error: impl std::fmt::Display) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Validation,
        format!("provider returned an invalid response: {error}"),
    )
}
/// Runtime-selected provider supporting Anthropic and OpenAI-compatible APIs.
pub enum ConfiguredProvider {
    Anthropic(MiniMaxProvider),
    OpenAi(OpenAiProvider),
}

impl ConfiguredProvider {
    pub fn from_config(config: &Config) -> MedusaResult<Self> {
        Self::from_config_with_api_key(config, None)
    }

    pub fn from_config_with_api_key(
        config: &Config,
        session_api_key: Option<String>,
    ) -> MedusaResult<Self> {
        if config.model.protocol.eq_ignore_ascii_case("openai") {
            Ok(Self::OpenAi(OpenAiProvider::from_config_with_api_key(
                config,
                session_api_key,
            )?))
        } else {
            Ok(Self::Anthropic(MiniMaxProvider::from_config_with_api_key(
                config,
                session_api_key,
            )?))
        }
    }

    /// Builds the configured primary provider plus ordered fallback providers.
    pub fn manager_from_config(
        config: &Config,
        session_api_key: Option<String>,
    ) -> MedusaResult<ProviderManager<Self>> {
        let mut providers = vec![Self::from_config_with_api_key(
            config,
            session_api_key.clone(),
        )?];
        for fallback in &config.model.fallback_providers {
            if fallback.eq_ignore_ascii_case(&config.model.provider) {
                continue;
            }
            let mut fallback_config = config.clone();
            fallback_config.model.provider = fallback.clone();
            providers.push(Self::from_config_with_api_key(
                &fallback_config,
                session_api_key.clone(),
            )?);
        }
        Ok(ProviderManager::new(providers))
    }
}

impl ModelProvider for ConfiguredProvider {
    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        match self {
            Self::Anthropic(provider) => provider.complete(request),
            Self::OpenAi(provider) => provider.complete(request),
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        match self {
            Self::Anthropic(provider) => provider.capabilities(),
            Self::OpenAi(provider) => provider.capabilities(),
        }
    }
}

pub struct OpenAiProvider {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
    max_retries: u8,
}

impl OpenAiProvider {
    pub fn from_config_with_api_key(
        config: &Config,
        session_api_key: Option<String>,
    ) -> MedusaResult<Self> {
        let provider = config
            .model
            .provider
            .trim()
            .to_ascii_uppercase()
            .replace('-', "_");
        let api_key = session_api_key
            .or_else(|| env::var(format!("{provider}_API_KEY")).ok())
            .or_else(|| env::var("OPENAI_API_KEY").ok())
            .or_else(|| env::var("MEDUSA_API_KEY").ok());
        if config.model.auth == "api-key" && api_key.is_none() {
            return Err(MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Environment,
                format!(
                    "missing provider credential; set {provider}_API_KEY, OPENAI_API_KEY, or MEDUSA_API_KEY"
                ),
            ));
        }
        let base_url = config
            .model
            .base_url
            .clone()
            .or_else(|| env::var(format!("{provider}_BASE_URL")).ok())
            .or_else(|| env::var("OPENAI_BASE_URL").ok())
            .or_else(|| env::var("MEDUSA_BASE_URL").ok())
            .unwrap_or_else(|| "https://api.openai.com/v1".to_owned());
        Ok(Self {
            client: shared_http_client()?,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
            model: config.model.name.clone(),
            max_retries: MAX_PROVIDER_RETRIES,
        })
    }

    fn request_body(&self, request: &ModelRequest) -> Value {
        let mut messages = vec![json!({"role": "system", "content": request.system})];
        for message in &request.messages {
            let role = match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            let mut text = String::new();
            let mut tool_calls = Vec::new();
            for block in &message.content {
                match block {
                    MessageBlock::Text { text: value } => text.push_str(value),
                    MessageBlock::ToolUse { id, name, input } => tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": input.to_string()}
                    })),
                    MessageBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => messages.push(json!({
                        "role": "tool", "tool_call_id": tool_use_id, "content": content
                    })),
                    MessageBlock::Image { .. } => {}
                }
            }
            let mut wire = json!({"role": role, "content": text});
            if !tool_calls.is_empty() {
                wire["tool_calls"] = Value::Array(tool_calls);
            }
            messages.push(wire);
        }
        let tools: Vec<Value> = request.tools.iter().map(|tool| json!({
            "type": "function",
            "function": {"name": tool.name, "description": tool.description, "parameters": tool.input_schema}
        })).collect();
        json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "max_tokens": request.max_tokens,
            "temperature": f64::from(request.temperature_milli) / 1000.0,
            "stream": false
        })
    }
}

impl ModelProvider for OpenAiProvider {
    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        let endpoint = format!("{}/chat/completions", self.base_url);
        let body = self.request_body(request);
        let mut attempt = 0_u8;
        loop {
            let mut builder = self.client.post(&endpoint).json(&body);
            if let Some(key) = &self.api_key {
                builder = builder.bearer_auth(key);
            }
            match builder.send() {
                Ok(response) if response.status().is_success() => {
                    let wire: OpenAiWireResponse = response.json().map_err(provider_error)?;
                    return wire.into_model_response();
                }
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().unwrap_or_default();
                    let error = classify_status(status, text);
                    if !error.retryable || attempt >= self.max_retries {
                        return Err(error);
                    }
                }
                Err(error) if attempt >= self.max_retries => return Err(provider_error(error)),
                Err(_) => {}
            }
            attempt = attempt.saturating_add(1);
            thread::sleep(Duration::from_millis(250 * u64::from(attempt)));
        }
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiWireResponse {
    id: Option<String>,
    #[serde(default)]
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: OpenAiUsage,
}
#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
    finish_reason: Option<String>,
}
#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<OpenAiToolCall>,
}
#[derive(Debug, Deserialize)]
struct OpenAiToolCall {
    id: String,
    function: OpenAiFunction,
}
#[derive(Debug, Deserialize)]
struct OpenAiFunction {
    name: String,
    arguments: String,
}
#[derive(Debug, Default, Deserialize)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}
impl OpenAiWireResponse {
    fn into_model_response(self) -> MedusaResult<ModelResponse> {
        let choice = self.choices.into_iter().next().ok_or_else(|| {
            MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Execution,
                "provider returned no choices",
            )
        })?;
        let mut blocks = Vec::new();
        if let Some(text) = choice.message.content.filter(|value| !value.is_empty()) {
            blocks.push(ResponseBlock::Text { text });
        }
        for call in choice.message.tool_calls {
            let input =
                serde_json::from_str(&call.function.arguments).map_err(provider_response_error)?;
            blocks.push(ResponseBlock::ToolUse {
                id: call.id,
                name: call.function.name,
                input,
            });
        }
        Ok(ModelResponse {
            response_id: self.id,
            stop_reason: choice.finish_reason,
            blocks,
            usage: Usage {
                input_tokens: self.usage.prompt_tokens,
                output_tokens: self.usage.completion_tokens,
                ..Usage::default()
            },
        })
    }
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

    #[test]
    fn default_provider_capabilities_reject_images_by_contract() {
        struct TextOnly;
        impl ModelProvider for TextOnly {
            fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
                unreachable!("not called")
            }
        }
        assert!(!TextOnly.capabilities().image_input);
    }

    #[test]
    fn rate_limit_is_retryable() {
        assert!(classify_status(StatusCode::TOO_MANY_REQUESTS, "slow down".into()).retryable);
    }

    #[test]
    fn session_credentials_support_provider_selection_without_environment_mutation() {
        let mut config = Config::default();
        config.model.provider = "anthropic".to_owned();
        config.model.name = "claude-sonnet-test".to_owned();
        let provider =
            MiniMaxProvider::from_config_with_api_key(&config, Some("session-key".to_owned()))
                .expect("build provider from session key");
        assert!(provider.capabilities().image_input);
        assert!(provider_settings("anthropic-compatible").is_ok());
        assert!(provider_settings("unsupported").is_err());
    }
}
