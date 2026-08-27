use std::{env, net::IpAddr, sync::atomic::AtomicBool};

use medusa_config::{Config, model_capabilities};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{Client as AsyncClient, Url, blocking::Client as BlockingClient};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ImageSource, MessageBlock, ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities,
    ProviderStreamEvent, ResponseBlock, Role, Usage, async_response_error, blocking_response_error,
    openai_transport, provider_error, provider_response_error, run_cancellable_request,
    shared_async_http_client, shared_blocking_http_client,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EndpointSource {
    RepositoryConfig,
    ProviderEnvironment,
    OpenAiEnvironment,
    MedusaEnvironment,
    Default,
}

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
        let (base_url, endpoint_source) = resolve_base_url(config, &provider);
        validate_provider_endpoint(&provider, &base_url)?;

        let provider_key_name = format!("{provider}_API_KEY");
        let provider_key_allowed =
            provider_credential_allowed(&provider, endpoint_source, &base_url);
        let api_key = (provider != "OPENAI_OAUTH")
            .then(|| {
                session_api_key
                    .or_else(|| {
                        provider_key_allowed
                            .then(|| env::var(&provider_key_name).ok())
                            .flatten()
                    })
                    .or_else(|| {
                        generic_openai_credential_allowed(&provider, endpoint_source, &base_url)
                            .then(|| env::var("OPENAI_API_KEY").ok())
                            .flatten()
                    })
                    .or_else(|| {
                        generic_medusa_credential_allowed(&provider, endpoint_source)
                            .then(|| env::var("MEDUSA_API_KEY").ok())
                            .flatten()
                    })
            })
            .flatten();
        if config.model.auth == "api-key" && api_key.is_none() {
            let mut message = format!("missing provider credential; set {provider_key_name}");
            if endpoint_source == EndpointSource::RepositoryConfig {
                message.push_str(
                    "; repository-configured endpoints cannot inherit ambient credentials unless the endpoint is the provider's canonical origin; use an explicit session credential or a user-level provider endpoint setting",
                );
            }
            return Err(MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Environment,
                message,
            ));
        }

        let registry_capabilities = model_capabilities(&config.model.provider, &config.model.name);
        let image_input = registry_capabilities.image_input;
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
                streaming: config.model.streaming,
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

    fn request_body(&self, request: &ModelRequest, streaming: bool) -> MedusaResult<Value> {
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
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "stream": streaming
        });
        if uses_reasoning_chat_parameters(&self.model) {
            body["max_completion_tokens"] = json!(request.max_tokens);
        } else {
            body["max_tokens"] = json!(request.max_tokens);
            body["temperature"] = json!(f64::from(request.temperature_milli) / 1000.0);
        }
        if streaming {
            body["stream_options"] = json!({"include_usage": true});
        }
        Ok(body)
    }

    fn complete_request(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.validate_request(request)?;
        let endpoint = format!("{}/chat/completions", self.base_url);
        let mut builder = self
            .blocking_client
            .post(&endpoint)
            .json(&self.request_body(request, false)?);
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
            .json(&self.request_body(request, false)?);
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

fn uses_reasoning_chat_parameters(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    model.starts_with("gpt-5")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
}

impl ModelProvider for OpenAiProvider {
    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.complete_request(request)
    }

    fn complete_streaming(
        &self,
        request: &ModelRequest,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        self.validate_request(request)?;
        let endpoint = format!("{}/chat/completions", self.base_url);
        openai_transport::complete_blocking(
            &self.blocking_client,
            &endpoint,
            self.api_key.as_deref(),
            self.request_body(request, true)?,
            sink,
        )
    }

    fn complete_streaming_cancellable(
        &self,
        request: &ModelRequest,
        cancel: &AtomicBool,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        self.validate_request(request)?;
        let endpoint = format!("{}/chat/completions", self.base_url);
        openai_transport::complete_cancellable(
            &self.async_client,
            &endpoint,
            self.api_key.as_deref(),
            self.request_body(request, true)?,
            cancel,
            sink,
        )
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

fn resolve_base_url(config: &Config, provider: &str) -> (String, EndpointSource) {
    if let Some(base_url) = config.model.base_url.clone() {
        return (base_url, EndpointSource::RepositoryConfig);
    }
    if let Ok(base_url) = env::var(format!("{provider}_BASE_URL")) {
        return (base_url, EndpointSource::ProviderEnvironment);
    }
    if provider == "OPENAI" {
        if let Ok(base_url) = env::var("OPENAI_BASE_URL") {
            return (base_url, EndpointSource::OpenAiEnvironment);
        }
    }
    if provider == "MEDUSA" {
        if let Ok(base_url) = env::var("MEDUSA_BASE_URL") {
            return (base_url, EndpointSource::MedusaEnvironment);
        }
    }
    let base_url = if provider == "MINIMAX" {
        "https://api.minimax.io/v1"
    } else {
        "https://api.openai.com/v1"
    };
    (base_url.to_owned(), EndpointSource::Default)
}

fn provider_credential_allowed(provider: &str, source: EndpointSource, base_url: &str) -> bool {
    if source != EndpointSource::RepositoryConfig {
        return true;
    }
    match provider {
        "OPENAI" => is_canonical_openai_endpoint(base_url),
        "MINIMAX" => is_canonical_minimax_endpoint(base_url),
        _ => false,
    }
}

fn generic_openai_credential_allowed(
    provider: &str,
    source: EndpointSource,
    base_url: &str,
) -> bool {
    provider == "OPENAI"
        && (source != EndpointSource::RepositoryConfig || is_canonical_openai_endpoint(base_url))
}

fn generic_medusa_credential_allowed(provider: &str, source: EndpointSource) -> bool {
    provider == "MEDUSA" && source != EndpointSource::RepositoryConfig
}

fn is_canonical_openai_endpoint(base_url: &str) -> bool {
    canonical_https_origin(base_url, "api.openai.com")
}

fn is_canonical_minimax_endpoint(base_url: &str) -> bool {
    canonical_https_origin(base_url, "api.minimax.io")
}

fn canonical_https_origin(base_url: &str, expected_host: &str) -> bool {
    Url::parse(base_url).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some(expected_host)
            && url.port_or_known_default() == Some(443)
    })
}

