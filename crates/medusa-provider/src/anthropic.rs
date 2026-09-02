use std::{
    collections::HashMap,
    env,
    io::Read,
    sync::{Arc, Mutex, atomic::AtomicBool, mpsc},
    thread,
};

use medusa_config::{Config, model_capabilities};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{Client as AsyncClient, blocking::Client as BlockingClient};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    MessageBlock, ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities,
    ProviderStreamEvent, ResponseBlock, Usage, async_response_error, async_response_json,
    blocking_response_error, blocking_response_json, provider_error, run_cancellable_request,
    shared_async_http_client, shared_blocking_http_client, split_dynamic_system_context,
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
        self.request_body_with_stream(request, false)
    }

    fn request_body_with_stream(&self, request: &ModelRequest, stream: bool) -> Value {
        let mut tools = json!(request.tools);
        if let Some(last) = tools.as_array_mut().and_then(|items| items.last_mut())
            && let Some(object) = last.as_object_mut()
        {
            object.insert("cache_control".to_owned(), json!({"type": "ephemeral"}));
        }
        let (stable_system, dynamic_system) = split_dynamic_system_context(&request.system);
        let mut system = vec![json!({
            "type": "text",
            "text": stable_system,
            "cache_control": {"type": "ephemeral"}
        })];
        if let Some(dynamic_system) = dynamic_system {
            system.push(json!({"type": "text", "text": dynamic_system}));
        }
        json!({
            "model": self.model,
            "system": system,
            "messages": self.request_messages(request),
            "tools": tools,
            "max_tokens": request.max_tokens,
            "temperature": f64::from(request.temperature_milli) / 1000.0,
            // Match the harness's fast/simple path: reasoning is opt-in, so short conversational
            // turns can begin producing visible text without waiting for a hidden thinking pass.
            "thinking": {"type": "disabled"},
            "stream": stream
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

    fn complete_streaming_request(
        &self,
        request: &ModelRequest,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        self.validate_request(request)?;
        let endpoint = format!("{}/v1/messages", self.base_url);
        let mut response = self
            .blocking_client
            .post(&endpoint)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&self.request_body_with_stream(request, true))
            .send()
            .map_err(provider_error)?;
        if !response.status().is_success() {
            return Err(blocking_response_error(response));
        }
        let mut decoder = AnthropicSseDecoder::default();
        let mut accumulator = AnthropicStreamAccumulator::default();
        let mut completed = None;
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            let read = response.read(&mut buffer).map_err(|error| {
                anthropic_stream_error(format!("MiniMax SSE read failed: {error}"))
            })?;
            if read == 0 {
                break;
            }
            decoder.push(&buffer[..read], |data| {
                if let Some(wire) = accumulator.push_sse_data(data, sink)? {
                    let model = self.model_response_from_wire(wire);
                    sink(ProviderStreamEvent::Completed {
                        response: model.clone(),
                    })?;
                    completed = Some(model);
                }
                Ok(())
            })?;
        }
        decoder.finish(|data| {
            if let Some(wire) = accumulator.push_sse_data(data, sink)? {
                let model = self.model_response_from_wire(wire);
                sink(ProviderStreamEvent::Completed {
                    response: model.clone(),
                })?;
                completed = Some(model);
            }
            Ok(())
        })?;
        completed
            .ok_or_else(|| anthropic_stream_error("MiniMax SSE stream ended without message_stop"))
    }

    fn complete_streaming_cancellable_request(
        &self,
        request: &ModelRequest,
        cancel: &AtomicBool,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        self.validate_request(request)?;
        let (sender, receiver) = mpsc::channel::<ProviderStreamEvent>();
        let endpoint = format!("{}/v1/messages", self.base_url);
        let body = self.request_body_with_stream(request, true);
        thread::scope(|scope| {
            let worker_sender = sender.clone();
            let worker = scope.spawn(move || {
                run_cancellable_request(cancel, async {
                    let mut response = self
                        .async_client
                        .post(&endpoint)
                        .header("x-api-key", &self.api_key)
                        .header("anthropic-version", "2023-06-01")
                        .json(&body)
                        .send()
                        .await
                        .map_err(provider_error)?;
                    if !response.status().is_success() {
                        return Err(async_response_error(response).await);
                    }
                    let mut decoder = AnthropicSseDecoder::default();
                    let mut accumulator = AnthropicStreamAccumulator::default();
                    let mut completed = None;
                    while let Some(chunk) = response.chunk().await.map_err(provider_error)? {
                        decoder.push(&chunk, |data| {
                            let mut channel_sink = |event| {
                                worker_sender.send(event).map_err(|_| {
                                    anthropic_stream_error("MiniMax stream consumer disconnected")
                                })
                            };
                            if let Some(wire) =
                                accumulator.push_sse_data(data, &mut channel_sink)?
                            {
                                let model = self.model_response_from_wire(wire);
                                worker_sender
                                    .send(ProviderStreamEvent::Completed {
                                        response: model.clone(),
                                    })
                                    .map_err(|_| {
                                        anthropic_stream_error(
                                            "MiniMax stream consumer disconnected",
                                        )
                                    })?;
                                completed = Some(model);
                            }
                            Ok(())
                        })?;
                    }
                    decoder.finish(|data| {
                        let mut channel_sink = |event| {
                            worker_sender.send(event).map_err(|_| {
                                anthropic_stream_error("MiniMax stream consumer disconnected")
                            })
                        };
                        if let Some(wire) = accumulator.push_sse_data(data, &mut channel_sink)? {
                            let model = self.model_response_from_wire(wire);
                            worker_sender
                                .send(ProviderStreamEvent::Completed {
                                    response: model.clone(),
                                })
                                .map_err(|_| {
                                    anthropic_stream_error("MiniMax stream consumer disconnected")
                                })?;
                            completed = Some(model);
                        }
                        Ok(())
                    })?;
                    completed.ok_or_else(|| {
                        anthropic_stream_error("MiniMax SSE stream ended without message_stop")
                    })
                })
            });
            drop(sender);
            for event in receiver {
                sink(event)?;
            }
            worker
                .join()
                .map_err(|_| anthropic_stream_error("MiniMax streaming worker panicked"))?
        })
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

    fn complete_streaming(
        &self,
        request: &ModelRequest,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        self.complete_streaming_request(request, sink)
    }

    fn complete_streaming_cancellable(
        &self,
        request: &ModelRequest,
        cancel: &AtomicBool,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        self.complete_streaming_cancellable_request(request, cancel, sink)
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
            streaming: true,
        }
    } else {
        ProviderCapabilities {
            tool_calling: true,
            streaming: true,
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
        streaming: true,
    }
}

#[derive(Debug, Default)]
struct AnthropicSseDecoder {
    pending: Vec<u8>,
    data: String,
}

impl AnthropicSseDecoder {
    fn push(
        &mut self,
        bytes: &[u8],
        mut sink: impl FnMut(&str) -> MedusaResult<()>,
    ) -> MedusaResult<()> {
        self.pending.extend_from_slice(bytes);
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line = self.pending.drain(..=newline).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.process_line(&line, &mut sink)?;
        }
        Ok(())
    }

    fn finish(&mut self, mut sink: impl FnMut(&str) -> MedusaResult<()>) -> MedusaResult<()> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.process_line(&line, &mut sink)?;
        }
        self.dispatch(&mut sink)
    }

    fn process_line(
        &mut self,
        line: &[u8],
        sink: &mut impl FnMut(&str) -> MedusaResult<()>,
    ) -> MedusaResult<()> {
        let line = std::str::from_utf8(line).map_err(|error| {
            anthropic_stream_error(format!("MiniMax SSE line is not UTF-8: {error}"))
        })?;
        if line.is_empty() {
            return self.dispatch(sink);
        }
        if let Some(value) = line.strip_prefix("data:") {
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(value.strip_prefix(' ').unwrap_or(value));
        }
        Ok(())
    }

    fn dispatch(&mut self, sink: &mut impl FnMut(&str) -> MedusaResult<()>) -> MedusaResult<()> {
        if self.data.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(&mut self.data);
        sink(&data)
    }
}

