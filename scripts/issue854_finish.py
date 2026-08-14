from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    if old not in text:
        raise SystemExit(f"missing patch anchor in {path}: {old[:80]!r}")
    file.write_text(text.replace(old, new, 1))


tx = Path("crates/medusa-agent/src/transaction.rs")
text = tx.read_text()
anchor = "pub fn preview_selective_revert(repo: &Path, mutation_id: &str) -> MedusaResult<RevertPreview> {"
helper = '''pub fn preview_session_selective_revert(
    repo: &Path,
    session_id: &str,
    mutation_id: &str,
) -> MedusaResult<RevertPreview> {
    let journal = load_provenance(repo)?;
    let record = journal
        .records
        .iter()
        .find(|record| record.id == mutation_id)
        .ok_or_else(|| provenance_boundary_error("mutation provenance record is missing"))?;
    if record.context.session_id != session_id {
        return Err(provenance_boundary_error(
            "mutation provenance does not belong to the requested session",
        ));
    }
    preview_selective_revert(repo, mutation_id)
}

pub fn apply_session_selective_revert(
    repo: &Path,
    session_id: &str,
    mutation_id: &str,
    activity_id: &str,
    actor: &str,
) -> MedusaResult<TransactionOutcome> {
    preview_session_selective_revert(repo, session_id, mutation_id)?;
    let sequence = next_mutation_sequence(repo, session_id)?;
    let occurred_at_unix_ms = i64::try_from(
        time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000,
    )
    .map_err(|_| provenance_boundary_error("mutation timestamp overflow"))?;
    let context = MutationContext {
        session_id: session_id.to_owned(),
        task_step_id: None,
        activity_id: activity_id.to_owned(),
        actor: actor.to_owned(),
        sequence,
        occurred_at_unix_ms,
    };
    apply_selective_revert(repo, mutation_id, &context)
}

'''
if helper not in text:
    if anchor not in text:
        raise SystemExit("missing selective revert anchor")
    tx.write_text(text.replace(anchor, helper + anchor, 1))

replace_once(
    "crates/medusa-agent/src/lib.rs",
    "    apply_atomic, apply_atomic_with_context, apply_selective_revert, preview,\n    preview_selective_revert,\n};",
    "    apply_atomic, apply_atomic_with_context, apply_selective_revert,\n    apply_session_selective_revert, preview, preview_selective_revert,\n    preview_session_selective_revert,\n};",
)

command = Path("crates/medusa-protocol/src/frontend/command.rs")
text = command.read_text()
command_anchor = '''    RecoveryAction {
        operation: String,
        checkpoint_id: Option<String>,
        confirmed_destructive_effects: bool,
    },'''
if "PreviewSelectiveRevert" not in text:
    if command_anchor not in text:
        raise SystemExit("missing recovery command anchor")
    text = text.replace(
        command_anchor,
        command_anchor
        + '''
    PreviewSelectiveRevert {
        mutation_id: String,
    },
    ApplySelectiveRevert {
        mutation_id: String,
    },''',
        1,
    )
validation_anchor = '''            Self::RecoveryAction { operation, .. } if operation.trim().is_empty() => {
                Err("recovery operation cannot be empty")
            }'''
if "mutation id cannot be empty" not in text:
    if validation_anchor not in text:
        raise SystemExit("missing recovery validation anchor")
    text = text.replace(
        validation_anchor,
        validation_anchor
        + '''
            Self::PreviewSelectiveRevert { mutation_id }
            | Self::ApplySelectiveRevert { mutation_id }
                if mutation_id.trim().is_empty() =>
            {
                Err("mutation id cannot be empty")
            }''',
        1,
    )
command.write_text(text)

front = Path("crates/medusa-daemon/src/frontend_control.rs")
text = front.read_text()
status_anchor = '''    Status {
        session_id: String,
        runtime_active: bool,
        busy: bool,
        journal_cursor: u64,
        latest_checkpoint_cursor: Option<u64>,
        replay_equivalent: bool,
    },'''
if "SelectiveRevertPreview" not in text:
    if status_anchor not in text:
        raise SystemExit("missing status result anchor")
    text = text.replace(
        status_anchor,
        status_anchor
        + '''
    SelectiveRevertPreview {
        session_id: String,
        mutation_id: String,
        path: String,
        start_byte: usize,
        remove_len: usize,
        restore_len: usize,
    },
    SelectiveRevertApplied {
        session_id: String,
        mutation_id: String,
        inverse_mutation_ids: Vec<String>,
    },''',
        1,
    )
recovery_anchor = '''            FrontendCommand::RecoveryAction {
                operation,
                checkpoint_id,
                confirmed_destructive_effects,
            } => {'''
