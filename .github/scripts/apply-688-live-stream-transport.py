from pathlib import Path


def replace(path: str, old: str, new: str) -> None:
    target = Path(path)
    text = target.read_text()
    if new in text:
        return
    if old not in text:
        raise SystemExit(f"expected source fragment missing in {path}")
    target.write_text(text.replace(old, new, 1))


replace(
    "crates/medusa-provider/Cargo.toml",
    'reqwest = { version = "0.12", default-features = false, features = ["blocking", "json", "rustls-tls"] }',
    'reqwest = { version = "0.12", default-features = false, features = ["blocking", "json", "rustls-tls", "stream"] }',
)

replace(
    "crates/medusa-provider/src/lib.rs",
    "mod openai_streaming;\n",
    "mod openai_streaming;\nmod openai_transport;\n",
)

replace(
    "crates/medusa-provider/src/streaming.rs",
    "    ResponseStarted {\n        response_id: Option<String>,\n    },\n    TextDelta {",
    "    ResponseStarted {\n        response_id: Option<String>,\n    },\n    /// First provider output fragment observed on the wire. Carries no unvalidated text.\n    OutputStarted,\n    TextDelta {",
)

replace(
    "crates/medusa-provider/src/openai_streaming.rs",
    "    usage: Usage,\n    completed: bool,",
    "    usage: Usage,\n    output_started: bool,\n    completed: bool,",
)
replace(
    "crates/medusa-provider/src/openai_streaming.rs",
    "        for choice in chunk.choices {\n            if let Some(content) = choice.delta.content.filter(|value| !value.is_empty()) {",
    "        for choice in chunk.choices {\n            let has_output = choice\n                .delta\n                .content\n                .as_ref()\n                .is_some_and(|value| !value.is_empty())\n                || !choice.delta.tool_calls.is_empty();\n            if has_output && !self.output_started {\n                self.output_started = true;\n                sink(ProviderStreamEvent::OutputStarted)?;\n            }\n            if let Some(content) = choice.delta.content.filter(|value| !value.is_empty()) {",
)

replace(
    "crates/medusa-provider/src/contracts.rs",
    "    fn complete_cancellable(\n        &self,\n        request: &ModelRequest,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {",
    "    /// Streams provider-neutral events while preserving cooperative cancellation.\n    /// Streaming-capable providers should override this so cancellation reaches the socket.\n    fn complete_streaming_cancellable(\n        &self,\n        request: &ModelRequest,\n        cancel: &AtomicBool,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        if cancel.load(Ordering::SeqCst) {\n            return Err(cancelled_provider_error());\n        }\n        let response = self.complete_streaming(request, sink)?;\n        if cancel.load(Ordering::SeqCst) {\n            return Err(cancelled_provider_error());\n        }\n        Ok(response)\n    }\n\n    fn complete_cancellable(\n        &self,\n        request: &ModelRequest,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {",
)

replace(
    "crates/medusa-provider/src/configured.rs",
    "    MiniMaxProvider, ModelProvider, ModelRequest, ModelResponse, OpenAiProvider,\n    ProviderCapabilities, ProviderManager, ProviderRouteProfile, RouteRetryPolicy,",
    "    MiniMaxProvider, ModelProvider, ModelRequest, ModelResponse, OpenAiProvider,\n    ProviderCapabilities, ProviderManager, ProviderRouteProfile, ProviderStreamEvent,\n    RouteRetryPolicy,",
)
replace(
    "crates/medusa-provider/src/configured.rs",
    "    fn complete_cancellable(\n        &self,\n        request: &ModelRequest,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {",
    "    fn complete_streaming(\n        &self,\n        request: &ModelRequest,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        match self {\n            Self::Anthropic(provider) => provider.complete_streaming(request, sink),\n            Self::OpenAi(provider) => provider.complete_streaming(request, sink),\n        }\n    }\n\n    fn complete_streaming_cancellable(\n        &self,\n        request: &ModelRequest,\n        cancel: &AtomicBool,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        match self {\n            Self::Anthropic(provider) => {\n                provider.complete_streaming_cancellable(request, cancel, sink)\n            }\n            Self::OpenAi(provider) => {\n                provider.complete_streaming_cancellable(request, cancel, sink)\n            }\n        }\n    }\n\n    fn complete_cancellable(\n        &self,\n        request: &ModelRequest,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {",
)

