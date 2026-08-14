use medusa_agent::{AgentEngine, StepOutcome, preview_session_selective_revert};
use medusa_config::Config;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_provider::{ModelProvider, ModelRequest, ModelResponse, ResponseBlock, Usage};
use serde_json::json;
use std::{collections::VecDeque, fs, sync::Mutex};

struct ScriptedProvider {
    responses: Mutex<VecDeque<ModelResponse>>,
}

impl ScriptedProvider {
    fn new(responses: Vec<ModelResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
        }
    }
}

impl ModelProvider for ScriptedProvider {
    fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.responses
            .lock()
            .expect("provider lock")
            .pop_front()
            .ok_or_else(|| {
                MedusaError::new(
                    ErrorCode::DependencyUnavailable,
                    ErrorCategory::Internal,
                    "scripted response exhausted",
                )
            })
    }
}

fn response(id: &str, content: &str) -> ModelResponse {
    ModelResponse {
        response_id: Some(format!("fixture-{id}")),
        stop_reason: Some("tool_use".into()),
        blocks: vec![ResponseBlock::ToolUse {
            id: id.into(),
            name: "fs_write".into(),
            input: json!({"path":"value.txt","content":content}),
        }],
        usage: Usage::default(),
    }
}

fn init_repo() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("tempdir");
    assert!(
        std::process::Command::new("git")
            .arg("init")
            .arg(directory.path())
            .status()
            .expect("git init")
            .success()
    );
    fs::write(directory.path().join("value.txt"), "alpha\nbeta\n").expect("fixture");
    directory
}

fn mutation_ids(repo: &std::path::Path) -> Vec<String> {
    let journal: serde_json::Value = serde_json::from_slice(
        &fs::read(repo.join(".medusa/mutation-provenance.json")).expect("journal"),
    )
    .expect("journal json");
    journal["records"]
        .as_array()
        .expect("records")
        .iter()
        .map(|record| record["id"].as_str().expect("mutation id").to_owned())
        .collect()
}

#[test]
fn selective_revert_rejects_stale_authored_scope_after_user_edit() {
    let directory = init_repo();
    let engine = AgentEngine::new(
        ScriptedProvider::new(vec![response("write-1", "alpha\nBETA\n")]),
        Config::default(),
    );
    let mut session = engine
        .create_session(directory.path(), "change beta".into())
        .expect("session");
    assert_eq!(
        engine.step(&mut session).expect("write step"),
        StepOutcome::Continue
    );
    let mutation_id = mutation_ids(directory.path()).remove(0);

    fs::write(directory.path().join("value.txt"), "alpha\nUSER\n").expect("user edit");

    assert!(
        preview_session_selective_revert(directory.path(), session.id.as_str(), &mutation_id)
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("value.txt")).unwrap(),
        "alpha\nUSER\n"
    );
}

#[test]
fn selective_revert_rejects_earlier_mutation_when_later_write_overlaps() {
    let directory = init_repo();
    let engine = AgentEngine::new(
        ScriptedProvider::new(vec![
            response("write-1", "alpha\nBETA\n"),
            response("write-2", "alpha\nBeta again\n"),
        ]),
        Config::default(),
    );
    let mut session = engine
        .create_session(directory.path(), "change beta twice".into())
        .expect("session");
    assert_eq!(
        engine.step(&mut session).expect("first write"),
        StepOutcome::Continue
    );
    let first_mutation_id = mutation_ids(directory.path()).remove(0);
    assert_eq!(
        engine.step(&mut session).expect("second write"),
        StepOutcome::Continue
    );
    assert_eq!(mutation_ids(directory.path()).len(), 2);

    assert!(
        preview_session_selective_revert(
            directory.path(),
            session.id.as_str(),
            &first_mutation_id,
        )
        .is_err()
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("value.txt")).unwrap(),
        "alpha\nBeta again\n"
    );
}

#[test]
fn selective_revert_fails_closed_when_persisted_provenance_is_corrupt() {
    let directory = init_repo();
    let engine = AgentEngine::new(
        ScriptedProvider::new(vec![response("write-1", "alpha\nBETA\n")]),
        Config::default(),
    );
    let mut session = engine
        .create_session(directory.path(), "change beta".into())
        .expect("session");
    assert_eq!(
        engine.step(&mut session).expect("write step"),
        StepOutcome::Continue
    );
    let mutation_id = mutation_ids(directory.path()).remove(0);

    fs::write(
        directory.path().join(".medusa/mutation-provenance.json"),
        b"{",
    )
    .expect("corrupt journal");

    assert!(
        preview_session_selective_revert(directory.path(), session.id.as_str(), &mutation_id)
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("value.txt")).unwrap(),
        "alpha\nBETA\n"
    );
}
