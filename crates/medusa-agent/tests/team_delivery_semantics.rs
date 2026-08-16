use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use medusa_agent::{
    AgentEngine, StepOutcome, TeamRole, TeamRuntime, inspect_effective_model_request,
    record_session_event,
};
use medusa_config::Config;
use medusa_core::MedusaResult;
use medusa_protocol::{Actor, EventPayload};
use medusa_provider::{ModelProvider, ModelRequest, ModelResponse, ResponseBlock, Usage};

struct NoopProvider;

impl ModelProvider for NoopProvider {
    fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
        Ok(text_response("done"))
    }
}

struct ScriptedProvider {
    responses: Arc<Mutex<VecDeque<ModelResponse>>>,
}

impl ModelProvider for ScriptedProvider {
    fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.responses
            .lock()
            .expect("scripted provider lock")
            .pop_front()
            .ok_or_else(|| {
                medusa_core::MedusaError::new(
                    medusa_core::ErrorCode::DependencyUnavailable,
                    medusa_core::ErrorCategory::Execution,
                    "scripted provider exhausted",
                )
            })
    }
}

fn text_response(text: &str) -> ModelResponse {
    ModelResponse {
        response_id: Some("team-delivery".to_owned()),
        stop_reason: Some("stop".to_owned()),
        blocks: vec![ResponseBlock::Text {
            text: text.to_owned(),
        }],
        usage: Usage::default(),
    }
}

fn tool_response() -> ModelResponse {
    ModelResponse {
        response_id: Some("team-delivery-tool".to_owned()),
        stop_reason: Some("tool_use".to_owned()),
        blocks: vec![ResponseBlock::ToolUse {
            id: "read-root".to_owned(),
            name: "fs_read".to_owned(),
            input: serde_json::json!({"path":"."}),
        }],
        usage: Usage::default(),
    }
}

fn team_for_session(
    repo: &std::path::Path,
    team_id: &str,
    session_id: &str,
) -> (TeamRuntime, medusa_agent::TeamMemberContext, medusa_agent::TeamMemberContext) {
    let team = TeamRuntime::create(
        repo.join(format!(".medusa/executions/{team_id}/team.json")),
        team_id,
        vec![
            ("lead".to_owned(), TeamRole::Lead),
            ("worker".to_owned(), TeamRole::Implementer),
        ],
    )
    .expect("team runtime");
    team.start_member("worker", "task-1", session_id)
        .expect("start worker");
    let lead = team.member_context("lead").expect("lead context");
    let worker = team.member_context("worker").expect("worker context");
    (team, lead, worker)
}

fn accepted_action_id(session: &medusa_agent::AgentSession, key: &str) -> String {
    session
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::SessionActionAccepted { action } if action.idempotency_key == key => {
                Some(action.action_id.clone())
            }
            _ => None,
        })
        .expect("accepted team action")
}

#[test]
fn duplicate_team_instruction_replays_after_revision_drift() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let engine = AgentEngine::new(NoopProvider, Config::default());
    let mut session = engine
        .create_session(directory.path(), "implement".to_owned())
        .expect("create session");
    let key = "team-idempotency:worker:1";

    let first = medusa_agent::team::admit_team_instruction(
        directory.path(),
        session.id.as_str(),
        "lead",
        "worker",
        "inspect the transaction boundary",
        key,
    )
    .expect("first admission");

    session = engine
        .load_session(directory.path(), session.id.as_str())
        .expect("reload after admission");
    record_session_event(
        &mut session,
        Actor::Coordinator,
        EventPayload::AssumptionRecorded {
            assumption: "revision advances independently".to_owned(),
            rationale: "exercise idempotent replay".to_owned(),
        },
    )
    .expect("advance revision");

    let replay = medusa_agent::team::admit_team_instruction(
        directory.path(),
        session.id.as_str(),
        "lead",
        "worker",
        "inspect the transaction boundary",
        key,
    )
    .expect("idempotent replay");
    assert_eq!(replay, first);

    let restored = engine
        .load_session(directory.path(), session.id.as_str())
        .expect("restore session");
    let accepted = restored
        .events
        .iter()
        .filter(|event| {
            matches!(
                &event.payload,
                EventPayload::SessionActionAccepted { action } if action.idempotency_key == key
            )
        })
        .count();
    assert_eq!(accepted, 1, "idempotent replay must not append a second action");

    assert!(
        medusa_agent::team::admit_team_instruction(
            directory.path(),
            session.id.as_str(),
            "lead",
            "worker",
            "different content",
            key,
        )
        .is_err(),
        "a reused key with different content must fail closed"
    );
}