replace(
    "crates/medusa-provider/src/route_latency.rs",
    "    pub total_first_token_ms: u64,\n    pub cancellation_total_ms: u64,",
    "    pub total_first_token_ms: u64,\n    #[serde(default)]\n    pub first_token_samples: u64,\n    pub cancellation_total_ms: u64,",
)
replace(
    "crates/medusa-provider/src/route_latency.rs",
    "        (self.samples > 0).then(|| self.total_first_token_ms / self.samples)",
    "        (self.first_token_samples > 0)\n            .then(|| self.total_first_token_ms / self.first_token_samples)",
)

replace(
    "crates/medusa-provider/src/route_metrics_store.rs",
    "    pub fn record_success(&self, index: usize, duration_ms: u64, usage: Usage) -> MedusaResult<()> {\n        self.update(index, |stats| {\n            stats.samples = stats.samples.saturating_add(1);\n            stats.successes = stats.successes.saturating_add(1);\n            stats.total_duration_ms = stats.total_duration_ms.saturating_add(duration_ms);\n            stats.input_tokens = stats.input_tokens.saturating_add(usage.input_tokens);\n            stats.cached_input_tokens = stats\n                .cached_input_tokens\n                .saturating_add(usage.cache_read_input_tokens);\n        })\n    }",
    "    pub fn record_success(&self, index: usize, duration_ms: u64, usage: Usage) -> MedusaResult<()> {\n        self.record_success_with_first_token(index, duration_ms, None, usage)\n    }\n\n    pub fn record_success_with_first_token(\n        &self,\n        index: usize,\n        duration_ms: u64,\n        first_token_ms: Option<u64>,\n        usage: Usage,\n    ) -> MedusaResult<()> {\n        self.update(index, |stats| {\n            stats.samples = stats.samples.saturating_add(1);\n            stats.successes = stats.successes.saturating_add(1);\n            stats.total_duration_ms = stats.total_duration_ms.saturating_add(duration_ms);\n            if let Some(first_token_ms) = first_token_ms {\n                stats.first_token_samples = stats.first_token_samples.saturating_add(1);\n                stats.total_first_token_ms = stats\n                    .total_first_token_ms\n                    .saturating_add(first_token_ms);\n            }\n            stats.input_tokens = stats.input_tokens.saturating_add(usage.input_tokens);\n            stats.cached_input_tokens = stats\n                .cached_input_tokens\n                .saturating_add(usage.cache_read_input_tokens);\n        })\n    }",
)
replace(
    "crates/medusa-provider/src/route_metrics_store.rs",
    '        "{}\\u{1f}{}\\u{1f}{}\\u{1f}{}\\u{1f}{}\\u{1f}{}",\n        profile.id,\n        profile.provider,\n        profile.model,\n        profile.protocol,\n        profile.endpoint.as_deref().unwrap_or_default(),\n        profile.auth_source,',
    '        "{}\\u{1f}{}\\u{1f}{}\\u{1f}{}\\u{1f}{}\\u{1f}{}\\u{1f}{}",\n        profile.id,\n        profile.provider,\n        profile.model,\n        profile.protocol,\n        profile.endpoint.as_deref().unwrap_or_default(),\n        profile.auth_source,\n        profile.streaming,',
)