fn validate_provider_endpoint(provider: &str, base_url: &str) -> MedusaResult<()> {
    let allow_insecure_loopback = env_flag("MEDUSA_ALLOW_INSECURE_PROVIDER_HTTP");
    validate_provider_endpoint_for_provider(provider, base_url, allow_insecure_loopback)
}

fn validate_provider_endpoint_for_provider(
    _provider: &str,
    base_url: &str,
    allow_insecure_loopback: bool,
) -> MedusaResult<()> {
    let url = parse_provider_endpoint(base_url)?;
    validate_parsed_provider_endpoint(url, allow_insecure_loopback)
}

#[cfg(test)]
fn validate_provider_endpoint_with_policy(
    base_url: &str,
    allow_insecure_loopback: bool,
) -> MedusaResult<()> {
    let url = parse_provider_endpoint(base_url)?;
    validate_parsed_provider_endpoint(url, allow_insecure_loopback)
}

fn parse_provider_endpoint(base_url: &str) -> MedusaResult<Url> {
    let url = Url::parse(base_url).map_err(|error| {
        MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Validation,
            format!("invalid provider base_url: {error}"),
        )
    })?;
    if url.username() != "" || url.password().is_some() {
        return Err(MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Validation,
            "provider base_url must not contain embedded credentials",
        ));
    }
    Ok(url)
}

fn validate_parsed_provider_endpoint(url: Url, allow_insecure_loopback: bool) -> MedusaResult<()> {
    if url.scheme() == "https" {
        return Ok(());
    }
    if url.scheme() == "http" && is_loopback_url(&url) && allow_insecure_loopback {
        return Ok(());
    }
    Err(MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Validation,
        "provider base_url must use HTTPS; loopback HTTP requires MEDUSA_ALLOW_INSECURE_PROVIDER_HTTP=1",
    ))
}

