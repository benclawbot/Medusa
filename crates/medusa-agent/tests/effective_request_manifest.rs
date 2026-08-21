use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use medusa_agent::{
    AgentEngine, AgentExecutionPolicy, AgentSession, StepOutcome, TeamRole,
    inspect_effective_model_request, record_session_event,
};
use medusa_config::{Config, Mode};
use medusa_core::MedusaResult;
use medusa_protocol::{Actor, EventPayload};
use medusa_provider::{
    MessageBlock, ModelProvider, ModelRequest, ModelResponse, ResponseBlock, Usage,
};

struct CountingProvider {
    calls: Arc<AtomicUsize>,
}

impl ModelProvider for CountingProvider {
    fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ModelResponse {
            response_id: Some("manifest-test".to_owned()),
            stop_reason: Some("stop".to_owned()),
            blocks: vec![ResponseBlock::Text {
                text: "done".to_owned(),
            }],
            usage: Usage::default(),
        })
    }
}

#[test]
fn effective_request_is_persisted_before_start_and_auditable_after_restart() {
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
    assert_eq!(
        engine.step(&mut session).expect("model step"),
        StepOutcome::TurnComplete
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let (start_sequence, request_id, request_fingerprint, manifest_ref) = session
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ModelRequestStarted {
                request_id: Some(request_id),
                request_fingerprint: Some(request_fingerprint),
                manifest_ref: Some(manifest_ref),
                ..
            } => Some((
                event.sequence,
                request_id.clone(),
                request_fingerprint.clone(),
                manifest_ref.clone(),
            )),
            _ => None,
        })
        .expect("request start with durable manifest");
    let response_sequence = session
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ModelResponseReceived {
                request_id: Some(response_request_id),
                request_fingerprint: Some(response_fingerprint),
                ..
            } if response_request_id == &request_id
                && response_fingerprint == &request_fingerprint =>
            {
                Some(event.sequence)
            }
            _ => None,
        })
        .expect("linked response");
    assert!(start_sequence < response_sequence);

    drop(engine);
    let audit =
        inspect_effective_model_request(directory.path(), session.id.as_str(), &manifest_ref)
            .expect("reconstruct after restart");
    assert_eq!(audit["healthy"], serde_json::json!(true));
    assert_eq!(audit["request_id"], serde_json::json!(request_id));
    assert_eq!(
        audit["request_fingerprint"],
        serde_json::json!(request_fingerprint)
    );
    assert!(audit["canonical_request"]["system"].is_string());
    assert!(audit["canonical_request"]["messages"].is_array());
    assert!(audit["canonical_request"]["tools"].is_array());
    assert_eq!(audit["reconstruction"]["status"], "source_bound");
    assert!(audit["reconstruction"]["source_events_fingerprint"].is_string());
    assert_eq!(audit["reconstruction_receipt"]["schema_version"], 1);
    assert_eq!(
        audit["reconstruction_receipt"]["assembler_version"],
        "effective-request-reconstructor-v1"
    );
    assert_eq!(audit["reconstruction_receipt"]["content_match"], true);
    assert_eq!(
        audit["reconstruction_receipt"]["source_status"],
        "source_bound"
    );
}

#[test]
fn session_persists_runtime_configuration_binding_for_resume() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let mut config = Config::default();
    config.agent.mode = Mode::ReadOnly;
    let binding = serde_json::json!({
        "schema_version": 1,
        "fingerprint": "runtime-config-session-fingerprint",
        "config": {"provider": "test", "model": "test-model"}
    });
    let engine = AgentEngine::new(
        CountingProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        config,
    )
    .with_runtime_config_binding(1, "runtime-config-session-fingerprint", binding.clone());

    let session = engine
        .create_session(directory.path(), "persist the runtime binding".to_owned())
        .expect("create session");
    let recorded = session
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::RuntimeConfigurationBound {
                schema_version,
                fingerprint,
                snapshot,
            } => Some((*schema_version, fingerprint.clone(), snapshot.clone())),
            _ => None,
        })
        .expect("runtime configuration binding event");
    assert_eq!(recorded.0, 1);
    assert_eq!(recorded.1, "runtime-config-session-fingerprint");
    assert_eq!(recorded.2, binding);

    let loaded = engine
        .load_session(directory.path(), session.id.as_str())
        .expect("load session");
    assert!(loaded.events.iter().any(|event| {
        matches!(
            event.payload,
            EventPayload::RuntimeConfigurationBound { .. }
        )
    }));
}