#[derive(Debug, Default)]
struct AnthropicStreamAccumulator {
    response_id: Option<String>,
    blocks: Vec<Option<WireBlock>>,
    tool_fragments: HashMap<usize, String>,
    stop_reason: Option<String>,
    usage: WireUsage,
    output_started: bool,
    completed: bool,
}

impl AnthropicStreamAccumulator {
    fn push_sse_data(
        &mut self,
        data: &str,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<Option<WireResponse>> {
        if self.completed {
            return Err(anthropic_stream_error(
                "MiniMax stream emitted data after message_stop",
            ));
        }
        let event: Value = serde_json::from_str(data).map_err(|error| {
            anthropic_stream_error(format!("MiniMax stream event is invalid JSON: {error}"))
        })?;
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match event_type {
            "message_start" => {
                let message = event.get("message").unwrap_or(&Value::Null);
                self.response_id = message.get("id").and_then(Value::as_str).map(str::to_owned);
                if let Some(id) = &self.response_id {
                    sink(ProviderStreamEvent::ResponseStarted {
                        response_id: Some(id.clone()),
                    })?;
                }
                if let Some(usage) = message.get("usage") {
                    self.apply_usage(usage);
                    sink(ProviderStreamEvent::Usage {
                        usage: self.usage.as_usage(),
                    })?;
                }
            }
            "content_block_start" => {
                let index =
                    event.get("index").and_then(Value::as_u64).ok_or_else(|| {
                        anthropic_stream_error("MiniMax block start omitted index")
                    })? as usize;
                self.ensure_block(index);
                let block = event.get("content_block").unwrap_or(&Value::Null);
                let wire = match block.get("type").and_then(Value::as_str) {
                    Some("text") => WireBlock::Text {
                        text: block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    },
                    Some("tool_use") => WireBlock::ToolUse {
                        id: block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        name: block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        input: block.get("input").cloned().unwrap_or_else(|| json!({})),
                    },
                    Some("thinking") => WireBlock::Thinking {
                        thinking: block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        signature: block
                            .get("signature")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    },
                    _ => WireBlock::Unknown,
                };
                self.blocks[index] = Some(wire);
            }
            "content_block_delta" => {
                let index =
                    event.get("index").and_then(Value::as_u64).ok_or_else(|| {
                        anthropic_stream_error("MiniMax block delta omitted index")
                    })? as usize;
                self.ensure_block(index);
                let delta = event.get("delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let text = delta
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if !text.is_empty() {
                            if !self.output_started {
                                self.output_started = true;
                                sink(ProviderStreamEvent::OutputStarted)?;
                            }
                            if let Some(Some(WireBlock::Text { text: output })) =
                                self.blocks.get_mut(index)
                            {
                                output.push_str(text);
                            }
                            sink(ProviderStreamEvent::TextDelta {
                                text: text.to_owned(),
                            })?;
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(thinking) = delta.get("thinking").and_then(Value::as_str)
                            && let Some(Some(WireBlock::Thinking {
                                thinking: output, ..
                            })) = self.blocks.get_mut(index)
                        {
                            output.push_str(thinking);
                        }
                    }
                    Some("input_json_delta") => {
                        self.tool_fragments.entry(index).or_default().push_str(
                            delta
                                .get("partial_json")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        );
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = event
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| anthropic_stream_error("MiniMax block stop omitted index"))?
                    as usize;
                if let Some(Some(WireBlock::ToolUse { input, .. })) = self.blocks.get_mut(index) {
                    if let Some(fragment) = self.tool_fragments.remove(&index) {
                        *input = serde_json::from_str(&fragment).map_err(|error| {
                            anthropic_stream_error(format!(
                                "MiniMax tool input is invalid JSON: {error}"
                            ))
                        })?;
                    }
                    if let Some(Some(WireBlock::ToolUse { id, name, input })) =
                        self.blocks.get(index)
                    {
                        if !self.output_started {
                            self.output_started = true;
                            sink(ProviderStreamEvent::OutputStarted)?;
                        }
                        sink(ProviderStreamEvent::ToolUseReady {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        })?;
                    }
                }
            }
            "message_delta" => {
                if let Some(reason) = event
                    .get("delta")
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    self.stop_reason = Some(reason.to_owned());
                }
                if let Some(usage) = event.get("usage") {
                    self.apply_usage(usage);
                    sink(ProviderStreamEvent::Usage {
                        usage: self.usage.as_usage(),
                    })?;
                }
            }
            "message_stop" => {
                self.completed = true;
                return Ok(Some(self.finish_wire()));
            }
            _ => {}
        }
        Ok(None)
    }

    fn ensure_block(&mut self, index: usize) {
        if self.blocks.len() <= index {
            self.blocks.resize_with(index + 1, || None);
        }
    }

    fn apply_usage(&mut self, usage: &Value) {
        for (key, target) in [
            ("input_tokens", &mut self.usage.input_tokens),
            ("output_tokens", &mut self.usage.output_tokens),
            (
                "cache_read_input_tokens",
                &mut self.usage.cache_read_input_tokens,
            ),
            (
                "cache_creation_input_tokens",
                &mut self.usage.cache_creation_input_tokens,
            ),
        ] {
            if let Some(value) = usage.get(key).and_then(Value::as_u64) {
                *target = value;
            }
        }
    }

    fn finish_wire(&self) -> WireResponse {
        WireResponse {
            id: self.response_id.clone(),
            stop_reason: self.stop_reason.clone(),
            content: self.blocks.iter().filter_map(Clone::clone).collect(),
            usage: self.usage.clone(),
        }
    }
}

fn anthropic_stream_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Execution,
        message.into(),
    )
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

