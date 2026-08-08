from pathlib import Path


def replace(path: str, old: str, new: str) -> None:
    target = Path(path)
    text = target.read_text()
    if new in text:
        return
    if old not in text:
        raise SystemExit(f"expected source fragment missing in {path}")
    target.write_text(text.replace(old, new, 1))


ENGINE = "crates/medusa-agent/src/engine.rs"
MANAGER = "crates/medusa-provider/src/manager.rs"

replace(
    ENGINE,
    "use medusa_provider::{Message, MessageBlock, ModelProvider, ModelRequest, ResponseBlock, Role};",
    "use medusa_provider::{\n    Message, MessageBlock, ModelProvider, ModelRequest, ProviderStreamEvent,\n    ProviderStreamTranscript, ResponseBlock, Role,\n};",
)

old = '''        let request_started = std::time::Instant::now();
        let response = match self
            .provider
            .complete_cancellable(&request, &self.cancellation)
        {
            Ok(response) => response,
            Err(error) if context_budget::is_context_limit_rejection(&error.to_string()) => {
                if !compacted {
                    compact_session(
                        session,
                        Some(
                            "recover from the provider context limit while preserving the current objective, decisions, tool results, and pending work",
                        ),
                    )?;
                    validate_messages(&session.messages, &self.provider.capabilities())?;
                    request.messages = messages_with_turn_instruction(session, turn_instruction);
                    validate_messages(&request.messages, &self.provider.capabilities())?;
                }
                self.provider
                    .complete_cancellable(&request, &self.cancellation)?
            }
            Err(error) => return Err(error),
        };'''
new = '''        let request_started = std::time::Instant::now();
        let streaming = self.provider.capabilities().streaming;
        let mut stream_transcript = ProviderStreamTranscript::default();
        let mut streamed_text = String::new();
        let mut stream_text_rejected = false;
        let mut complete_request = |request: &ModelRequest| {
            if !streaming {
                return self
                    .provider
                    .complete_cancellable(request, &self.cancellation);
            }
            let mut sink = |event: ProviderStreamEvent| {
                stream_transcript.push(event.clone())?;
                if let ProviderStreamEvent::TextDelta { text } = event
                    && !stream_text_rejected
                {
                    streamed_text.push_str(&text);
                    if validate_provider_text(&streamed_text).is_ok() {
                        observer(&AgentUpdate::AssistantText(text));
                    } else {
                        stream_text_rejected = true;
                        observer(&AgentUpdate::AssistantText(
                            "[provider output rejected: identity or policy contamination]".to_owned(),
                        ));
                    }
                }
                Ok(())
            };
            self.provider.complete_streaming_cancellable(
                request,
                &self.cancellation,
                &mut sink,
            )
        };
        let response = match complete_request(&request) {
            Ok(response) => response,
            Err(error) if context_budget::is_context_limit_rejection(&error.to_string()) => {
                if !compacted {
                    compact_session(
                        session,
                        Some(
                            "recover from the provider context limit while preserving the current objective, decisions, tool results, and pending work",
                        ),
                    )?;
                    validate_messages(&session.messages, &self.provider.capabilities())?;
                    request.messages = messages_with_turn_instruction(session, turn_instruction);
                    validate_messages(&request.messages, &self.provider.capabilities())?;
                }
                complete_request(&request)?
            }
            Err(error) => return Err(error),
        };'''
replace(ENGINE, old, new)

replace(
    ENGINE,
    '''        if fallback_question.is_none() && !assistant_text.is_empty() {
            observer(&AgentUpdate::AssistantText(assistant_text.join("\\n")));
        }''',
    '''        if fallback_question.is_none()
            && !assistant_text.is_empty()
            && (!streaming || streamed_text.is_empty())
        {
            observer(&AgentUpdate::AssistantText(assistant_text.join("\\n")));
        }''',
)

