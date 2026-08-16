use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use medusa_agent::{AgentEngine, StepOutcome, inspect_effective_model_request};
use medusa_config::{Config, Mode};
use medusa_core::MedusaResult;
use medusa_protocol::EventPayload;
use medusa_provider::{ModelProvider, ModelRequest, ModelResponse, ResponseBlock, Usage};

struct CountingProvider {
    calls: Arc<AtomicUsize>,
}

impl ModelProvider for CountingProvider {
    fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ModelResponse {
            response_id: Some("manifest-test".to_owned()),
            stop_reason: Some("stop".to_owned()),
            blocks: vec![ResponseBlock::Text {
                text: "done".to_owned(),
            }],
            usage: Usage::default(),
        })
    }
}

#[test]
fn effective_request_is_persisted_before_start_and_auditable_after_restart() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let calls = Arc::new(AtomicUsize::new(0));
    let mut config = Config::default();
    config.agent.mode = Mode::ReadOnly;
    let engine = AgentEngine::new(
        CountingProvider {
            calls: Arc::clone(&calls),
        },
        config,
    );
    let mut session = engine
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("create session");
    assert_eq!(
        engine.step(&mut session).expect("model step"),
        StepOutcome::TurnComplete
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let (start_sequence, request_id, request_fingerprint, manifest_ref) = session
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ModelRequestStarted {
                request_id: Some(request_id),
                request_fingerprint: Some(request_fingerprint),
                manifest_ref: Some(manifest_ref),
                ..
            } => Some((
                event.sequence,
                request_id.clone(),
                request_fingerprint.clone(),
                manifest_ref.clone(),
            )),
            _ => None,
        })
        .expect("request start with durable manifest");
    let response_sequence = session
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ModelResponseReceived {
                request_id: Some(response_request_id),
                request_fingerprint: Some(response_fingerprint),
                ..
            } if response_request_id == &request_id
                && response_fingerprint == &request_fingerprint =>
            {
                Some(event.sequence)
            }
            _ => None,
        })
        .expect("linked response");
    assert!(start_sequence < response_sequence);

    drop(engine);
    let audit =
        inspect_effective_model_request(directory.path(), session.id.as_str(), &manifest_ref)
            .expect("reconstruct after restart");
    assert_eq!(audit["healthy"], serde_json::json!(true));
    assert_eq!(audit["request_id"], serde_json::json!(request_id));
    assert_eq!(
        audit["request_fingerprint"],
        serde_json::json!(request_fingerprint)
    );
    assert!(audit["canonical_request"]["system"].is_string());
    assert!(audit["canonical_request"]["messages"].is_array());
    assert!(audit["canonical_request"]["tools"].is_array());
}