impl WireUsage {
    fn as_usage(&self) -> Usage {
        Usage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
        }
    }
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
            usage: self.usage.as_usage(),
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
    fn dynamic_system_context_is_after_the_cached_system_breakpoint() {
        let provider = test_provider();
        let mut request = empty_request();
        request.system = format!("stable{}\n\nvolatile", crate::DYNAMIC_SYSTEM_CONTEXT_MARKER);
        let body = provider.request_body(&request);
        assert_eq!(body["system"].as_array().map(Vec::len), Some(2));
        assert_eq!(body["system"][0]["text"], "stable");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["system"][1]["text"], "\n\nvolatile");
        assert!(body["system"][1].get("cache_control").is_none());
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
    fn configured_streaming_uses_native_wire_support() {
        let mut config = Config::default();
        config.model.provider = "anthropic".to_owned();
        config.model.protocol = "anthropic".to_owned();
        config.model.streaming = true;
        let provider =
            MiniMaxProvider::from_config_with_api_key(&config, Some("session-key".to_owned()))
                .expect("anthropic provider");
        assert!(provider.capabilities().streaming);
        assert_eq!(
            provider.request_body_with_stream(&empty_request(), true)["stream"],
            Value::Bool(true)
        );
    }

    #[test]
    fn native_stream_accumulator_emits_visible_text_and_terminal_response() {
        let mut accumulator = AnthropicStreamAccumulator::default();
        let mut events = Vec::new();
        let mut push = |data: &str| {
            if let Some(wire) = accumulator
                .push_sse_data(data, &mut |event| {
                    events.push(event);
                    Ok(())
                })
                .expect("stream event")
            {
                assert_eq!(wire.id.as_deref(), Some("msg-1"));
                assert_eq!(wire.stop_reason.as_deref(), Some("end_turn"));
                assert_eq!(wire.content.len(), 1);
            }
        };
        push(r#"{"type":"message_start","message":{"id":"msg-1","usage":{"input_tokens":2}}}"#);
        push(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        );
        push(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}"#,
        );
        push(
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
        );
        push(r#"{"type":"message_stop"}"#);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ProviderStreamEvent::OutputStarted))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            ProviderStreamEvent::TextDelta { text } if text == "hello"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ProviderStreamEvent::Usage { usage } if usage.input_tokens == 2 && usage.output_tokens == 1
        )));
    }
}
