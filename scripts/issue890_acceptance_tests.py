from pathlib import Path
p = Path('crates/medusa-agent/tests/effective_request_manifest.rs')
s = p.read_text()
s = s.replace('use std::sync::{\n', 'use std::{fs, sync::{\n', 1)
s = s.replace('};\n\nuse medusa_agent::{', '}};\n\nuse medusa_agent::{', 1)
s = s.replace('    AgentEngine, AgentExecutionPolicy, StepOutcome, inspect_effective_model_request,\n', '    AgentEngine, AgentExecutionPolicy, AgentSession, StepOutcome, TeamRole,\n    inspect_effective_model_request, record_session_event,\n', 1)
s = s.replace('use medusa_protocol::EventPayload;\n', 'use medusa_protocol::{Actor, EventPayload};\n', 1)
s = s.replace('use medusa_provider::{ModelProvider, ModelRequest, ModelResponse, ResponseBlock, Usage};\n', 'use medusa_provider::{MessageBlock, ModelProvider, ModelRequest, ModelResponse, ResponseBlock, Usage};\n', 1)
append = r'''

fn request_manifests(session: &AgentSession) -> Vec<(String, String)> {
    session
        .events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::ModelRequestStarted {
                request_fingerprint: Some(fingerprint),
                manifest_ref: Some(reference),
                ..
            } => Some((fingerprint.clone(), reference.clone())),
            _ => None,
        })
        .collect()
}

fn request_artifact_path(repo: &std::path::Path, session: &AgentSession, reference: &str) -> std::path::PathBuf {
    let hash = reference
        .strip_prefix("request-content:sha256:")
        .expect("request content reference");
    repo.join(".medusa")
        .join("request-artifacts")
        .join(session.id.as_str())
        .join(format!("{hash}.json"))
}

#[test]
fn action_compaction_and_tool_schema_provenance_are_exact_and_non_repeating() {
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

    record_session_event(
        &mut session,
        Actor::Coordinator,
        EventPayload::SessionActionTranscriptLinked {
            action_id: "delivered-action".to_owned(),
            transcript_event_sequence: 1,
        },
    )
    .expect("link delivered action");
    record_session_event(
        &mut session,
        Actor::User,
        EventPayload::UserFollowupQueued {
            command_id: "queued-only".to_owned(),
            prompt: serde_json::json!({"text":"not delivered yet"}),
        },
    )
    .expect("queue follow-up");
    record_session_event(
        &mut session,
        Actor::Coordinator,
        EventPayload::ConversationCompacted {
            original_messages: 9,
            retained_messages: 4,
            generation: 7,
            source_event_sequences: vec![1, 2, 3],
            preserved_sections: vec!["objective".to_owned()],
        },
    )
    .expect("record compaction");

    engine.step(&mut session).expect("first model step");
    let first_manifest = request_manifests(&session)
        .last()
        .expect("first manifest")
        .1
        .clone();
    let first = inspect_effective_model_request(
        directory.path(),
        session.id.as_str(),
        &first_manifest,
    )
    .expect("inspect first request");
    assert_eq!(first["delivered_action_ids"], serde_json::json!(["delivered-action"]));
    assert_eq!(first["compaction_generation"], serde_json::json!(7));
    assert_eq!(
        first["compaction_source_event_sequences"],
        serde_json::json!([1, 2, 3])
    );
    assert!(
        first["delivered_action_ids"]
            .as_array()
            .expect("delivered ids")
            .iter()
            .all(|id| id != "queued-only")
    );

    let mut tool_names = first["canonical_request"]["tools"]
        .as_array()
        .expect("tool array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    tool_names.sort();
    let mut schema_names = first["tool_schema_fingerprints"]
        .as_object()
        .expect("schema fingerprint map")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    schema_names.sort();
    assert!(!tool_names.is_empty(), "request should advertise read-only tools");
    assert_eq!(tool_names, schema_names);

    engine
        .append_user_message(
            &mut session,
            vec![MessageBlock::Text {
                text: "continue inspection".to_owned(),
            }],
        )
        .expect("append next user turn");
    engine.step(&mut session).expect("second model step");
    let second_manifest = request_manifests(&session)
        .last()
        .expect("second manifest")
        .1
        .clone();
    let second = inspect_effective_model_request(
        directory.path(),
        session.id.as_str(),
        &second_manifest,
    )
    .expect("inspect second request");
    assert_eq!(second["delivered_action_ids"], serde_json::json!([]));
}

#[test]
fn dynamic_turn_instruction_changes_fingerprint_and_is_attributable() {
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

    let mut plain = engine
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("plain session");
    engine.step(&mut plain).expect("plain step");
    let plain_fp = request_manifests(&plain)[0].0.clone();

    let mut instructed = engine
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("instructed session");
    engine
        .step_with_observer_and_context_and_turn_instruction(
            &mut instructed,
            None,
            Some("inspect only README.md"),
            |_| {},
        )
        .expect("instructed step");
    let (instructed_fp, instructed_ref) = request_manifests(&instructed)[0].clone();
    assert_ne!(plain_fp, instructed_fp);
    let audit = inspect_effective_model_request(
        directory.path(),
        instructed.id.as_str(),
        &instructed_ref,
    )
    .expect("inspect instructed request");
    assert!(audit["assembly_provenance"]["turn_instruction"].is_string());
}

#[test]
fn planner_and_implementer_record_different_actual_tool_sets() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let mut config = Config::default();
    config.agent.mode = Mode::Yolo;

    let planner = AgentEngine::new(
        CountingProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        config.clone(),
    )
    .with_execution_policy(AgentExecutionPolicy::for_team_role(TeamRole::Planner));
    let mut planner_session = planner
        .create_session(directory.path(), "plan the change".to_owned())
        .expect("planner session");
    planner.step(&mut planner_session).expect("planner step");
    let planner_ref = request_manifests(&planner_session)[0].1.clone();
    let planner_audit = inspect_effective_model_request(
        directory.path(),
        planner_session.id.as_str(),
        &planner_ref,
    )
    .expect("planner audit");

    let implementer = AgentEngine::new(
        CountingProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        config,
    )
    .with_execution_policy(AgentExecutionPolicy::for_team_role(TeamRole::Implementer));
    let mut implementer_session = implementer
        .create_session(directory.path(), "implement the change".to_owned())
        .expect("implementer session");
    implementer
        .step(&mut implementer_session)
        .expect("implementer step");
    let implementer_ref = request_manifests(&implementer_session)[0].1.clone();
    let implementer_audit = inspect_effective_model_request(
        directory.path(),
        implementer_session.id.as_str(),
        &implementer_ref,
    )
    .expect("implementer audit");

    let planner_tools = planner_audit["tool_schema_fingerprints"]
        .as_object()
        .expect("planner tool fingerprints");
    let implementer_tools = implementer_audit["tool_schema_fingerprints"]
        .as_object()
        .expect("implementer tool fingerprints");
    assert!(!planner_tools.contains_key("fs_write"));
    assert!(implementer_tools.contains_key("fs_write"));
    assert_ne!(planner_tools, implementer_tools);
}

#[test]
fn corrupted_or_missing_protected_request_artifact_fails_closed_precisely() {
    let corrupt_repo = tempfile::tempdir().expect("corrupt repository");
    let mut config = Config::default();
    config.agent.mode = Mode::ReadOnly;
    let corrupt_engine = AgentEngine::new(
        CountingProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        config.clone(),
    );
    let mut corrupt_session = corrupt_engine
        .create_session(corrupt_repo.path(), "inspect".to_owned())
        .expect("corrupt session");
    corrupt_engine
        .step(&mut corrupt_session)
        .expect("corrupt baseline step");
    let corrupt_ref = request_manifests(&corrupt_session)[0].1.clone();
    let baseline = inspect_effective_model_request(
        corrupt_repo.path(),
        corrupt_session.id.as_str(),
        &corrupt_ref,
    )
    .expect("baseline audit");
    let artifact = request_artifact_path(
        corrupt_repo.path(),
        &corrupt_session,
        baseline["request_content_ref"].as_str().expect("content ref"),
    );
    let mut content: serde_json::Value =
        serde_json::from_slice(&fs::read(&artifact).expect("read request artifact"))
            .expect("request json");
    content["system"] = serde_json::json!("tampered system");
    fs::write(&artifact, serde_json::to_vec_pretty(&content).expect("tampered json"))
        .expect("write tampered request artifact");
    let corrupt_error = inspect_effective_model_request(
        corrupt_repo.path(),
        corrupt_session.id.as_str(),
        &corrupt_ref,
    )
    .expect_err("corruption must fail closed");
    let corrupt_debug = format!("{corrupt_error:?}");
    assert!(corrupt_debug.contains("mismatched_components"));
    assert!(corrupt_debug.contains("system"));

    let missing_repo = tempfile::tempdir().expect("missing repository");
    let missing_engine = AgentEngine::new(
        CountingProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        config,
    );
    let mut missing_session = missing_engine
        .create_session(missing_repo.path(), "inspect".to_owned())
        .expect("missing session");
    missing_engine
        .step(&mut missing_session)
        .expect("missing baseline step");
    let missing_ref = request_manifests(&missing_session)[0].1.clone();
    let missing_audit = inspect_effective_model_request(
        missing_repo.path(),
        missing_session.id.as_str(),
        &missing_ref,
    )
    .expect("baseline missing audit");
    let missing_artifact = request_artifact_path(
        missing_repo.path(),
        &missing_session,
        missing_audit["request_content_ref"].as_str().expect("content ref"),
    );
    fs::remove_file(missing_artifact).expect("remove protected artifact");
    let missing_error = inspect_effective_model_request(
        missing_repo.path(),
        missing_session.id.as_str(),
        &missing_ref,
    )
    .expect_err("missing protected artifact must fail closed");
    assert!(
        missing_error
            .to_string()
            .contains("protected request artifact is unavailable or redacted")
    );
}
'''
s += append
p.write_text(s)
