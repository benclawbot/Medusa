use std::{
    fs,
    sync::atomic::{AtomicUsize, Ordering},
};

use medusa_agent::AgentEngine;
use medusa_config::Config;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};

struct FailingProvider {
    attempts: AtomicUsize,
}

impl FailingProvider {
    fn new() -> Self {
        Self {
            attempts: AtomicUsize::new(0),
        }
    }
}

impl ModelProvider for FailingProvider {
    fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Transient,
            "provider temporarily unavailable",
        )
        .with_retryable(true))
    }
}

#[test]
fn terminal_provider_failure_records_history() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let engine = AgentEngine::new(FailingProvider::new(), Config::default());
    let mut session = engine
        .create_session(
            directory.path(),
            "exercise runtime failure handling".to_owned(),
        )
        .expect("create session");

    let error = engine
        .run_to_completion(&mut session)
        .expect_err("provider failure should exhaust its bounded retry budget");
    assert_eq!(error.code, ErrorCode::DependencyUnavailable);

    let history = directory
        .path()
        .join(".medusa/learning/failure-history")
        .join(format!("{}.json", session.id));
    let history_json: serde_json::Value =
        serde_json::from_slice(&fs::read(history).expect("read failure history"))
            .expect("failure history json");
    assert_eq!(
        history_json["records"].as_array().expect("records").len(),
        4
    );
}
