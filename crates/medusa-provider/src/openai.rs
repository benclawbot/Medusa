use std::{env, sync::atomic::AtomicBool};

use medusa_config::Config;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{Client as AsyncClient, blocking::Client as BlockingClient};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ImageSource, MessageBlock, ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities,
    ResponseBlock, Role, Usage, async_response_error, blocking_response_error, provider_error,
    provider_response_error, run_cancellable_request, shared_async_http_client,
    shared_blocking_http_client,
};

#[derive(Clone)]
pub struct OpenAiProvider {
    blocking_client: BlockingClient,
    async_client: AsyncClient,
    base_url: String,
    api_key: Option<String>,
    model: String,
    capabilities: ProviderCapabilities,
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
            .unwrap_or_else(|| {
                if config.model.provider.eq_ignore_ascii_case("minimax") {
                    "https://api.minimax.io/v1".to_owned()
                } else {
                    "https://api.openai.com/v1".to_owned()
                }
            });
        let image_input = config.model.provider.eq_ignore_ascii_case("openai")
            || config.model.auth.eq_ignore_ascii_case("chatgpt-oauth");
        Ok(Self {
            blocking_client: shared_blocking_http_client()?,
            async_client: shared_async_http_client()?,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
            model: config.model.name.clone(),
            capabilities: ProviderCapabilities {
                image_input,
                supported_image_media_types: if image_input {
                    vec![
                        "image/png".to_owned(),
                        "image/jpeg".to_owned(),
                        "image/webp".to_owned(),
                        "image/gif".to_owned(),
                    ]
                } else {
                    Vec::new()
                },
                max_image_bytes: image_input.then_some(20 * 1024 * 1024),
                max_images_per_request: image_input.then_some(10),
                tool_calling: config.model.tool_calling,
                streaming: false,
            },
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
        Ok(())
    }

    fn request_body(&self, request: &ModelRequest) -> MedusaResult<Value> {
        let mut messages = vec![json!({"role": "system", "content": request.system})];
        for message in &request.messages {
            let role = match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            let mut text = String::new();
            let mut content_parts = Vec::new();
            let mut tool_calls = Vec::new();
            for block in &message.content {
                match block {
                    MessageBlock::Text { text: value } => {
                        text.push_str(value);
                        content_parts.push(json!({"type": "text", "text": value}));
                    }
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
                    MessageBlock::Image { source, .. } => match source {
                        ImageSource::Base64 { media_type, data } => {
                            if !self.capabilities.image_input {
                                return Err(openai_image_error(
                                    &self.model,
                                    "selected OpenAI route does not support image input",
                                ));
                            }
                            if !self.capabilities.supported_image_media_types.is_empty()
                                && !self
                                    .capabilities
                                    .supported_image_media_types
                                    .iter()
                                    .any(|supported| supported == media_type)
                            {
                                return Err(openai_image_error(
                                    &self.model,
                                    format!("unsupported image media type {media_type}"),
                                ));
                            }
                            content_parts.push(json!({
                                "type": "image_url",
                                "image_url": {"url": format!("data:{media_type};base64,{data}")}
                            }));
                        }
                        ImageSource::AttachmentRef { attachment_id } => {
                            return Err(openai_image_error(
                                &self.model,
                                format!("unresolved image attachment reference {attachment_id}"),
                            ));
                        }
                    },
                }
            }
            let has_content = !text.is_empty() || !content_parts.is_empty();
            let content = if content_parts
                .iter()
                .any(|part| part["type"] == Value::String("image_url".to_owned()))
            {
                Value::Array(content_parts)
            } else {
                Value::String(text)
            };
            if has_content || !tool_calls.is_empty() {
                let mut wire = json!({"role": role, "content": content});
                if !tool_calls.is_empty() {
                    wire["tool_calls"] = Value::Array(tool_calls);
                }
                messages.push(wire);
            }
        }
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema
                    }
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "max_tokens": request.max_tokens,
            "temperature": f64::from(request.temperature_milli) / 1000.0,
            "stream": false
        }))
    }

    fn complete_request(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.validate_request(request)?;
        let endpoint = format!("{}/chat/completions", self.base_url);
        let mut builder = self
            .blocking_client
            .post(&endpoint)
            .json(&self.request_body(request)?);
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }
        let response = builder.send().map_err(provider_error)?;
        if response.status().is_success() {
            let wire: OpenAiWireResponse = response.json().map_err(provider_error)?;
            return wire.into_model_response();
        }
        Err(blocking_response_error(response))
    }

    async fn complete_request_async(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.validate_request(request)?;
        let endpoint = format!("{}/chat/completions", self.base_url);
        let mut builder = self
            .async_client
            .post(&endpoint)
            .json(&self.request_body(request)?);
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }
        let response = builder.send().await.map_err(provider_error)?;
        if response.status().is_success() {
            let wire: OpenAiWireResponse = response.json().await.map_err(provider_error)?;
            return wire.into_model_response();
        }
        Err(async_response_error(response).await)
    }
}

