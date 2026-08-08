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
    "crates/medusa-agent/src/engine.rs",
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
        };
        drop(complete_request);'''
replace("crates/medusa-agent/src/engine.rs", old, new)

replace(
    "crates/medusa-agent/src/engine.rs",
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
