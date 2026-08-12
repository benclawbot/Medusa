use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

use medusa_agent::AgentEngine;
use medusa_config::Config;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};
use serde_json::json;

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

fn install_skill(repo: &Path) {
    let skill = repo.join(".medusa/skills/runtime/SKILL.md");
    fs::create_dir_all(skill.parent().expect("skill parent")).expect("create skill directory");
    fs::write(
        skill,
        "---\nname: runtime\ndescription: Runtime recovery\n---\n# Runtime recovery\n",
    )
    .expect("write skill");
}

fn enable_learning_telemetry(repo: &Path) {
    let state = repo.join(".medusa/learning-review/state.json");
    fs::create_dir_all(state.parent().expect("privacy state parent"))
        .expect("create privacy state directory");
    fs::write(
        state,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "revision": 1,
            "privacy": {
                "capture_enabled": true,
                "user_persistence_enabled": false,
                "cross_repository_reuse_enabled": false,
                "telemetry_enabled": true,
                "automatic_proposals_enabled": true
            },
            "items": [],
            "audit_head": "0000000000000000000000000000000000000000000000000000000000000000"
        }))
        .expect("serialize privacy state"),
    )
    .expect("write privacy state");
}

#[test]
fn terminal_provider_failure_records_history_and_negative_skill_outcome() {
    let directory = tempfile::tempdir().expect("temporary repository");
    install_skill(directory.path());
    enable_learning_telemetry(directory.path());
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

    let outcome = directory
        .path()
        .join(".medusa/learning/skill-outcomes")
        .join(format!("{}.json", session.id));
    let outcome_json: serde_json::Value =
        serde_json::from_slice(&fs::read(outcome).expect("read negative skill outcome"))
            .expect("skill outcome json");
    assert_eq!(outcome_json["completed"], false);
    assert_eq!(outcome_json["verified"], false);
    assert_eq!(
        outcome_json["automatically_loaded_skills"],
        serde_json::json!(["runtime"])
    );
    assert_eq!(outcome_json["terminal_failure"]["disposition"], "terminal");
}