#[test]
fn repeated_prompt_assembly_does_not_acknowledge_or_duplicate_instruction() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let engine = AgentEngine::new(NoopProvider, Config::default());
    let session = engine
        .create_session(directory.path(), "implement".to_owned())
        .expect("create session");
    let (_team, lead, worker) =
        team_for_session(directory.path(), "team-prompt-repeat", session.id.as_str());
    lead.execute(
        "team_send_message",
        &serde_json::json!({"recipient":"worker","body":"keep this instruction exact"}),
    )
    .expect("send instruction");

    let before = engine
        .load_session(directory.path(), session.id.as_str())
        .expect("load before prompt assembly");
    let first = worker.prompt_context().expect("first prompt context");
    let second = worker.prompt_context().expect("second prompt context");
    assert_eq!(first, second);
    assert!(first.contains("keep this instruction exact"));

    let after = engine
        .load_session(directory.path(), session.id.as_str())
        .expect("load after prompt assembly");
    assert_eq!(after.events, before.events, "prompt assembly must be observational");
    assert!(!after.events.iter().any(|event| matches!(
        event.payload,
        EventPayload::SessionActionTranscriptLinked { .. }
    )));
}

#[test]
fn team_instruction_is_model_visible_in_exactly_one_effective_request() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let bootstrap = AgentEngine::new(NoopProvider, Config::default());
    let session = bootstrap
        .create_session(directory.path(), "implement".to_owned())
        .expect("create session");
    let (_team, lead, worker) =
        team_for_session(directory.path(), "team-model-visible", session.id.as_str());
    lead.execute(
        "team_send_message",
        &serde_json::json!({"recipient":"worker","body":"inspect exactly once"}),
    )
    .expect("send instruction");

    let admitted = bootstrap
        .load_session(directory.path(), session.id.as_str())
        .expect("load admitted session");
    let key = admitted
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::SessionActionAccepted { action } if action.source == "team:lead:worker" => {
                Some(action.idempotency_key.clone())
            }
            _ => None,
        })
        .expect("team action key");
    let action_id = accepted_action_id(&admitted, &key);

    let responses = Arc::new(Mutex::new(VecDeque::from([
        tool_response(),
        text_response("complete"),
    ])));
    let engine = AgentEngine::new(
        ScriptedProvider {
            responses: Arc::clone(&responses),
        },
        Config::default(),
    )
    .with_team_context(worker.clone());
    let mut running = engine
        .load_session(directory.path(), session.id.as_str())
        .expect("restart worker session");

    assert_eq!(
        engine.step(&mut running).expect("first model step"),
        StepOutcome::Continue
    );
    let first_manifest = running
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ModelRequestStarted {
                manifest_ref: Some(reference),
                ..
            } => Some(reference.clone()),
            _ => None,
        })
        .expect("first request manifest");
    let audit = inspect_effective_model_request(
        directory.path(),
        session.id.as_str(),
        &first_manifest,
    )
    .expect("inspect first request");
    assert_eq!(
        audit["delivered_action_ids"],
        serde_json::json!([action_id.clone()])
    );
    assert!(
        !worker
            .prompt_context()
            .expect("post-request prompt context")
            .contains("inspect exactly once"),
        "persisted request manifest is the model-visible receipt"
    );

    assert_eq!(
        engine.step(&mut running).expect("second model step"),
        StepOutcome::TurnComplete
    );
    let manifests = running
        .events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::ModelRequestStarted {
                manifest_ref: Some(reference),
                ..
            } => Some(reference.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(manifests.len() >= 2);
    let occurrences = manifests
        .iter()
        .map(|reference| {
            inspect_effective_model_request(directory.path(), session.id.as_str(), reference)
                .expect("inspect request")
        })
        .filter(|audit| {
            audit["delivered_action_ids"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|id| id == &serde_json::json!(action_id)))
        })
        .count();
    assert_eq!(occurrences, 1, "one team action must bind to one effective request");
}
