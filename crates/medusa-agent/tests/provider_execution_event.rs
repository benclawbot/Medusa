use medusa_agent::{AgentEngine, AgentUpdate, StepOutcome};
use medusa_config::Config;
use medusa_core::MedusaResult;
use medusa_protocol::EventPayload;
use medusa_provider::{ModelProvider, ModelRequest, ModelResponse, ResponseBlock, Usage};
use serde_json::json;

struct StatusProvider;

impl ModelProvider for StatusProvider {
    fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
        Ok(ModelResponse {
            response_id: Some("status-response".to_owned()),
            stop_reason: Some("tool_use".to_owned()),
            blocks: vec![ResponseBlock::ToolUse {
                id: "read-root".to_owned(),
                name: "fs_read".to_owned(),
                input: json!({"path": "."}),
            }],
            usage: Usage::default(),
        })
    }

    fn execution_status(&self) -> Option<serde_json::Value> {
        Some(json!({
            "provider_index": 1,
            "cache_hit": false,
            "attempts": 2,
            "retries": 1,
            "failovers": 1
        }))
    }
}

#[test]
fn provider_execution_event_is_observed_ordered_and_durable() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let engine = AgentEngine::new(StatusProvider, Config::default());
    let mut session = engine
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("session");
    let mut updates = Vec::new();

    assert_eq!(
        engine
            .step_with_observer(&mut session, |update| updates.push(update.clone()))
            .expect("provider step"),
        StepOutcome::Continue
    );

    let response_index = updates
        .iter()
        .position(|update| {
            matches!(
                update,
                AgentUpdate::Event(EventPayload::ModelResponseReceived { .. })
            )
        })
        .expect("model response event");
    let execution_index = updates
        .iter()
        .position(|update| {
            matches!(
                update,
                AgentUpdate::Event(EventPayload::ProviderExecutionRecorded { .. })
            )
        })
        .expect("provider execution event");
    let tool_index = updates
        .iter()
        .position(|update| {
            matches!(
                update,
                AgentUpdate::Event(EventPayload::ToolCallRequested { .. })
            )
        })
        .expect("tool request event");

    assert!(response_index < execution_index);
    assert!(execution_index < tool_index);
    assert!(session.events.iter().any(|event| {
        matches!(
            &event.payload,
            EventPayload::ProviderExecutionRecorded { status }
                if status["provider_index"] == json!(1)
                    && status["retries"] == json!(1)
                    && status["failovers"] == json!(1)
        )
    }));

    let restored = engine
        .load_session(directory.path(), session.id.as_str())
        .expect("restored session");
    assert!(restored.events.iter().any(|event| {
        matches!(
            &event.payload,
            EventPayload::ProviderExecutionRecorded { status }
                if status["provider_index"] == json!(1)
        )
    }));
}
