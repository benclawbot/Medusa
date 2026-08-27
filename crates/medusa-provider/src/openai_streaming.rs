use std::collections::BTreeSet;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::Deserialize;

use crate::{
    ModelResponse, OpenAiPromptTokenDetails, ProviderStreamEvent, ResponseBlock,
    StreamingToolCallAssembler, Usage,
};

/// Stateful OpenAI chat-completions SSE parser that emits provider-neutral stream events.
#[derive(Debug, Default)]
pub struct OpenAiStreamAccumulator {
    response_id: Option<String>,
    stop_reason: Option<String>,
    text: String,
    tool_calls: StreamingToolCallAssembler,
    pending_tool_indices: BTreeSet<u32>,
    tool_blocks: Vec<ResponseBlock>,
    usage: Usage,
    output_started: bool,
    completed: bool,
}

impl OpenAiStreamAccumulator {
    /// Consumes one SSE `data:` payload and returns the completed response on `[DONE]`.
    pub fn push_sse_data(
        &mut self,
        data: &str,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<Option<ModelResponse>> {
        if self.completed {
            return Err(stream_error("OpenAI stream emitted data after completion"));
        }
        if data.trim() == "[DONE]" {
            let response = self.finish(sink)?;
            return Ok(Some(response));
        }

        let chunk: OpenAiStreamChunk = serde_json::from_str(data).map_err(|error| {
            stream_error(format!("OpenAI stream chunk is invalid JSON: {error}"))
        })?;
        if self.response_id.is_none() {
            self.response_id = chunk.id.clone();
            if chunk.id.is_some() {
                sink(ProviderStreamEvent::ResponseStarted {
                    response_id: chunk.id.clone(),
                })?;
            }
        }
        if let Some(usage) = chunk.usage {
            self.usage = usage.into_usage();
            sink(ProviderStreamEvent::Usage { usage: self.usage })?;
        }

        for choice in chunk.choices {
            let has_output = choice
                .delta
                .content
                .as_ref()
                .is_some_and(|value| !value.is_empty())
                || !choice.delta.tool_calls.is_empty();
            if has_output && !self.output_started {
                self.output_started = true;
                sink(ProviderStreamEvent::OutputStarted)?;
            }
            if let Some(content) = choice.delta.content.filter(|value| !value.is_empty()) {
                self.text.push_str(&content);
                sink(ProviderStreamEvent::TextDelta { text: content })?;
            }
            for call in choice.delta.tool_calls {
                let index = u32::try_from(call.index)
                    .map_err(|_| stream_error("OpenAI tool-call index exceeds u32"))?;
                self.pending_tool_indices.insert(index);
                self.tool_calls.push_fragment(
                    index,
                    call.id.as_deref(),
                    call.function
                        .as_ref()
                        .and_then(|function| function.name.as_deref()),
                    call.function
                        .as_ref()
                        .and_then(|function| function.arguments.as_deref())
                        .unwrap_or_default(),
                )?;
            }
            if let Some(reason) = choice.finish_reason {
                self.stop_reason = Some(reason);
                self.finish_pending_tools(sink)?;
            }
        }
        Ok(None)
    }

    /// Finishes a stream after all fragmented tool calls reached a completion boundary.
    pub fn finish(
        &mut self,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        if self.completed {
            return Err(stream_error("OpenAI stream completed twice"));
        }
        if !self.pending_tool_indices.is_empty() {
            return Err(stream_error(
                "OpenAI stream ended before fragmented tool calls reached a completion boundary",
            ));
        }
        let mut blocks = Vec::new();
        let visible_text = crate::strip_hidden_reasoning(&self.text);
        if !visible_text.is_empty() {
            blocks.push(ResponseBlock::Text { text: visible_text });
        }
        blocks.extend(self.tool_blocks.clone());
        let response = ModelResponse {
            response_id: self.response_id.clone(),
            stop_reason: self.stop_reason.clone(),
            blocks,
            usage: self.usage,
        };
        self.completed = true;
        sink(ProviderStreamEvent::Completed {
            response: response.clone(),
        })?;
        Ok(response)
    }

    /// Finalizes a provider stream that closes after a terminal chunk without emitting `[DONE]`.
    /// MiniMax's OpenAI-compatible endpoint uses this valid HTTP/SSE close pattern.
    pub fn finish_at_eof(
        &mut self,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<Option<ModelResponse>> {
        if self.completed || self.stop_reason.is_none() {
            return Ok(None);
        }
        self.finish(sink).map(Some)
    }

    fn finish_pending_tools(
        &mut self,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<()> {
        let pending = self
            .pending_tool_indices
            .iter()
            .copied()
            .collect::<Vec<_>>();
        for index in pending {
            let ready = self.tool_calls.finish(index)?;
            let ProviderStreamEvent::ToolUseReady { id, name, input } = &ready else {
                return Err(stream_error("tool-call assembler emitted a non-tool event"));
            };
            self.tool_blocks.push(ResponseBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            });
            sink(ready)?;
            self.pending_tool_indices.remove(&index);
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamChunk {
    id: Option<String>,
    #[serde(default)]
    choices: Vec<OpenAiStreamChoice>,
    usage: Option<OpenAiStreamUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamChoice {
    #[serde(default)]
    delta: OpenAiStreamDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAiStreamDelta {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<OpenAiToolCallDelta>,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolCallDelta {
    index: usize,
    id: Option<String>,
    function: Option<OpenAiFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct OpenAiFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: OpenAiPromptTokenDetails,
}

impl OpenAiStreamUsage {
    fn into_usage(self) -> Usage {
        Usage {
            input_tokens: self.prompt_tokens,
            output_tokens: self.completion_tokens,
            cache_read_input_tokens: self.prompt_tokens_details.cached_tokens,
            ..Usage::default()
        }
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
    fn fragmented_tool_call_becomes_ready_before_done() {
        let mut accumulator = OpenAiStreamAccumulator::default();
        let mut events = Vec::new();
        {
            let mut sink = |event| {
                events.push(event);
                Ok(())
            };

            accumulator
                .push_sse_data(
                    r#"{"id":"chatcmpl-1","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"read_file","arguments":"{\"path\":\"src/"}}]},"finish_reason":null}]}"#,
                    &mut sink,
                )
                .expect("first chunk");
            accumulator
                .push_sse_data(
                    r#"{"id":"chatcmpl-1","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"lib.rs\"}"}}]},"finish_reason":"tool_calls"}]}"#,
                    &mut sink,
                )
                .expect("completion boundary");
        }

        assert!(events.iter().any(|event| matches!(
            event,
            ProviderStreamEvent::ToolUseReady { id, name, input }
                if id == "call-1" && name == "read_file" && input["path"] == "src/lib.rs"
        )));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ProviderStreamEvent::Completed { .. }))
        );

        let response = {
            let mut sink = |event| {
                events.push(event);
                Ok(())
            };
            accumulator
                .push_sse_data("[DONE]", &mut sink)
                .expect("done")
                .expect("response")
        };
        assert_eq!(response.stop_reason.as_deref(), Some("tool_calls"));
        assert!(matches!(
            response.blocks.as_slice(),
            [ResponseBlock::ToolUse { .. }]
        ));
    }

    #[test]
    fn text_usage_and_cached_tokens_are_preserved() {
        let mut accumulator = OpenAiStreamAccumulator::default();
        let mut events = Vec::new();
        let mut sink = |event| {
            events.push(event);
            Ok(())
        };
        accumulator
            .push_sse_data(
                r#"{"id":"chatcmpl-2","choices":[{"delta":{"content":"hel"},"finish_reason":null}]}"#,
                &mut sink,
            )
            .expect("text one");
        accumulator
            .push_sse_data(
                r#"{"id":"chatcmpl-2","choices":[{"delta":{"content":"lo"},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":90}}}"#,
                &mut sink,
            )
            .expect("text two");
        let response = accumulator
            .push_sse_data("[DONE]", &mut sink)
            .expect("done")
            .expect("response");

        assert_eq!(
            response.blocks,
            vec![ResponseBlock::Text {
                text: "hello".to_owned()
            }]
        );
        assert_eq!(response.usage.input_tokens, 100);
        assert_eq!(response.usage.cache_read_input_tokens, 90);
        assert!(events.iter().any(|event| matches!(
            event,
            ProviderStreamEvent::TextDelta { text } if text == "hel"
        )));
    }

    #[test]
    fn terminal_chunk_can_finish_without_done_sentinel() {
        let mut accumulator = OpenAiStreamAccumulator::default();
        let mut events = Vec::new();
        let mut sink = |event| {
            events.push(event);
            Ok(())
        };
        accumulator
            .push_sse_data(
                r#"{"id":"chatcmpl-eof","choices":[{"delta":{"content":"hello"},"finish_reason":"stop"}]}"#,
                &mut sink,
            )
            .expect("terminal chunk");
        let response = accumulator
            .finish_at_eof(&mut sink)
            .expect("finish at eof")
            .expect("response");
        assert_eq!(
            response.blocks,
            vec![ResponseBlock::Text {
                text: "hello".into()
            }]
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ProviderStreamEvent::Completed { .. }))
        );
    }

    #[test]
    fn done_before_tool_completion_fails_closed() {
        let mut accumulator = OpenAiStreamAccumulator::default();
        let mut sink = |_| Ok(());
        accumulator
            .push_sse_data(
                r#"{"id":"chatcmpl-3","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"tool","arguments":"{\"x\":"}}]},"finish_reason":null}]}"#,
                &mut sink,
            )
            .expect("fragment");
        assert!(accumulator.push_sse_data("[DONE]", &mut sink).is_err());
    }

    #[test]
    fn malformed_stream_json_is_rejected() {
        let mut accumulator = OpenAiStreamAccumulator::default();
        let mut sink = |_| Ok(());
        assert!(accumulator.push_sse_data("{not-json}", &mut sink).is_err());
    }
}
