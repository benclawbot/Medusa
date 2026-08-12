use std::fs;

use medusa_agent::{AgentEngine, session_browser::list_sessions};
use medusa_config::Config;
use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};
use medusa_runtime::RuntimeController;

struct NeverCalled;

impl ModelProvider for NeverCalled {
    fn complete(&self, _request: &ModelRequest) -> medusa_core::MedusaResult<ModelResponse> {
        panic!("session startup must not call the provider")
    }
}

#[test]
fn continue_latest_selects_the_most_recent_durable_session() {
    let directory = tempfile::tempdir().expect("tempdir");
    let engine = AgentEngine::new(NeverCalled, Config::default());
    let first = engine
        .create_session(directory.path(), "first".to_owned())
        .expect("first session");
    let second = engine
        .create_session(directory.path(), "second".to_owned())
        .expect("second session");

    let sessions = list_sessions(directory.path()).expect("sessions");
    assert_eq!(
        sessions.first().map(|session| session.id.as_str()),
        Some(second.id.as_str())
    );
    assert_ne!(first.id, second.id);

    let error = match RuntimeController::start_continue_latest(directory.path().to_path_buf()) {
        Ok(runtime) => {
            drop(runtime);
            return;
        }
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("missing provider credential"),
        "continue-latest must select a durable session before provider setup: {error}"
    );
}

#[test]
fn continue_latest_fails_when_repository_has_no_sessions() {
    let directory = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(directory.path().join(".medusa")).expect("state directory");
    let error = match RuntimeController::start_continue_latest(directory.path().to_path_buf()) {
        Ok(runtime) => {
            drop(runtime);
            panic!("missing session must fail")
        }
        Err(error) => error,
    };
    assert!(error.to_string().contains("no durable sessions exist"));
}