transport = r'''use std::{
    io::Read,
    sync::{atomic::AtomicBool, mpsc},
    thread,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{Client as AsyncClient, blocking::Client as BlockingClient};
use serde_json::Value;

use crate::{
    ModelResponse, OpenAiStreamAccumulator, ProviderStreamEvent, async_response_error,
    blocking_response_error, provider_error, run_cancellable_request,
};

const READ_BUFFER_BYTES: usize = 8 * 1024;

pub(crate) fn complete_blocking(
    client: &BlockingClient,
    endpoint: &str,
    api_key: Option<&str>,
    body: Value,
    sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
) -> MedusaResult<ModelResponse> {
    let mut builder = client.post(endpoint).json(&body);
    if let Some(key) = api_key {
        builder = builder.bearer_auth(key);
    }
    let mut response = builder.send().map_err(provider_error)?;
    if !response.status().is_success() {
        return Err(blocking_response_error(response));
    }

    let mut decoder = SseDecoder::default();
    let mut accumulator = OpenAiStreamAccumulator::default();
    let mut completed = None;
    let mut buffer = [0_u8; READ_BUFFER_BYTES];
    loop {
        let read = response
            .read(&mut buffer)
            .map_err(|error| stream_error(format!("OpenAI SSE read failed: {error}")))?;
        if read == 0 {
            break;
        }
        decoder.push(&buffer[..read], |data| {
            if let Some(response) = accumulator.push_sse_data(data, sink)? {
                completed = Some(response);
            }
            Ok(())
        })?;
    }
    decoder.finish(|data| {
        if let Some(response) = accumulator.push_sse_data(data, sink)? {
            completed = Some(response);
        }
        Ok(())
    })?;
    completed.ok_or_else(|| stream_error("OpenAI SSE stream ended without [DONE]"))
}

pub(crate) fn complete_cancellable(
    client: &AsyncClient,
    endpoint: &str,
    api_key: Option<&str>,
    body: Value,
    cancel: &AtomicBool,
    sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
) -> MedusaResult<ModelResponse> {
    let (sender, receiver) = mpsc::channel::<ProviderStreamEvent>();
    thread::scope(|scope| {
        let worker = scope.spawn(|| {
            run_cancellable_request(cancel, async {
                let mut builder = client.post(endpoint).json(&body);
                if let Some(key) = api_key {
                    builder = builder.bearer_auth(key);
                }
                let mut response = builder.send().await.map_err(provider_error)?;
                if !response.status().is_success() {
                    return Err(async_response_error(response).await);
                }

                let mut decoder = SseDecoder::default();
                let mut accumulator = OpenAiStreamAccumulator::default();
                let mut completed = None;
                while let Some(chunk) = response.chunk().await.map_err(provider_error)? {
                    decoder.push(&chunk, |data| {
                        let mut channel_sink = |event| {
                            sender
                                .send(event)
                                .map_err(|_| stream_error("OpenAI stream consumer disconnected"))
                        };
                        if let Some(response) =
                            accumulator.push_sse_data(data, &mut channel_sink)?
                        {
                            completed = Some(response);
                        }
                        Ok(())
                    })?;
                }
                decoder.finish(|data| {
                    let mut channel_sink = |event| {
                        sender
                            .send(event)
                            .map_err(|_| stream_error("OpenAI stream consumer disconnected"))
                    };
                    if let Some(response) = accumulator.push_sse_data(data, &mut channel_sink)? {
                        completed = Some(response);
                    }
                    Ok(())
                })?;
                completed.ok_or_else(|| stream_error("OpenAI SSE stream ended without [DONE]"))
            })
        });

        for event in receiver {
            sink(event)?;
        }
        worker
            .join()
            .map_err(|_| stream_error("OpenAI streaming worker panicked"))?
    })
}

#[derive(Debug, Default)]
struct SseDecoder {
    pending: Vec<u8>,
    data: String,
}

impl SseDecoder {
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

    fn finish(
        &mut self,
        mut sink: impl FnMut(&str) -> MedusaResult<()>,
    ) -> MedusaResult<()> {
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
        let line = std::str::from_utf8(line)
            .map_err(|error| stream_error(format!("OpenAI SSE line is not UTF-8: {error}")))?;
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

    fn dispatch(
        &mut self,
        sink: &mut impl FnMut(&str) -> MedusaResult<()>,
    ) -> MedusaResult<()> {
        if self.data.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(&mut self.data);
        sink(&data)
    }
}

fn stream_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Execution,
        message.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_handles_fragmented_crlf_and_multiline_data() {
        let mut decoder = SseDecoder::default();
        let mut seen = Vec::new();
        decoder
            .push(b"data: {\"a\":1}\r", |data| {
                seen.push(data.to_owned());
                Ok(())
            })
            .expect("first fragment");
        decoder
            .push(b"\ndata: tail\r\n\r\n", |data| {
                seen.push(data.to_owned());
                Ok(())
            })
            .expect("second fragment");
        assert_eq!(seen, vec!["{\"a\":1}\ntail"]);
    }
}
'''
Path("crates/medusa-provider/src/openai_transport.rs").write_text(transport)

