use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_provider::{
    Message, MessageBlock, ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities,
    ProviderManager, ProviderRouteProfile, ProviderStreamEvent, ResponseBlock, Role,
    RouteRetryPolicy, Usage,
};

#[derive(Clone)]
enum Behavior {
    Success,
    FailAfterDelta,
}

#[derive(Clone)]
struct StreamingStub {
    calls: Arc<AtomicUsize>,
    behavior: Behavior,
    text: &'static str,
}

impl ModelProvider for StreamingStub {
    fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
        panic!("streaming route should use complete_streaming")
    }

    fn complete_streaming(
        &self,
        _: &ModelRequest,
        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
    ) -> MedusaResult<ModelResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        sink(ProviderStreamEvent::OutputStarted)?;
        sink(ProviderStreamEvent::TextDelta {
            text: self.text.to_owned(),
        })?;
        match self.behavior {
            Behavior::Success => {
                let response = response(self.text);
                sink(ProviderStreamEvent::Completed {
                    response: response.clone(),
                })?;
                Ok(response)
            }
            Behavior::FailAfterDelta => Err(MedusaError::new(
                ErrorCode::DependencyUnavailable,
                ErrorCategory::Transient,
                "stream failed after output",
            )),
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        }
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        system: "system".to_owned(),
        messages: vec![Message {
            role: Role::User,
            content: vec![MessageBlock::Text {
                text: "hello".to_owned(),
            }],
        }],
        tools: Vec::new(),
        max_tokens: 32,
        temperature_milli: 0,
    }
}

fn response(text: &str) -> ModelResponse {
    ModelResponse {
        response_id: Some("response".to_owned()),
        stop_reason: Some("stop".to_owned()),
        blocks: vec![ResponseBlock::Text {
            text: text.to_owned(),
        }],
        usage: Usage::default(),
    }
}

fn profile(id: &str) -> ProviderRouteProfile {
    ProviderRouteProfile {
        id: id.to_owned(),
        provider: "stub".to_owned(),
        model: id.to_owned(),
        protocol: "test".to_owned(),
        endpoint: None,
        auth_source: "test".to_owned(),
        tool_calling: true,
        streaming: true,
        retry: RouteRetryPolicy {
            max_retries: 0,
            base_delay_ms: 0,
            max_delay_ms: 0,
            jitter_ms: 0,
        },
    }
}

#[test]
fn manager_forwards_incremental_events_in_order() {
    let calls = Arc::new(AtomicUsize::new(0));
    let manager = ProviderManager::new_with_profiles(
        vec![StreamingStub {
            calls: Arc::clone(&calls),
            behavior: Behavior::Success,
            text: "delta",
        }],
        vec![profile("primary")],
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let mut sink = move |event| {
        captured.lock().expect("events").push(event);
        Ok(())
    };
    let result = manager
        .complete_streaming(&request(), &mut sink)
        .expect("streaming completion");
    assert_eq!(result, response("delta"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let events = events.lock().expect("events");
    assert!(matches!(events[0], ProviderStreamEvent::OutputStarted));
    assert!(matches!(
        &events[1],
        ProviderStreamEvent::TextDelta { text } if text == "delta"
    ));
    assert!(matches!(events[2], ProviderStreamEvent::Completed { .. }));
}

#[test]
fn manager_does_not_fail_over_after_stream_output_is_exposed() {
    let first_calls = Arc::new(AtomicUsize::new(0));
    let second_calls = Arc::new(AtomicUsize::new(0));
    let manager = ProviderManager::new_with_profiles(
        vec![
            StreamingStub {
                calls: Arc::clone(&first_calls),
                behavior: Behavior::FailAfterDelta,
                text: "partial",
            },
            StreamingStub {
                calls: Arc::clone(&second_calls),
                behavior: Behavior::Success,
                text: "fallback",
            },
        ],
        vec![profile("primary"), profile("fallback")],
    );
    let mut events = Vec::new();
    let error = manager
        .complete_streaming(&request(), &mut |event| {
            events.push(event);
            Ok(())
        })
        .expect_err("streamed failure must remain authoritative");
    assert_eq!(error.category, ErrorCategory::Transient);
    assert_eq!(first_calls.load(Ordering::SeqCst), 1);
    assert_eq!(second_calls.load(Ordering::SeqCst), 0);
    assert!(events.iter().any(|event| matches!(event, ProviderStreamEvent::TextDelta { .. })));
}

#[test]
fn streaming_cache_hit_emits_one_terminal_event_without_requerying_provider() {
    let calls = Arc::new(AtomicUsize::new(0));
    let manager = ProviderManager::new_with_profiles(
        vec![StreamingStub {
            calls: Arc::clone(&calls),
            behavior: Behavior::Success,
            text: "cached",
        }],
        vec![profile("primary")],
    );
    manager
        .complete_streaming(&request(), &mut |_| Ok(()))
        .expect("prime cache");
    let mut events = Vec::new();
    let result = manager
        .complete_streaming(&request(), &mut |event| {
            events.push(event);
            Ok(())
        })
        .expect("cached completion");
    assert_eq!(result, response("cached"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], ProviderStreamEvent::Completed { .. }));
}
