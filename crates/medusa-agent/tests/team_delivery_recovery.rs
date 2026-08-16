use std::fs;

use medusa_agent::{
    AgentEngine, StepOutcome, TeamRole, TeamRuntime, compact_session,
    inspect_effective_model_request,
};
use medusa_config::Config;
use medusa_core::MedusaResult;
use medusa_protocol::EventPayload;
use medusa_provider::{ModelProvider, ModelRequest, ModelResponse, ResponseBlock, Usage};
use serde_json::json;

struct NoopProvider;

impl ModelProvider for NoopProvider {
    fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
        Ok(ModelResponse {
            response_id: Some("team-recovery".to_owned()),
            stop_reason: Some("stop".to_owned()),
            blocks: vec![ResponseBlock::Text {
                text: "done".to_owned(),
            }],
            usage: Usage::default(),
        })
    }
}

fn team_for_session(
    repo: &std::path::Path,
    team_id: &str,
    session_id: &str,
) -> (
    TeamRuntime,
    medusa_agent::TeamMemberContext,
    medusa_agent::TeamMemberContext,
) {
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

fn accepted_action_id(session: &medusa_agent::AgentSession) -> String {
    session
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::SessionActionAccepted { action }
                if action.source == "team:lead:worker" =>
            {
                Some(action.action_id.clone())
            }
            _ => None,
        })
        .expect("accepted team action")
}

fn first_manifest_ref(session: &medusa_agent::AgentSession) -> String {
    session
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ModelRequestStarted {
                manifest_ref: Some(reference),
                ..
            } => Some(reference.clone()),
            _ => None,
        })
        .expect("effective request manifest")
}

#[test]
fn legacy_undelivered_boolean_migrates_to_unknown_queue_idempotently() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let path = directory
        .path()
        .join(".medusa/executions/legacy-undelivered/team.json");
    fs::create_dir_all(path.parent().expect("team state parent")).expect("create parent");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "team_id": "legacy-undelivered",
            "members": {
                "lead": {
                    "id": "lead",
                    "role": "lead",
                    "lifecycle": "running",
                    "current_task": null,
                    "session_id": null
                },
                "worker": {
                    "id": "worker",
                    "role": "implementer",
                    "lifecycle": "idle",
                    "current_task": null,
                    "session_id": null
                }
            },
            "messages": [{
                "sequence": 1,
                "from": "lead",
                "to": "worker",
                "body": "legacy unresolved delivery",
                "delivered": false
            }],
            "next_sequence": 2
        }))
        .expect("legacy json"),
    )
    .expect("write legacy team state");

    let first = TeamRuntime::load(&path).expect("migrate legacy team state");
    drop(first);
    let migrated_once = fs::read_to_string(&path).expect("read migrated team state");
    assert!(migrated_once.contains("legacy_queued"));
    assert!(!migrated_once.contains("model_visible"));
    assert!(!migrated_once.contains("\"delivered\""));

    let second = TeamRuntime::load(&path).expect("reload migrated team state");
    drop(second);
    let migrated_twice = fs::read_to_string(path).expect("read reloaded team state");
    assert_eq!(migrated_twice, migrated_once, "migration must be idempotent");
}

#[test]
fn pending_team_instruction_survives_compaction_with_same_identity() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let engine = AgentEngine::new(NoopProvider, Config::default());
    let session = engine
        .create_session(directory.path(), "implement".to_owned())
        .expect("create session");
    let (_team, lead, worker) =
        team_for_session(directory.path(), "team-compact-pending", session.id.as_str());
    lead.execute(
        "team_send_message",
        &json!({"recipient":"worker","body":"preserve through compaction"}),
    )
    .expect("send instruction");

    let mut restored = engine
        .load_session(directory.path(), session.id.as_str())
        .expect("load accepted session");
    let action_id = accepted_action_id(&restored);
    compact_session(&mut restored, Some("preserve worker instruction identity"))
        .expect("compact session");

    let after = engine
        .load_session(directory.path(), session.id.as_str())
        .expect("reload compacted session");
    assert_eq!(accepted_action_id(&after), action_id);
    assert_eq!(
        after
            .events
            .iter()
            .filter(|event| matches!(event.payload, EventPayload::SessionActionAccepted { .. }))
            .count(),
        1,
        "compaction must not duplicate accepted actions"
    );
    let context = worker.prompt_context().expect("worker prompt context");
    assert!(context.contains("preserve through compaction"));
    assert!(context.contains(&action_id));
}

#[test]
fn model_visible_team_instruction_stays_consumed_after_compaction() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let bootstrap = AgentEngine::new(NoopProvider, Config::default());
    let session = bootstrap
        .create_session(directory.path(), "implement".to_owned())
        .expect("create session");
    let (_team, lead, worker) =
        team_for_session(directory.path(), "team-compact-visible", session.id.as_str());
    lead.execute(
        "team_send_message",
        &json!({"recipient":"worker","body":"consume once before compaction"}),
    )
    .expect("send instruction");

    let admitted = bootstrap
        .load_session(directory.path(), session.id.as_str())
        .expect("load admitted session");
    let action_id = accepted_action_id(&admitted);
    let engine = AgentEngine::new(NoopProvider, Config::default()).with_team_context(worker.clone());
    let mut running = engine
        .load_session(directory.path(), session.id.as_str())
        .expect("load worker session");
    assert_eq!(
        engine.step(&mut running).expect("model step"),
        StepOutcome::TurnComplete
    );

    let manifest_ref = first_manifest_ref(&running);
    let audit = inspect_effective_model_request(
        directory.path(),
        session.id.as_str(),
        &manifest_ref,
    )
    .expect("inspect effective request");
    assert_eq!(audit["delivered_action_ids"], json!([action_id.clone()]));
    assert!(
        !worker
            .prompt_context()
            .expect("post-request context")
            .contains("consume once before compaction")
    );

    compact_session(&mut running, Some("compact after model visibility"))
        .expect("compact model-visible session");
    let after = bootstrap
        .load_session(directory.path(), session.id.as_str())
        .expect("reload compacted session");
    assert_eq!(accepted_action_id(&after), action_id);
    assert!(
        !worker
            .prompt_context()
            .expect("post-compaction context")
            .contains("consume once before compaction"),
        "compaction must not make a model-visible action pending again"
    );
}