fn is_loopback_url(url: &Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
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
    #[serde(default)]
    prompt_tokens_details: OpenAiPromptTokenDetails,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAiPromptTokenDetails {
    #[serde(default)]
    cached_tokens: u64,
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
            let text = crate::strip_hidden_reasoning(&text);
            if !text.is_empty() {
                blocks.push(ResponseBlock::Text { text });
            }
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
                cache_read_input_tokens: self.usage.prompt_tokens_details.cached_tokens,
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
    fn non_streaming_usage_preserves_openai_cached_prompt_tokens() {
        let wire: OpenAiWireResponse = serde_json::from_value(json!({
            "id": "response-1",
            "choices": [{
                "message": {"content": "ok", "tool_calls": []},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 4,
                "prompt_tokens_details": {"cached_tokens": 90}
            }
        }))
        .expect("wire response");
        let response = wire.into_model_response().expect("model response");
        assert_eq!(response.usage.input_tokens, 100);
        assert_eq!(response.usage.cache_read_input_tokens, 90);
    }

    #[test]
    fn configured_streaming_never_exceeds_wire_support() {
        let mut config = Config::default();
        config.model.provider = "openai".to_owned();
        config.model.protocol = "openai".to_owned();
        config.model.streaming = true;
        let provider =
            OpenAiProvider::from_config_with_api_key(&config, Some("session-key".to_owned()))
                .expect("openai provider");
        assert!(provider.capabilities().streaming);
        let body = provider
            .request_body(&empty_request(), true)
            .expect("request body");
        assert_eq!(body["stream"], Value::Bool(true));
        assert_eq!(body["stream_options"]["include_usage"], Value::Bool(true));
    }

    #[test]
    fn serializes_base64_images_as_image_url_parts() {
        let body = test_provider(true)
            .request_body(
                &request_with_image(ImageSource::Base64 {
                    media_type: "image/png".to_owned(),
                    data: "AAEC".to_owned(),
                }),
                false,
            )
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
            .request_body(
                &request_with_image(ImageSource::Base64 {
                    media_type: "image/png".to_owned(),
                    data: "AAEC".to_owned(),
                }),
                false,
            )
            .expect_err("reject image");
        assert_eq!(error.context["content_type"], "image");
    }

    #[test]
    fn rejects_unresolved_attachment_references() {
        let error = test_provider(true)
            .request_body(
                &request_with_image(ImageSource::AttachmentRef {
                    attachment_id: "attachment-1".to_owned(),
                }),
                false,
            )
            .expect_err("reject unresolved reference");
        assert!(error.to_string().contains("attachment-1"));
    }

    #[test]
    fn repository_openai_override_cannot_inherit_openai_key() {
        assert!(!provider_credential_allowed(
            "OPENAI",
            EndpointSource::RepositoryConfig,
            "https://attacker.example/v1",
        ));
        assert!(!generic_openai_credential_allowed(
            "OPENAI",
            EndpointSource::RepositoryConfig,
            "https://attacker.example/v1",
        ));
    }

    #[test]
    fn reasoning_models_use_completion_limit_and_omit_temperature() {
        for model in ["gpt-5", "gpt-5-mini", "o1", "o3-mini", "o4-mini"] {
            let mut provider = test_provider(true);
            provider.model = model.to_owned();
            let body = provider
                .request_body(&empty_request(), false)
                .expect("request body");
            assert_eq!(body["max_completion_tokens"], 100, "{model}");
            assert!(body.get("max_tokens").is_none(), "{model}");
            assert!(body.get("temperature").is_none(), "{model}");
        }
        let mut provider = test_provider(true);
        provider.model = "gpt-4.1".to_owned();
        let body = provider
            .request_body(&empty_request(), false)
            .expect("request body");
        assert_eq!(body["max_tokens"], 100);
        assert_eq!(body["temperature"], 0.0);
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn canonical_provider_origins_can_use_provider_keys() {
        assert!(provider_credential_allowed(
            "OPENAI",
            EndpointSource::RepositoryConfig,
            "https://api.openai.com/v1",
        ));
        assert!(provider_credential_allowed(
            "MINIMAX",
            EndpointSource::RepositoryConfig,
            "https://api.minimax.io/v1",
        ));
    }

    #[test]
    fn arbitrary_provider_does_not_receive_ambient_credentials() {
        assert!(!provider_credential_allowed(
            "EVIL",
            EndpointSource::RepositoryConfig,
            "https://attacker.example/v1",
        ));
        assert!(!generic_openai_credential_allowed(
            "EVIL",
            EndpointSource::RepositoryConfig,
            "https://attacker.example/v1",
        ));
        assert!(!generic_medusa_credential_allowed(
            "EVIL",
            EndpointSource::RepositoryConfig,
        ));
    }

    #[test]
    fn user_level_provider_endpoint_can_authorize_provider_specific_key() {
        assert!(provider_credential_allowed(
            "CUSTOM",
            EndpointSource::ProviderEnvironment,
            "https://provider.example/v1",
        ));
    }

    #[test]
    fn remote_http_is_rejected() {
        let error = validate_provider_endpoint_with_policy("http://example.com/v1", true)
            .expect_err("remote HTTP must fail");
        assert!(error.to_string().contains("HTTPS"));
    }

    #[test]
    fn loopback_http_requires_explicit_opt_in() {
        assert!(validate_provider_endpoint_with_policy("http://127.0.0.1:8080/v1", false).is_err());
        validate_provider_endpoint_with_policy("http://127.0.0.1:8080/v1", true)
            .expect("explicit loopback development opt-in");
    }

    #[test]
    fn chatgpt_oauth_does_not_use_a_loopback_gateway() {
        let mut config = Config::default();
        config.model.provider = "openai-oauth".to_owned();
        config.model.protocol = "openai".to_owned();
        config.model.auth = "none".to_owned();
        config.model.base_url = None;
        let provider = OpenAiProvider::from_config_with_api_key(
            &config,
            Some("api-key-must-not-enter-oauth-route".to_owned()),
        )
        .expect("the direct app-server route does not construct an HTTP gateway");
        assert!(
            provider.api_key.is_none(),
            "ChatGPT OAuth must rely on app-server authentication, not an API key"
        );
        assert!(
            validate_provider_endpoint_for_provider(
                "OPENAI_OAUTH",
                "http://127.0.0.1:10531/v1",
                false,
            )
            .is_err()
        );
    }

    #[test]
    fn embedded_endpoint_credentials_are_rejected() {
        let error =
            validate_provider_endpoint_with_policy("https://user:password@example.com/v1", false)
                .expect_err("embedded credentials must fail");
        assert!(error.to_string().contains("embedded credentials"));
    }
}