cases = '''            FrontendCommand::PreviewSelectiveRevert { mutation_id } => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                let preview = medusa_agent::preview_session_selective_revert(
                    &self.repo,
                    &session_id,
                    mutation_id,
                )
                .map_err(|error| FrontendControlError::InvalidCommand(error.to_string()))?;
                Ok(FrontendControlResult::SelectiveRevertPreview {
                    session_id,
                    mutation_id: preview.mutation_id,
                    path: preview.path,
                    start_byte: preview.start_byte,
                    remove_len: preview.remove_len,
                    restore_len: preview.restore_len,
                })
            }
            FrontendCommand::ApplySelectiveRevert { mutation_id } => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                let outcome = medusa_agent::apply_session_selective_revert(
                    &self.repo,
                    &session_id,
                    mutation_id,
                    &envelope.command_id,
                    "frontend-control",
                )
                .map_err(|error| FrontendControlError::InvalidCommand(error.to_string()))?;
                Ok(FrontendControlResult::SelectiveRevertApplied {
                    session_id,
                    mutation_id: mutation_id.clone(),
                    inverse_mutation_ids: outcome.mutation_ids,
                })
            }
'''
if "FrontendCommand::PreviewSelectiveRevert" not in text:
    if recovery_anchor not in text:
        raise SystemExit("missing recovery execute anchor")
    text = text.replace(recovery_anchor, cases + recovery_anchor, 1)
front.write_text(text)

cargo = Path("crates/medusa-daemon/Cargo.toml")
text = cargo.read_text()
if "medusa-agent.workspace = true" not in text:
    if "[dependencies]\n" not in text:
        raise SystemExit("missing daemon dependencies section")
    cargo.write_text(text.replace("[dependencies]\n", "[dependencies]\nmedusa-agent.workspace = true\n", 1))

test = Path("crates/medusa-agent/tests/production_mutation_revert.rs")
test.write_text(r'''use std::{collections::VecDeque, fs, sync::Mutex};
use medusa_agent::{AgentEngine, StepOutcome, apply_session_selective_revert, preview_session_selective_revert};
use medusa_config::Config;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_provider::{ModelProvider, ModelRequest, ModelResponse, ResponseBlock, Usage};
use serde_json::json;

struct ScriptedProvider { responses: Mutex<VecDeque<ModelResponse>> }
impl ScriptedProvider { fn new(responses: Vec<ModelResponse>) -> Self { Self { responses: Mutex::new(responses.into()) } } }
impl ModelProvider for ScriptedProvider {
    fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.responses.lock().expect("provider lock").pop_front().ok_or_else(|| MedusaError::new(ErrorCode::DependencyUnavailable, ErrorCategory::Internal, "scripted response exhausted"))
    }
}
fn response(blocks: Vec<ResponseBlock>, stop_reason: &str) -> ModelResponse {
    ModelResponse { response_id: Some("fixture".into()), stop_reason: Some(stop_reason.into()), blocks, usage: Usage::default() }
}

#[test]
fn ordinary_dispatch_write_survives_restart_and_selectively_reverts() {
    let directory = tempfile::tempdir().expect("tempdir");
    fs::write(directory.path().join("value.txt"), "alpha\nbeta\n").expect("fixture");
    let engine = AgentEngine::new(ScriptedProvider::new(vec![response(vec![ResponseBlock::ToolUse { id: "write-1".into(), name: "fs_write".into(), input: json!({"path":"value.txt","content":"alpha\nBETA\n"}) }], "tool_use")]), Config::default());
    let mut session = engine.create_session(directory.path(), "change beta".into()).expect("session");
    assert_eq!(engine.step(&mut session).expect("write step"), StepOutcome::Continue);
    let journal: serde_json::Value = serde_json::from_slice(&fs::read(directory.path().join(".medusa/mutation-provenance.json")).expect("journal")).expect("json");
    let mutation_id = journal["records"][0]["id"].as_str().expect("mutation id").to_owned();
    assert_eq!(journal["records"][0]["context"]["session_id"].as_str(), Some(session.id.as_str()));
    assert_eq!(journal["records"][0]["context"]["activity_id"].as_str(), Some("write-1"));
    fs::write(directory.path().join("notes.txt"), "user edit\n").expect("unrelated user edit");
    let restarted = AgentEngine::new(ScriptedProvider::new(vec![]), Config::default());
    let resumed = restarted.load_session(directory.path(), session.id.as_str()).expect("restart load");
    assert_eq!(resumed.id, session.id);
    let preview = preview_session_selective_revert(directory.path(), session.id.as_str(), &mutation_id).expect("preview");
    assert_eq!(preview.path, "value.txt");
    let outcome = apply_session_selective_revert(directory.path(), session.id.as_str(), &mutation_id, "frontend-revert-1", "frontend-control").expect("revert");
    assert_eq!(fs::read_to_string(directory.path().join("value.txt")).unwrap(), "alpha\nbeta\n");
    assert_eq!(fs::read_to_string(directory.path().join("notes.txt")).unwrap(), "user edit\n");
    assert_eq!(outcome.mutation_ids.len(), 1);
}

#[test]
fn session_scoped_preview_rejects_cross_session_authority() {
    let directory = tempfile::tempdir().expect("tempdir");
    fs::write(directory.path().join("value.txt"), "before\n").expect("fixture");
    let engine = AgentEngine::new(ScriptedProvider::new(vec![response(vec![ResponseBlock::ToolUse { id: "write-1".into(), name: "fs_write".into(), input: json!({"path":"value.txt","content":"after\n"}) }], "tool_use")]), Config::default());
    let mut session = engine.create_session(directory.path(), "write".into()).expect("session");
    engine.step(&mut session).expect("write step");
    let journal: serde_json::Value = serde_json::from_slice(&fs::read(directory.path().join(".medusa/mutation-provenance.json")).unwrap()).unwrap();
    let mutation_id = journal["records"][0]["id"].as_str().unwrap();
    assert!(preview_session_selective_revert(directory.path(), "other-session", mutation_id).is_err());
}
''')
