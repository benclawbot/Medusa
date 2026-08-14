use medusa_agent::{
    AgentEngine, StepOutcome, apply_session_selective_revert, preview_session_selective_revert,
};
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
fn response(blocks: Vec<ResponseBlock>, stop_reason: &str) -> ModelResponse {
    ModelResponse {
        response_id: Some("fixture".into()),
        stop_reason: Some(stop_reason.into()),
        blocks,
        usage: Usage::default(),
    }
}

#[test]
fn ordinary_dispatch_write_survives_restart_and_selectively_reverts() {
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
    let engine = AgentEngine::new(
        ScriptedProvider::new(vec![response(
            vec![ResponseBlock::ToolUse {
                id: "write-1".into(),
                name: "fs_write".into(),
                input: json!({"path":"value.txt","content":"alpha\nBETA\n"}),
            }],
            "tool_use",
        )]),
        Config::default(),
    );
    let mut session = engine
        .create_session(directory.path(), "change beta".into())
        .expect("session");
    assert_eq!(
        engine.step(&mut session).expect("write step"),
        StepOutcome::Continue
    );
    let journal: serde_json::Value = serde_json::from_slice(
        &fs::read(directory.path().join(".medusa/mutation-provenance.json")).expect("journal"),
    )
    .expect("json");
    let mutation_id = journal["records"][0]["id"]
        .as_str()
        .expect("mutation id")
        .to_owned();
    assert_eq!(
        journal["records"][0]["context"]["session_id"].as_str(),
        Some(session.id.as_str())
    );
    assert_eq!(
        journal["records"][0]["context"]["activity_id"].as_str(),
        Some("write-1")
    );
    fs::write(directory.path().join("notes.txt"), "user edit\n").expect("unrelated user edit");
    let restarted = AgentEngine::new(ScriptedProvider::new(vec![]), Config::default());
    let resumed = restarted
        .load_session(directory.path(), session.id.as_str())
        .expect("restart load");
    assert_eq!(resumed.id, session.id);
    // Seed durable verification evidence to prove a successful revert invalidates it.
    let session_path = directory
        .path()
        .join(".medusa/sessions")
        .join(format!("{}.json", session.id));
    let mut persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(&session_path).expect("session snapshot")).expect("json");
    persisted["evidence"] = json!(["verification-before-revert"]);
    fs::write(
        &session_path,
        serde_json::to_vec_pretty(&persisted).unwrap(),
    )
    .expect("seed evidence");
    let preview =
        preview_session_selective_revert(directory.path(), session.id.as_str(), &mutation_id)
            .expect("preview");
    assert_eq!(preview.path, "value.txt");
    let outcome = apply_session_selective_revert(
        directory.path(),
        session.id.as_str(),
        &mutation_id,
        "frontend-revert-1",
        "frontend-control",
    )
    .expect("revert");
    assert_eq!(
        fs::read_to_string(directory.path().join("value.txt")).unwrap(),
        "alpha\nbeta\n"
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("notes.txt")).unwrap(),
        "user edit\n"
    );
    assert_eq!(outcome.mutation_ids.len(), 1);
    let refreshed = restarted
        .load_session(directory.path(), session.id.as_str())
        .expect("session after revert");
    assert!(refreshed.evidence.is_empty());
}

#[test]
fn session_scoped_preview_rejects_cross_session_authority() {
    let directory = tempfile::tempdir().expect("tempdir");
    assert!(
        std::process::Command::new("git")
            .arg("init")
            .arg(directory.path())
            .status()
            .expect("git init")
            .success()
    );
    fs::write(directory.path().join("value.txt"), "before\n").expect("fixture");
    let engine = AgentEngine::new(
        ScriptedProvider::new(vec![response(
            vec![ResponseBlock::ToolUse {
                id: "write-1".into(),
                name: "fs_write".into(),
                input: json!({"path":"value.txt","content":"after\n"}),
            }],
            "tool_use",
        )]),
        Config::default(),
    );
    let mut session = engine
        .create_session(directory.path(), "write".into())
        .expect("session");
    engine.step(&mut session).expect("write step");
    let journal: serde_json::Value = serde_json::from_slice(
        &fs::read(directory.path().join(".medusa/mutation-provenance.json")).unwrap(),
    )
    .unwrap();
    let mutation_id = journal["records"][0]["id"].as_str().unwrap();
    assert!(
        preview_session_selective_revert(directory.path(), "other-session", mutation_id).is_err()
    );
}