replace(
    MANAGER,
    '''    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.complete_with_cancel(request, None)
    }

    fn complete_cancellable(
        &self,
        request: &ModelRequest,
        cancel: &AtomicBool,
    ) -> MedusaResult<ModelResponse> {
        self.complete_with_cancel(request, Some(cancel))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.providers
            .first()
            .map_or_else(ProviderCapabilities::default, ModelProvider::capabilities)
    }''',
    '''    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.complete_with_cancel_and_sink(request, None, None)
    }

    fn complete_streaming(
        &self,
        request: &ModelRequest,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        self.complete_with_cancel_and_sink(request, None, Some(sink))
    }

    fn complete_streaming_cancellable(
        &self,
        request: &ModelRequest,
        cancel: &AtomicBool,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        self.complete_with_cancel_and_sink(request, Some(cancel), Some(sink))
    }

    fn complete_cancellable(
        &self,
        request: &ModelRequest,
        cancel: &AtomicBool,
    ) -> MedusaResult<ModelResponse> {
        self.complete_with_cancel_and_sink(request, Some(cancel), None)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        let mut capabilities = self
            .providers
            .first()
            .map_or_else(ProviderCapabilities::default, ModelProvider::capabilities);
        capabilities.streaming = self.providers.iter().enumerate().any(|(index, provider)| {
            self.profiles
                .get(index)
                .is_some_and(|profile| profile.streaming)
                && provider.capabilities().streaming
        });
        capabilities
    }''',
)

replace(
    MANAGER,
    '''    fn complete_with_cancel(
        &self,
        request: &ModelRequest,
        cancel: Option<&AtomicBool>,
    ) -> MedusaResult<ModelResponse> {''',
    '''    fn complete_with_cancel_and_sink(
        &self,
        request: &ModelRequest,
        cancel: Option<&AtomicBool>,
        mut sink: Option<&mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>>,
    ) -> MedusaResult<ModelResponse> {''',
)

replace(
    MANAGER,
    '''        if let Ok(cache) = self.cache.lock()
            && let Some(response) = cache.get(&key)
        {
            self.record_cache_hit()?;
            return Ok(response.clone());
        }''',
    '''        if let Ok(cache) = self.cache.lock()
            && let Some(response) = cache.get(&key)
        {
            self.record_cache_hit()?;
            if let Some(sink) = sink.as_deref_mut() {
                sink(ProviderStreamEvent::Completed {
                    response: response.clone(),
                })?;
            }
            return Ok(response.clone());
        }''',
)

replace(
    MANAGER,
    '''                let mut first_token_ms = None;
                let mut stream_sink = |event: ProviderStreamEvent| {
                    if first_token_ms.is_none()
                        && matches!(event, ProviderStreamEvent::OutputStarted)
                    {
                        first_token_ms = Some(elapsed_ms(started));
                    }
                    Ok(())
                };''',
    '''                let mut first_token_ms = None;
                let mut route_stream_started = false;
                let mut stream_sink = |event: ProviderStreamEvent| {
                    if first_token_ms.is_none()
                        && matches!(event, ProviderStreamEvent::OutputStarted)
                    {
                        first_token_ms = Some(elapsed_ms(started));
                    }
                    route_stream_started = true;
                    if let Some(sink) = sink.as_deref_mut() {
                        sink(event)?;
                    }
                    Ok(())
                };''',
)

replace(
    MANAGER,
    '''                    Ok(response) => {
                        let duration_ms = elapsed_ms(started);
                        self.latency.record_success_with_first_token(
                            index,
                            duration_ms,
                            first_token_ms,
                            response.usage,
                        )?;
                        self.record_success(index)?;
                        if let Ok(mut cache) = self.cache.lock() {
                            cache.insert(key.clone(), response.clone());
                        }
                        return Ok(response);
                    }
                    Err(error) => {
                        let duration_ms = elapsed_ms(started);''',
    '''                    Ok(response) => {
                        let duration_ms = elapsed_ms(started);
                        self.latency.record_success_with_first_token(
                            index,
                            duration_ms,
                            first_token_ms,
                            response.usage,
                        )?;
                        self.record_success(index)?;
                        if !streaming
                            && let Some(sink) = sink.as_deref_mut()
                        {
                            sink(ProviderStreamEvent::Completed {
                                response: response.clone(),
                            })?;
                        }
                        if let Ok(mut cache) = self.cache.lock() {
                            cache.insert(key.clone(), response.clone());
                        }
                        return Ok(response);
                    }
                    Err(error) => {
                        if route_stream_started {
                            return Err(error);
                        }
                        let duration_ms = elapsed_ms(started);''',
)