impl ModelProvider for OpenAiProvider {
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

fn openai_image_error(model: &str, message: impl Into<String>) -> MedusaError {
    let mut error = MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Validation,
        message.into(),
    );
    error
        .context
        .insert("provider".to_owned(), Value::from("openai"));
    error
        .context
        .insert("model".to_owned(), Value::from(model.to_owned()));
    error
        .context
        .insert("content_type".to_owned(), Value::from("image"));
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Message;

    fn test_provider(image_input: bool) -> OpenAiProvider {
        OpenAiProvider {
            blocking_client: shared_blocking_http_client().expect("blocking client"),
            async_client: shared_async_http_client().expect("async client"),
            base_url: "https://example.invalid/v1".to_owned(),
            api_key: None,
            model: "gpt-5".to_owned(),
            capabilities: ProviderCapabilities {
                image_input,
                supported_image_media_types: vec!["image/png".to_owned()],
                max_image_bytes: Some(20 * 1024 * 1024),
                max_images_per_request: Some(10),
                tool_calling: true,
                streaming: false,
            },
        }
    }

    fn empty_request() -> ModelRequest {
        ModelRequest {
            system: "system".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_tokens: 100,
            temperature_milli: 0,
        }
    }

    fn request_with_image(source: ImageSource) -> ModelRequest {
        ModelRequest {
            system: "system".to_owned(),
            messages: vec![Message {
                role: Role::User,
                content: vec![
                    MessageBlock::Text {
                        text: "inspect".to_owned(),
                    },
                    MessageBlock::Image {
                        source,
                        alt_text: Some("screenshot".to_owned()),
                    },
                ],
            }],
            tools: Vec::new(),
            max_tokens: 100,
            temperature_milli: 0,
        }
    }

    #[test]
    fn configured_streaming_never_exceeds_wire_support() {
        let mut config = Config::default();
        config.model.provider = "openai".to_owned();
        config.model.protocol = "openai".to_owned();
        config.model.streaming = true;
        let provider = OpenAiProvider::from_config_with_api_key(
            &config,
            Some("session-key".to_owned()),
        )
        .expect("openai provider");
        assert!(!provider.capabilities().streaming);
        assert_eq!(
            provider.request_body(&empty_request()).expect("request body")["stream"],
            Value::Bool(false)
        );
    }

    #[test]
    fn serializes_base64_images_as_image_url_parts() {
        let body = test_provider(true)
            .request_body(&request_with_image(ImageSource::Base64 {
                media_type: "image/png".to_owned(),
                data: "AAEC".to_owned(),
            }))
            .expect("request body");
        let content = body["messages"][1]["content"]
            .as_array()
            .expect("multimodal content array");
        assert_eq!(content[0], json!({"type": "text", "text": "inspect"}));
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AAEC");
    }

    #[test]
    fn rejects_images_when_route_is_text_only() {
        let error = test_provider(false)
            .request_body(&request_with_image(ImageSource::Base64 {
                media_type: "image/png".to_owned(),
                data: "AAEC".to_owned(),
            }))
            .expect_err("reject image");
        assert_eq!(error.context["content_type"], "image");
    }

    #[test]
    fn rejects_unresolved_attachment_references() {
        let error = test_provider(true)
            .request_body(&request_with_image(ImageSource::AttachmentRef {
                attachment_id: "attachment-1".to_owned(),
            }))
            .expect_err("reject unresolved reference");
        assert!(error.to_string().contains("attachment-1"));
    }
}