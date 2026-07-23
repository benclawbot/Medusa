use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_provider::{
    Message, MessageBlock, ModelProvider, ModelRequest, ModelResponse, ProviderManager, Role, Usage,
};
use serde_json::json;

#[derive(Clone)]
enum TestProvider {
    Unavailable { calls: Arc<AtomicUsize> },
    Completing { calls: Arc<AtomicUsize> },
}

impl ModelProvider for TestProvider {
    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
        match self {
            Self::Unavailable { calls } => {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(MedusaError::new(
                    ErrorCode::DependencyUnavailable,
                    ErrorCategory::Transient,
                    "primary temporarily unavailable",
                )
                .with_retryable(true))
            }
            Self::Completing { calls } => {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(ModelResponse {
                    response_id: Some(format!("response-{}", request.system)),
                    stop_reason: Some("end_turn".to_owned()),
                    blocks: Vec::new(),
                    usage: Usage::default(),
                })
            }
        }
    }
}

fn request(phase: &str) -> ModelRequest {
    ModelRequest {
        system: phase.to_owned(),
        messages: vec![Message {
            role: Role::User,
            content: vec![MessageBlock::Text {
                text: format!("execute {phase} phase"),
            }],
        }],
        tools: Vec::new(),
        max_tokens: 32,
        temperature_milli: 0,
    }
}

#[test]
fn failover_completes_planning_tool_and_final_turns_once_each() {
    let primary_calls = Arc::new(AtomicUsize::new(0));
    let fallback_calls = Arc::new(AtomicUsize::new(0));
    let manager = ProviderManager::new(vec![
        TestProvider::Unavailable {
            calls: primary_calls.clone(),
        },
        TestProvider::Completing {
            calls: fallback_calls.clone(),
        },
    ]);

    for phase in ["planning", "tool_use", "final_response"] {
        let response = manager
            .complete(&request(phase))
            .expect("fallback response");
        assert_eq!(response.response_id, Some(format!("response-{phase}")));

        let status = ModelProvider::execution_status(&manager).expect("execution status");
        assert_eq!(status["provider_index"], json!(1));
        assert_eq!(status["cache_hit"], json!(false));
    }

    assert_eq!(primary_calls.load(Ordering::SeqCst), 6);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 3);
    assert_eq!(manager.health()[0].retries, 3);
    assert_eq!(manager.health()[0].failovers, 3);
    assert_eq!(manager.health()[1].successes, 3);
}

#[test]
fn cached_final_response_does_not_repeat_provider_or_tool_phase_work() {
    let primary_calls = Arc::new(AtomicUsize::new(0));
    let fallback_calls = Arc::new(AtomicUsize::new(0));
    let manager = ProviderManager::new(vec![
        TestProvider::Unavailable {
            calls: primary_calls.clone(),
        },
        TestProvider::Completing {
            calls: fallback_calls.clone(),
        },
    ]);
    let final_request = request("final_response");

    manager
        .complete(&final_request)
        .expect("initial fallback response");
    manager
        .complete(&final_request)
        .expect("cached fallback response");

    assert_eq!(primary_calls.load(Ordering::SeqCst), 2);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    let status = manager.execution_status().expect("cached status");
    assert_eq!(status["provider_index"], json!(1));
    assert_eq!(status["cache_hit"], json!(true));
    assert_eq!(status["cache_hits"], json!(1));
}