#[test]
fn invalid_runtime_configuration_binding_fails_before_session_bootstrap() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let mut config = Config::default();
    config.agent.mode = Mode::ReadOnly;
    let engine = AgentEngine::new(
        CountingProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        config,
    )
    .with_runtime_config_binding(0, "", serde_json::Value::Null);

    let error = engine
        .create_session(directory.path(), "reject invalid binding".to_owned())
        .expect_err("invalid binding must fail closed");
    assert_eq!(error.code, medusa_core::ErrorCode::InvalidConfiguration);
    assert!(!directory.path().join(".medusa/sessions").exists());
}

#[test]
fn execution_policy_and_ambient_config_are_bound_without_replay_drift() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let mut config = Config::default();
    config.agent.mode = Mode::ReadOnly;

    let first_calls = Arc::new(AtomicUsize::new(0));
    let first = AgentEngine::new(
        CountingProvider {
            calls: Arc::clone(&first_calls),
        },
        config.clone(),
    )
    .with_execution_policy(
        AgentExecutionPolicy::unrestricted().with_allowed_write_paths(["src".to_owned()]),
    );
    let mut first_session = first
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("first session");
    first.step(&mut first_session).expect("first step");
    let (first_fp, first_manifest) = first_session
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ModelRequestStarted {
                request_fingerprint: Some(fp),
                manifest_ref: Some(reference),
                ..
            } => Some((fp.clone(), reference.clone())),
            _ => None,
        })
        .expect("first manifest");

    let second_calls = Arc::new(AtomicUsize::new(0));
    let second = AgentEngine::new(
        CountingProvider {
            calls: Arc::clone(&second_calls),
        },
        config,
    )
    .with_execution_policy(
        AgentExecutionPolicy::unrestricted().with_allowed_write_paths(["tests".to_owned()]),
    );
    let mut second_session = second
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("second session");
    second.step(&mut second_session).expect("second step");
    let second_fp = second_session
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ModelRequestStarted {
                request_fingerprint: Some(fp),
                ..
            } => Some(fp.clone()),
            _ => None,
        })
        .expect("second fingerprint");
    assert_ne!(
        first_fp, second_fp,
        "execution policy must affect the stable request fingerprint"
    );

    let audit = inspect_effective_model_request(
        directory.path(),
        first_session.id.as_str(),
        &first_manifest,
    )
    .expect("historical request remains reconstructable");
    assert_eq!(audit["healthy"], serde_json::json!(true));
    assert_eq!(
        audit["execution_policy"]["allowed_write_paths"],
        serde_json::json!(["src"]),
    );
}

#[test]
fn runtime_configuration_fingerprint_is_bound_to_effective_request_evidence() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let calls = Arc::new(AtomicUsize::new(0));
    let mut config = Config::default();
    config.agent.mode = Mode::ReadOnly;
    let engine = AgentEngine::new(
        CountingProvider {
            calls: Arc::clone(&calls),
        },
        config,
    )
    .with_runtime_config_fingerprint("runtime-config-test-fingerprint");
    let mut session = engine
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("create session");
    engine.step(&mut session).expect("model step");

    let manifest = session
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ModelRequestStarted {
                manifest_ref: Some(reference),
                ..
            } => Some(reference.clone()),
            _ => None,
        })
        .expect("request manifest");
    let audit = inspect_effective_model_request(directory.path(), session.id.as_str(), &manifest)
        .expect("inspect request");
    assert_eq!(
        audit["assembly_provenance"]["runtime_config_fingerprint"],
        "runtime-config-test-fingerprint"
    );
}

#[test]
fn model_experience_contract_is_bound_to_effective_request_evidence() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let mut config = Config::default();
    config.agent.mode = Mode::ReadOnly;
    let engine = AgentEngine::new(
        CountingProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        config,
    );
    let mut session = engine
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("create session");
    engine.step(&mut session).expect("model step");

    let manifest = session
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ModelRequestStarted {
                manifest_ref: Some(reference),
                ..
            } => Some(reference.clone()),
            _ => None,
        })
        .expect("request manifest");
    let audit = inspect_effective_model_request(directory.path(), session.id.as_str(), &manifest)
        .expect("inspect request");
    let provenance = &audit["assembly_provenance"];
    assert!(provenance["model_experience_contract"].is_string());
    assert!(provenance["model_experience_component:system"].is_string());
    assert!(provenance["model_experience_component:tools"].is_string());
    assert!(provenance["model_experience_estimated_tokens"].is_string());
    assert!(provenance["model_experience_total_bytes"].is_string());
    assert!(provenance["model_experience_stable_prefix_bytes"].is_string());
    assert_eq!(provenance["model_experience_cache"], "unknown");
}

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