replace(
    "crates/medusa-provider/src/openai.rs",
    "    ImageSource, MessageBlock, ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities,\n    ResponseBlock, Role, Usage, async_response_error, blocking_response_error, provider_error,\n    provider_response_error, run_cancellable_request, shared_async_http_client,\n    shared_blocking_http_client,",
    "    ImageSource, MessageBlock, ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities,\n    ProviderStreamEvent, ResponseBlock, Role, Usage, async_response_error, blocking_response_error,\n    openai_transport, provider_error, provider_response_error, run_cancellable_request,\n    shared_async_http_client, shared_blocking_http_client,",
)
replace(
    "crates/medusa-provider/src/openai.rs",
    "                streaming: false,",
    "                streaming: config.model.streaming,",
)
replace(
    "crates/medusa-provider/src/openai.rs",
    "    fn request_body(&self, request: &ModelRequest) -> MedusaResult<Value> {",
    "    fn request_body(&self, request: &ModelRequest, streaming: bool) -> MedusaResult<Value> {",
)
replace(
    "crates/medusa-provider/src/openai.rs",
    "        Ok(json!({\n            \"model\": self.model,\n            \"messages\": messages,\n            \"tools\": tools,\n            \"max_tokens\": request.max_tokens,\n            \"temperature\": f64::from(request.temperature_milli) / 1000.0,\n            \"stream\": false\n        }))",
    "        let mut body = json!({\n            \"model\": self.model,\n            \"messages\": messages,\n            \"tools\": tools,\n            \"max_tokens\": request.max_tokens,\n            \"temperature\": f64::from(request.temperature_milli) / 1000.0,\n            \"stream\": streaming\n        });\n        if streaming {\n            body[\"stream_options\"] = json!({\"include_usage\": true});\n        }\n        Ok(body)",
)
openai = Path("crates/medusa-provider/src/openai.rs")
text = openai.read_text().replace("self.request_body(request)?", "self.request_body(request, false)?")
text = text.replace(".request_body(&empty_request())", ".request_body(&empty_request(), true)")
text = text.replace("assert!(!provider.capabilities().streaming);", "assert!(provider.capabilities().streaming);")
text = text.replace(
    "assert_eq!(body.get(\"stream\"), Some(&Value::Bool(false)));",
    "assert_eq!(body.get(\"stream\"), Some(&Value::Bool(true)));\n        assert_eq!(body[\"stream_options\"][\"include_usage\"], Value::Bool(true));",
)
marker = "    fn complete_cancellable(\n        &self,\n        request: &ModelRequest,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {\n        run_cancellable_request(cancel, self.complete_request_async(request))\n    }\n"
insert = "    fn complete_streaming(\n        &self,\n        request: &ModelRequest,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        self.validate_request(request)?;\n        let endpoint = format!(\"{}/chat/completions\", self.base_url);\n        openai_transport::complete_blocking(\n            &self.blocking_client,\n            &endpoint,\n            self.api_key.as_deref(),\n            self.request_body(request, true)?,\n            sink,\n        )\n    }\n\n    fn complete_streaming_cancellable(\n        &self,\n        request: &ModelRequest,\n        cancel: &AtomicBool,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        self.validate_request(request)?;\n        let endpoint = format!(\"{}/chat/completions\", self.base_url);\n        openai_transport::complete_cancellable(\n            &self.async_client,\n            &endpoint,\n            self.api_key.as_deref(),\n            self.request_body(request, true)?,\n            cancel,\n            sink,\n        )\n    }\n\n" + marker
if "fn complete_streaming_cancellable(" not in text:
    if marker not in text:
        raise SystemExit("OpenAI trait insertion marker missing")
    text = text.replace(marker, insert, 1)
openai.write_text(text)

replace(
    "crates/medusa-provider/src/manager.rs",
    "    ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities, ProviderHealthStore,\n    ProviderRouteLatencyStore, RouteLatencyPolicy, RouteLatencyStats, latency_aware_route_order,",
    "    ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities, ProviderHealthStore,\n    ProviderRouteLatencyStore, ProviderStreamEvent, RouteLatencyPolicy, RouteLatencyStats,\n    latency_aware_route_order,",
)
replace(
    "crates/medusa-provider/src/manager.rs",
    """                let started = Instant::now();
                match cancel.map_or_else(
                    || provider.complete(request),
                    |flag| provider.complete_cancellable(request, flag),
                ) {
                    Ok(response) => {
                        let duration_ms = elapsed_ms(started);
                        self.latency
                            .record_success(index, duration_ms, response.usage)?;""",
    """                let started = Instant::now();
                let streaming = self
                    .profiles
                    .get(index)
                    .is_some_and(|profile| profile.streaming)
                    && provider.capabilities().streaming;
                let mut first_token_ms = None;
                let mut stream_sink = |event: ProviderStreamEvent| {
                    if first_token_ms.is_none()
                        && matches!(event, ProviderStreamEvent::OutputStarted)
                    {
                        first_token_ms = Some(elapsed_ms(started));
                    }
                    Ok(())
                };
                let result = if streaming {
                    match cancel {
                        Some(flag) => provider.complete_streaming_cancellable(
                            request,
                            flag,
                            &mut stream_sink,
                        ),
                        None => provider.complete_streaming(request, &mut stream_sink),
                    }
                } else {
                    match cancel {
                        Some(flag) => provider.complete_cancellable(request, flag),
                        None => provider.complete(request),
                    }
                };
                match result {
                    Ok(response) => {
                        let duration_ms = elapsed_ms(started);
                        self.latency.record_success_with_first_token(
                            index,
                            duration_ms,
                            first_token_ms,
                            response.usage,
                        )?;""",
)
