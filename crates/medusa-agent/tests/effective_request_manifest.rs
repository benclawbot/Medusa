use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use medusa_agent::{
    AgentEngine, AgentExecutionPolicy, StepOutcome, inspect_effective_model_request,
};
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

#[test]
fn execution_policy_and_ambient_config_are_bound_without_replay_drift() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let mut config = Config::default();
    config.agent.mode = Mode::ReadOnly;

    let first_calls = Arc::new(AtomicUsize::new(0));
    let first = AgentEngine::new(
        CountingProvider {
            calls: Arc::clone(&first_calls),
        },
        config.clone(),
    )
    .with_execution_policy(
        AgentExecutionPolicy::unrestricted().with_allowed_write_paths(["src".to_owned()]),
    );
    let mut first_session = first
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("first session");
    first.step(&mut first_session).expect("first step");
    let (first_fp, first_manifest) = first_session
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ModelRequestStarted {
                request_fingerprint: Some(fp),
                manifest_ref: Some(reference),
                ..
            } => Some((fp.clone(), reference.clone())),
            _ => None,
        })
        .expect("first manifest");

    let second_calls = Arc::new(AtomicUsize::new(0));
    let second = AgentEngine::new(
        CountingProvider {
            calls: Arc::clone(&second_calls),
        },
        config,
    )
    .with_execution_policy(
        AgentExecutionPolicy::unrestricted().with_allowed_write_paths(["tests".to_owned()]),
    );
    let mut second_session = second
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("second session");
    second.step(&mut second_session).expect("second step");
    let second_fp = second_session
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ModelRequestStarted {
                request_fingerprint: Some(fp),
                ..
            } => Some(fp.clone()),
            _ => None,
        })
        .expect("second fingerprint");
    assert_ne!(
        first_fp, second_fp,
        "execution policy must affect the stable request fingerprint"
    );

    let audit = inspect_effective_model_request(
        directory.path(),
        first_session.id.as_str(),
        &first_manifest,
    )
    .expect("historical request remains reconstructable");
    assert_eq!(audit["healthy"], serde_json::json!(true));
    assert_eq!(
        audit["execution_policy"]["allowed_write_paths"],
        serde_json::json!(["src"]),
    );
}
