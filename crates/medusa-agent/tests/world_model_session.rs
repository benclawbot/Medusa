use medusa_agent::AgentEngine;
use medusa_config::Config;
use medusa_core::MedusaResult;
use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};

struct UnusedProvider;

impl ModelProvider for UnusedProvider {
    fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {
        unreachable!("session creation and model loading do not call the provider")
    }
}

#[test]
fn session_world_model_survives_restart() {
    let repository = tempfile::tempdir().expect("repository");
    let engine = AgentEngine::new(UnusedProvider, Config::default());
    let session = engine
        .create_session(
            repository.path(),
            "diagnose a cancellation failure".to_owned(),
        )
        .expect("create session");

    let reference = session
        .world_model
        .as_ref()
        .expect("new sessions receive a world model");
    assert!(repository.path().join(&reference.relative_path).is_file());

    let restored = engine
        .load_session(repository.path(), session.id.as_str())
        .expect("restore session");
    let model = engine
        .load_session_world_model(&restored)
        .expect("load world model")
        .expect("world model reference");

    assert_eq!(model.objective.original_request, session.objective);
    assert_eq!(model.revision, reference.revision);
}