fn request_artifact_path(
    repo: &std::path::Path,
    session: &AgentSession,
    reference: &str,
) -> std::path::PathBuf {
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
    let first =
        inspect_effective_model_request(directory.path(), session.id.as_str(), &first_manifest)
            .expect("inspect first request");
    assert_eq!(
        first["delivered_action_ids"],
        serde_json::json!(["delivered-action"])
    );
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
    assert!(
        !tool_names.is_empty(),
        "request should advertise read-only tools"
    );
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
    let second =
        inspect_effective_model_request(directory.path(), session.id.as_str(), &second_manifest)
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
    let audit =
        inspect_effective_model_request(directory.path(), instructed.id.as_str(), &instructed_ref)
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
        baseline["request_content_ref"]
            .as_str()
            .expect("content ref"),
    );
    let mut content: serde_json::Value =
        serde_json::from_slice(&fs::read(&artifact).expect("read request artifact"))
            .expect("request json");
    content["system"] = serde_json::json!("tampered system");
    fs::write(
        &artifact,
        serde_json::to_vec_pretty(&content).expect("tampered json"),
    )
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
        missing_audit["request_content_ref"]
            .as_str()
            .expect("content ref"),
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

#[test]
fn provider_credentials_and_endpoint_are_absent_from_request_authority() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let secret = "sk-issue890-never-persist-this";
    let endpoint = "https://signed.example.invalid/path?token=never-persist";
    let mut config = Config::default();
    config.agent.mode = Mode::ReadOnly;
    config.model.auth = secret.to_owned();
    config.model.base_url = Some(endpoint.to_owned());
    let engine = AgentEngine::new(
        CountingProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        config,
    );
    let mut session = engine
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("create session");
    engine.step(&mut session).expect("model step");
    let (_, manifest_ref) = request_manifests(&session)[0].clone();
    let audit =
        inspect_effective_model_request(directory.path(), session.id.as_str(), &manifest_ref)
            .expect("inspect request");
    let request_path = request_artifact_path(
        directory.path(),
        &session,
        audit["request_content_ref"].as_str().expect("content ref"),
    );
    let manifest_hash = manifest_ref
        .strip_prefix("request-manifest:sha256:")
        .expect("manifest reference");
    let manifest_path = directory
        .path()
        .join(".medusa/request-manifests")
        .join(session.id.as_str())
        .join(format!("{manifest_hash}.json"));
    let authority = format!(
        "{}\n{}",
        fs::read_to_string(request_path).expect("request artifact"),
        fs::read_to_string(manifest_path).expect("manifest artifact")
    );
    assert!(!authority.contains(secret));
    assert!(!authority.contains(endpoint));
    assert!(!authority.contains("token=never-persist"));
}

#[test]
fn immutable_manifest_persistence_conflict_prevents_provider_invocation() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let mut config = Config::default();
    config.agent.mode = Mode::ReadOnly;

    let baseline_engine = AgentEngine::new(
        CountingProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        config.clone(),
    );
    let mut baseline_session = baseline_engine
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("baseline session");
    baseline_engine
        .step(&mut baseline_session)
        .expect("baseline model step");
    let baseline_ref = request_manifests(&baseline_session)[0].1.clone();
    let baseline_audit = inspect_effective_model_request(
        directory.path(),
        baseline_session.id.as_str(),
        &baseline_ref,
    )
    .expect("baseline audit");
    let request_hash = baseline_audit["request_content_ref"]
        .as_str()
        .and_then(|reference| reference.strip_prefix("request-content:sha256:"))
        .expect("request content hash")
        .to_owned();

    let blocked_calls = Arc::new(AtomicUsize::new(0));
    let blocked_engine = AgentEngine::new(
        CountingProvider {
            calls: Arc::clone(&blocked_calls),
        },
        config,
    );
    let mut blocked_session = blocked_engine
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("blocked session");
    let conflicting = directory
        .path()
        .join(".medusa/request-artifacts")
        .join(blocked_session.id.as_str())
        .join(format!("{request_hash}.json"));
    fs::create_dir_all(conflicting.parent().expect("artifact parent"))
        .expect("create artifact parent");
    fs::write(&conflicting, b"conflicting immutable bytes").expect("seed immutable conflict");

    let error = blocked_engine
        .step(&mut blocked_session)
        .expect_err("manifest persistence conflict must block the call");
    assert!(
        !error.to_string().trim().is_empty(),
        "persistence failure must return an error"
    );
    assert_eq!(
        blocked_calls.load(Ordering::SeqCst),
        0,
        "provider must not be invoked when request authority cannot persist"
    );
    assert!(
        blocked_session
            .events
            .iter()
            .all(|event| !matches!(event.payload, EventPayload::ModelRequestStarted { .. })),
        "request start must not be journaled after a persistence failure"
    );
}
