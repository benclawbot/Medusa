use std::{fs, path::Path, sync::mpsc, thread};

use medusa_agent::{AgentPlanStep, AgentPlanStepStatus, AgentUpdate};
use medusa_core::{CorrelationId, SessionId};
use medusa_protocol::{Actor, EventEnvelope, EventPayload};
use medusa_provider::{ImageSource, Message, MessageBlock, Role};
use serde_json::json;
use tempfile::tempdir;
use time::OffsetDateTime;

use crate::prompt::{FileAttachment, ImageAttachment, PromptAttachment};

use super::support::{
    UpdateState, discover_skills, forward_update, load_selected_skill, message_blocks,
    model_configuration_details, tool_title,
};
use super::*;
use crate::coordination::production_orchestrator;

#[test]
fn general_chat_requests_skip_repository_work_but_attachments_stay_explicit() {
    assert!(is_general_chat_request("hey", 0));
    assert!(is_general_chat_request("what can you do?", 0));
    assert!(!is_general_chat_request("fix the login bug", 0));
    assert!(!is_general_chat_request("hey", 1));
}

#[test]
fn general_chat_preparation_does_not_scan_or_capture_repository_state() {
    let draft = PromptDraft {
        text: "hey".to_owned(),
        ..PromptDraft::default()
    };

    assert!(!should_capture_review_baseline_for_plan(true, false, false));
    let plan = execution_plan_for_prompt(Path::new("C:/profile-root"), &draft, true)
        .expect("general chat plan");
    assert_eq!(plan.mode, production_orchestrator::ExecutionMode::Direct);
    assert!(plan.planning.scope.effective.is_empty());
}

#[test]
fn project_conversation_does_not_capture_a_review_baseline() {
    assert!(!should_capture_review_baseline_for_plan(
        false, false, false
    ));
    assert!(should_capture_review_baseline_for_plan(false, false, true));
    assert!(!should_capture_review_baseline_for_plan(true, false, true));
    assert!(!should_capture_review_baseline_for_plan(false, true, true));
}

#[test]
fn command_processing_does_not_wait_for_capability_discovery() {
    use std::{
        sync::{Arc, Mutex, mpsc},
        thread,
        time::Duration,
    };

    let directory = tempdir().expect("temporary directory");
    let state = RuntimeState::load(directory.path().to_path_buf()).expect("runtime state");
    let (command_tx, command_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let submission = Arc::new(Mutex::new(SubmissionState::default()));
    let (discovery_started_tx, discovery_started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let worker = thread::spawn(move || {
        worker_loop_with_discovery(state, command_rx, event_tx, cancel, submission, move |_| {
            discovery_started_tx
                .send(())
                .expect("signal discovery start");
            release_rx.recv().expect("release discovery");
            RuntimeEvent::Notice {
                title: "Runtime capabilities".to_owned(),
                details: vec!["ready".to_owned()],
            }
        });
    });

    assert!(matches!(
        event_rx.recv_timeout(Duration::from_secs(1)),
        Ok(RuntimeEvent::Settings { .. })
    ));
    discovery_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("discovery started");

    command_tx
        .send(RuntimeCommand::Slash(SlashCommand::Help))
        .expect("send help command");
    assert!(matches!(
        event_rx.recv_timeout(Duration::from_secs(1)),
        Ok(RuntimeEvent::Notice { title, .. }) if title == "Slash commands"
    ));

    release_tx.send(()).expect("release discovery");
    assert!(matches!(
        event_rx.recv_timeout(Duration::from_secs(1)),
        Ok(RuntimeEvent::Notice { title, .. }) if title == "Runtime capabilities"
    ));
    command_tx
        .send(RuntimeCommand::Shutdown)
        .expect("stop worker");
    worker.join().expect("worker joins");
}

#[test]
fn resumed_session_rejects_a_changed_effective_runtime_configuration() {
    let current = (
        1_u16,
        "current-fingerprint".to_owned(),
        json!({"schema_version": 1, "fingerprint": "current-fingerprint"}),
    );
    let persisted = (
        1_u16,
        "persisted-fingerprint".to_owned(),
        json!({"schema_version": 1, "fingerprint": "persisted-fingerprint"}),
    );

    let error = validate_session_runtime_config_binding(Some(&current), Some(&persisted))
        .expect_err("a resumed session must not adopt changed runtime defaults");
    assert!(
        error
            .to_string()
            .contains("different runtime configuration")
    );
}

#[test]
fn invalid_repository_runtime_configuration_fails_before_runtime_startup() {
    let directory = tempdir().expect("temporary directory");
    fs::create_dir_all(directory.path().join(".medusa")).expect("create project config");
    fs::write(
        directory.path().join(".medusa/runtime.toml"),
        "unknown_authority = true\n",
    )
    .expect("write invalid runtime config");

    let result = RuntimeState::load(directory.path().to_path_buf());
    let error = match result {
        Ok(_) => panic!("invalid runtime config must fail closed"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("failed to parse runtime configuration")
    );
}

#[test]
fn unadmitted_repository_runtime_route_fails_before_provider_startup() {
    let directory = tempdir().expect("temporary directory");
    fs::create_dir_all(directory.path().join(".medusa")).expect("create project config");
    fs::write(
        directory.path().join(".medusa/runtime.toml"),
        "schema_version = 1\nprovider = \"unadmitted-provider\"\nmodel = \"unadmitted-model\"\n",
    )
    .expect("write runtime route");

    let result = RuntimeState::load(directory.path().to_path_buf());
    let error = match result {
        Ok(_) => panic!("an unadmitted runtime route must fail closed"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("not an admitted provider/model route")
    );
}

#[test]
fn admitted_repository_runtime_route_applies_the_selected_fallback() {
    let directory = tempdir().expect("temporary directory");
    fs::create_dir_all(directory.path().join(".medusa")).expect("create project config");
    fs::write(
        directory.path().join(".medusa/runtime.toml"),
        "schema_version = 1\nprovider = \"fallback\"\nmodel = \"fallback-model\"\n",
    )
    .expect("write runtime route");

    let mut config = Config::default();
    config
        .model
        .fallback_providers
        .push(medusa_config::FallbackProviderConfig {
            provider: "fallback".to_owned(),
            name: "fallback-model".to_owned(),
            protocol: "openai".to_owned(),
            base_url: Some("https://fallback.invalid".to_owned()),
            auth: "none".to_owned(),
            tool_calling: true,
            streaming: true,
            max_retries: 2,
            retry_base_delay_ms: 10,
            retry_max_delay_ms: 100,
            retry_jitter_ms: 0,
        });

    let effective = runtime_config_effective_for_repo(directory.path(), &config)
        .expect("admitted route compiles");
    apply_runtime_route(&mut config, &effective).expect("admitted route applies");
    assert_eq!(config.model.provider, "fallback");
    assert_eq!(config.model.name, "fallback-model");
    assert_eq!(
        config.model.base_url.as_deref(),
        Some("https://fallback.invalid")
    );
    assert_eq!(config.model.auth, "none");
    assert!(config.model.streaming);
    assert_eq!(config.model.max_retries, 2);
}

#[test]
fn explicit_config_startup_rejects_invalid_runtime_policy() {
    let directory = tempdir().expect("temporary directory");
    fs::create_dir_all(directory.path().join(".medusa")).expect("create project config");
    fs::write(
        directory.path().join(".medusa/runtime.toml"),
        "schema_version = 1\nservice_provider = \"unregistered-service\"\n",
    )
    .expect("write runtime policy");

    let controller =
        RuntimeController::start_with_config(directory.path().to_path_buf(), Config::default());
    let event = controller
        .events
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("startup event");
    assert!(matches!(
        event,
        RuntimeEvent::Failed(message)
            if message.contains("no certified non-authority service provider is registered")
    ));
}

#[test]
fn text_prompt_becomes_user_message_block() {
    let draft = PromptDraft {
        text: "fix the failing test".to_owned(),
        ..PromptDraft::default()
    };
    assert_eq!(
        message_blocks(&draft).expect("message blocks"),
        vec![MessageBlock::Text {
            text: "fix the failing test".to_owned(),
        }]
    );
}

#[test]
fn screenshot_is_encoded_as_png_image_block() {
    let draft = PromptDraft {
        attachments: vec![PromptAttachment::Image(ImageAttachment {
            display_name: "screen.png".to_owned(),
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255],
            source_format: Some("image/rgba8".to_owned()),
        })],
        ..PromptDraft::default()
    };
    let blocks = message_blocks(&draft).expect("message blocks");
    assert!(matches!(
        &blocks[0],
        MessageBlock::Image {
            source: ImageSource::Base64 { media_type, data },
            ..
        } if media_type == "image/png" && !data.is_empty()
    ));
}

#[test]
fn attached_utf8_file_is_bounded_and_included() {
    let directory = tempdir().expect("temporary directory");
    let path = directory.path().join("error.txt");
    fs::write(&path, "compiler error").expect("write fixture");
    let draft = PromptDraft {
        attachments: vec![PromptAttachment::File(FileAttachment {
            path,
            byte_len: 14,
        })],
        ..PromptDraft::default()
    };
    let blocks = message_blocks(&draft).expect("message blocks");
    assert!(matches!(
        &blocks[0],
        MessageBlock::Text { text } if text.contains("compiler error")
    ));
}

#[test]
fn provider_usage_forwards_legacy_and_normalized_telemetry() {
    let (sender, receiver) = mpsc::channel();
    let mut state = UpdateState::new();
    forward_update(
        &AgentUpdate::Event(EventPayload::ModelRequestStarted {
            provider: "minimax".to_owned(),
            model: "MiniMax-M3".to_owned(),
            request_id: None,
            request_fingerprint: None,
            manifest_ref: None,
            attempt_ordinal: 0,
            parent_request_id: None,
        }),
        &sender,
        &mut state,
    );
    forward_update(
        &AgentUpdate::Event(EventPayload::ModelResponseReceived {
            response_id: Some("legacy-response".to_owned()),
            request_id: None,
            request_fingerprint: None,
            usage: json!({
                "input_tokens": 120,
                "output_tokens": 30,
                "cache_read_input_tokens": 80,
                "cache_creation_input_tokens": 20
            }),
        }),
        &sender,
        &mut state,
    );
    assert!(matches!(
        receiver.recv().expect("legacy usage event"),
        RuntimeEvent::Usage {
            input_tokens: 120,
            output_tokens: 30,
            cache_read_input_tokens: 80,
            cache_creation_input_tokens: 20,
            total_tokens: 250,
            duration_ms,
            provenance: UsageProvenance::ProviderReported,
            ..
        } if duration_ms >= 1
    ));
    assert_eq!(state.current_context_tokens, 220);

    forward_update(
        &AgentUpdate::Event(EventPayload::ModelResponseReceived {
            response_id: Some("normalized-response".to_owned()),
            request_id: None,
            request_fingerprint: None,
            usage: json!({
                "turn": 2,
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_read_input_tokens": 2,
                "cache_creation_input_tokens": 1,
                "total_tokens": 18,
                "duration_ms": 100,
                "tokens_per_second_milli": 180_000,
                "estimated_cost_microusd": 7,
                "provenance": "provider_reported"
            }),
        }),
        &sender,
        &mut state,
    );
    assert!(matches!(
        receiver.recv().expect("normalized usage event"),
        RuntimeEvent::Usage {
            total_tokens: 18,
            duration_ms: 100,
            tokens_per_second_milli: 180_000,
            estimated_cost_microusd: 7,
            provenance: UsageProvenance::ProviderReported,
            ..
        }
    ));
}

#[test]
fn runtime_events_preserve_agent_plan_contracts() {
    let (sender, receiver) = mpsc::channel();
    let mut state = UpdateState::new();
    forward_update(
        &AgentUpdate::Plan(vec![AgentPlanStep {
            title: "Extract runtime".to_owned(),
            status: AgentPlanStepStatus::InProgress,
        }]),
        &sender,
        &mut state,
    );
    let RuntimeEvent::Plan(plan) = receiver.recv().expect("plan event") else {
        panic!("expected plan event");
    };
    assert_eq!(plan[0].title, "Extract runtime");
    assert_eq!(plan[0].status, AgentPlanStepStatus::InProgress);
}

#[test]
fn tool_call_is_shown_as_one_high_level_row() {
    let (sender, receiver) = mpsc::channel();
    let mut state = UpdateState::new();
    forward_update(
        &AgentUpdate::Event(EventPayload::ToolCallRequested {
            tool: "fs_read".to_owned(),
            arguments: json!({"path": "src/lib.rs"}),
        }),
        &sender,
        &mut state,
    );

    let started = match receiver.recv().expect("tool start") {
        RuntimeEvent::Activity(activity) => activity,
        other => panic!("expected tool activity, received {other:?}"),
    };

    forward_update(
        &AgentUpdate::ToolOutput {
            tool: "fs_read".to_owned(),
            output: "line one\nline two".to_owned(),
            is_error: false,
        },
        &sender,
        &mut state,
    );

    let completed = match receiver.recv().expect("tool result") {
        RuntimeEvent::Activity(activity) => activity,
        other => panic!("expected tool activity, received {other:?}"),
    };
    assert_eq!(started.id, completed.id);
    assert_eq!(completed.title, "Read(src/lib.rs)");
    assert!(started.details.is_empty());
    assert_eq!(completed.details, vec!["line one", "line two"]);
}

#[test]
fn portable_tool_titles_distinguish_shell_and_directory_operations() {
    assert_eq!(
        tool_title("shell_run", &json!({"program": "cargo", "args": ["test"]})),
        "Shell(cargo test)"
    );
    assert_eq!(
        tool_title("fs_create_dir", &json!({"path": "landing-page/assets"})),
        "Mkdir(landing-page/assets)"
    );
}

#[test]
fn controller_exposes_shared_busy_and_cancel_semantics() {
    let directory = tempdir().expect("temporary directory");
    let runtime = RuntimeController::start(directory.path().to_path_buf());
    assert!(!runtime.is_busy());
    assert!(!runtime.cancel());
}

#[test]
fn initial_submit_reports_pre_session_failure_instead_of_channel_loss() {
    let directory = tempdir().expect("temporary directory");
    let submission = std::sync::Arc::new(std::sync::Mutex::new(SubmissionState::default()));
    let (command_tx, command_rx) = mpsc::channel();
    let (_frontend_tx, frontend_rx) = mpsc::channel();
    let (event_sender, _runtime_event_rx) = mpsc::channel();
    let runtime = RuntimeController {
        commands: command_tx,
        events: frontend_rx,
        cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        submission: std::sync::Arc::clone(&submission),
        event_sender,
        team_control: TeamControlPlane::default(),
        repo: directory.path().to_path_buf(),
        invariants: std::sync::Arc::new(std::sync::Mutex::new(RuntimeInvariantRegistry::default())),
    };
    let worker = thread::spawn(move || {
        let RuntimeCommand::Submit { accepted, .. } =
            command_rx.recv().expect("submission command")
        else {
            panic!("expected submission command");
        };
        accepted
            .send(Err(
                "capability discovery failed: persistence failed".to_owned()
            ))
            .expect("reject submission");
    });

    let error = runtime
        .submit(PromptDraft {
            text: "start a durable session".to_owned(),
            ..PromptDraft::default()
        })
        .expect_err("pre-session failure should be returned");
    assert!(
        error
            .to_string()
            .contains("capability discovery failed: persistence failed")
    );
    assert!(!error.to_string().contains("prompt ended"));
    worker.join().expect("worker joins");
    assert!(!submission.lock().expect("submission state").busy);
}

#[test]
fn runtime_invariant_failure_blocks_submission_before_queueing() {
    let directory = tempdir().expect("temporary directory");
    let submission = std::sync::Arc::new(std::sync::Mutex::new(SubmissionState::default()));
    let (command_tx, command_rx) = mpsc::channel();
    let (_frontend_tx, frontend_rx) = mpsc::channel();
    let (event_sender, _runtime_event_rx) = mpsc::channel();
    let runtime = RuntimeController {
        commands: command_tx,
        events: frontend_rx,
        cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        submission,
        event_sender,
        team_control: TeamControlPlane::default(),
        repo: directory.path().to_path_buf(),
        invariants: std::sync::Arc::new(std::sync::Mutex::new(RuntimeInvariantRegistry::default())),
    };
    runtime
        .register_runtime_invariant("durability", |_context| {
            Err("durability preflight is unavailable".to_owned())
        })
        .expect("register invariant");

    let error = runtime
        .submit(PromptDraft {
            text: "should not queue".to_owned(),
            ..PromptDraft::default()
        })
        .expect_err("invariant should block submission");
    assert!(
        error
            .to_string()
            .contains("durability preflight is unavailable")
    );
    assert!(command_rx.try_recv().is_err());
}

#[test]
fn model_configuration_redacts_session_api_keys() {
    let directory = tempdir().expect("temporary directory");
    let mut state = RuntimeState::load(directory.path().to_path_buf()).expect("runtime state");
    state.session_api_key = Some("secret-value".to_owned());
    let details = model_configuration_details(&state).join("\n");
    assert!(details.contains("credential: configured"));
    assert!(!details.contains("secret-value"));
}

#[test]
fn model_picker_configuration_updates_provider_model_effort_and_session_key() {
    let directory = tempdir().expect("temporary directory");
    let mut state = RuntimeState::load(directory.path().to_path_buf()).expect("runtime state");
    state.session_api_key = Some("previous-session-secret".to_owned());
    let (sender, receiver) = mpsc::channel();

    configure_model(
        &mut state,
        ModelConfiguration {
            provider: "anthropic".to_owned(),
            model: "claude-sonnet-4-6".to_owned(),
            effort: Effort::Low,
            api_key: Some("session-secret".to_owned()),
            base_url: Some("https://gateway.example/v1".to_owned()),
        },
        &sender,
    )
    .expect("configure model");

    assert_eq!(state.config.model.provider, "anthropic");
    assert_eq!(state.config.model.name, "claude-sonnet-4-6");
    assert_eq!(
        state.config.model.base_url.as_deref(),
        Some("https://gateway.example/v1")
    );
    assert_eq!(state.config.agent.max_turns, 64);
    assert_eq!(state.session_api_key.as_deref(), Some("session-secret"));
    assert!(matches!(
        receiver.recv().expect("settings update"),
        RuntimeEvent::Settings {
            model,
            effort,
            credential_configured: true,
            ..
        } if model == "anthropic / claude-sonnet-4-6" && effort == "effort:low"
    ));
    let notice = receiver.recv().expect("configuration notice");
    assert!(!format!("{notice:?}").contains("session-secret"));
}

#[test]
fn model_picker_switches_route_authentication_mode() {
    let directory = tempdir().expect("temporary directory");
    let mut state = RuntimeState::load(directory.path().to_path_buf()).expect("runtime state");
    let (sender, _receiver) = mpsc::channel();

    configure_model(
        &mut state,
        ModelConfiguration {
            provider: "openai-oauth".to_owned(),
            model: "gpt-5.6-luna".to_owned(),
            effort: Effort::Medium,
            api_key: None,
            base_url: None,
        },
        &sender,
    )
    .expect("switch to OAuth route");

    assert_eq!(state.config.model.auth, "none");

    configure_model(
        &mut state,
        ModelConfiguration {
            provider: "minimax".to_owned(),
            model: "MiniMax-M2.7".to_owned(),
            effort: Effort::Medium,
            api_key: None,
            base_url: None,
        },
        &sender,
    )
    .expect("switch back to direct route");

    assert_eq!(state.config.model.auth, "api-key");
}

#[test]
fn oauth_route_reports_ready_without_a_medusa_api_key() {
    let directory = tempdir().expect("temporary directory");
    let mut state = RuntimeState::load(directory.path().to_path_buf()).expect("runtime state");
    let (sender, receiver) = mpsc::channel();

    configure_model(
        &mut state,
        ModelConfiguration {
            provider: "openai-oauth".to_owned(),
            model: "gpt-5.6-luna".to_owned(),
            effort: Effort::High,
            api_key: None,
            base_url: None,
        },
        &sender,
    )
    .expect("configure OAuth route");

    assert!(matches!(
        receiver.recv().expect("settings update"),
        RuntimeEvent::Settings {
            model,
            credential_configured: true,
            ..
        } if model == "openai-oauth / gpt-5.6-luna"
    ));
}

#[test]
fn api_key_route_without_a_credential_remains_unready() {
    let directory = tempdir().expect("temporary directory");
    let mut state = RuntimeState::load(directory.path().to_path_buf()).expect("runtime state");
    state.config.model.provider = "test-provider".to_owned();
    state.config.model.auth = "api-key".to_owned();

    assert!(matches!(
        state.settings_event(),
        RuntimeEvent::Settings {
            credential_configured: false,
            ..
        }
    ));
}

#[test]
fn effort_command_updates_the_runtime_turn_budget() {
    let directory = tempdir().expect("temporary directory");
    let mut state = RuntimeState::load(directory.path().to_path_buf()).expect("runtime state");
    let (sender, receiver) = mpsc::channel();
    execute_slash_command(
        &mut state,
        SlashCommand::Effort {
            effort: Some(Effort::Medium),
        },
        &sender,
        &Arc::new(AtomicBool::new(false)),
    )
    .expect("set effort");
    assert_eq!(state.config.agent.max_turns, 200);
    assert!(matches!(
        receiver.recv().expect("settings update"),
        RuntimeEvent::Settings { effort, .. } if effort == "effort:medium"
    ));
}

#[test]
fn goal_command_is_durable_and_guides_the_next_agent_turn() {
    let directory = tempdir().expect("temporary directory");
    let mut state = RuntimeState::load(directory.path().to_path_buf()).expect("runtime state");
    let (sender, receiver) = mpsc::channel();

    execute_slash_command(
        &mut state,
        SlashCommand::Goal {
            objective: Some("Build a responsive portfolio".to_owned()),
        },
        &sender,
        &Arc::new(AtomicBool::new(false)),
    )
    .expect("set goal");

    assert_eq!(
        state.pending_goal.as_deref(),
        Some("Build a responsive portfolio")
    );
    assert!(matches!(
        receiver.recv().expect("goal notice"),
        RuntimeEvent::Notice { title, details }
            if title == "Goal updated"
                && details.iter().any(|detail| detail.contains("next agent turn"))
    ));
}

#[test]
fn direct_skill_command_stages_validated_context_for_the_next_prompt() {
    let directory = tempdir().expect("temporary directory");
    let skill = directory.path().join(".medusa/skills/release/SKILL.md");
    fs::create_dir_all(skill.parent().expect("skill directory")).expect("create skills");
    fs::write(
        &skill,
        "---\nname: release\ndescription: Prepare a release\n---\nUse release steps.",
    )
    .expect("write skill");
    let mut state = RuntimeState::load(directory.path().to_path_buf()).expect("runtime state");
    let (sender, receiver) = mpsc::channel();

    execute_slash_command(
        &mut state,
        SlashCommand::Skill {
            selector: "release".to_owned(),
            task: None,
        },
        &sender,
        &Arc::new(AtomicBool::new(false)),
    )
    .expect("load skill");

    let selected = state.pending_skill.as_ref().expect("selected skill");
    assert_eq!(selected.name, "release");
    assert!(selected.prompt_context().contains("Use release steps."));
    assert!(matches!(
        receiver.recv().expect("skill notice"),
        RuntimeEvent::Notice { title, details }
            if title == "Skill loaded"
                && details.iter().any(|detail| detail.contains("next prompt"))
    ));
}

#[test]
fn duplicate_skill_names_require_an_explicit_scope_or_cleanup() {
    let directory = tempdir().expect("temporary directory");
    for root in [".medusa/skills/release", ".claude/skills/release"] {
        let skill = directory.path().join(root).join("SKILL.md");
        fs::create_dir_all(skill.parent().expect("skill directory")).expect("create skills");
        fs::write(skill, "---\ndescription: Release\n---\nBody").expect("write skill");
    }
    let error = load_selected_skill(directory.path(), "release")
        .expect_err("duplicate project skills must be rejected");
    assert!(error.to_string().contains("ambiguous"));
}

#[test]
fn skills_command_discovers_project_skill_metadata() {
    let directory = tempdir().expect("temporary directory");
    let skill = directory.path().join(".claude/skills/release/SKILL.md");
    fs::create_dir_all(skill.parent().expect("skill directory")).expect("create skills");
    fs::write(
        &skill,
        "---\nname: release\ndescription: Prepare a release\n---\nBody",
    )
    .expect("write skill");
    assert!(
        discover_skills(directory.path())
            .iter()
            .any(|skill| skill == "release (project) - Prepare a release")
    );
}

#[test]
fn internal_plan_transport_is_hidden_and_assistant_text_is_forwarded_verbatim() {
    let (sender, receiver) = mpsc::channel();
    let mut state = UpdateState::new();
    forward_update(
        &AgentUpdate::Event(EventPayload::ToolCallRequested {
            tool: "update_plan".to_owned(),
            arguments: json!({"steps": [{"title": "Inspect", "status": "active"}]}),
        }),
        &sender,
        &mut state,
    );
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    forward_update(
        &AgentUpdate::AssistantText(
            "Now I have a clear picture. Key findings:\n\n1. First detail\n2. Second detail"
                .to_owned(),
        ),
        &sender,
        &mut state,
    );
    assert!(matches!(
        receiver.recv().expect("assistant text"),
        RuntimeEvent::AssistantText(text)
            if text == "Now I have a clear picture. Key findings:\n\n1. First detail\n2. Second detail"
    ));
}

#[test]
fn busy_submission_is_queued_as_a_follow_up_without_rejection() {
    let submission = Arc::new(Mutex::new(SubmissionState {
        busy: true,
        ..SubmissionState::default()
    }));
    {
        let mut state = submission.lock().expect("submission state");
        state.followups.push_back(QueuedFollowup {
            command_id: "followup-1".to_owned(),
            draft: PromptDraft {
                text: "also update the documentation".to_owned(),
                ..PromptDraft::default()
            },
            durably_recorded: true,
        });
    }
    let queued = take_followups(&submission);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].draft.text, "also update the documentation");
    assert!(submission.lock().expect("submission state").busy);
}

#[test]
fn runtime_atomically_reopens_input_only_when_followups_are_empty() {
    let submission = Arc::new(Mutex::new(SubmissionState {
        busy: true,
        ..SubmissionState::default()
    }));
    assert!(finish_or_take_followups(&submission).is_empty());
    assert!(!submission.lock().expect("submission state").busy);
}

#[test]
fn configuration_changes_queue_while_the_current_turn_finishes() {
    let directory = tempdir().expect("temporary directory");
    let submission = Arc::new(Mutex::new(SubmissionState {
        busy: true,
        active_session_id: Some("session-1".to_owned()),
        ..SubmissionState::default()
    }));
    let (command_tx, command_rx) = mpsc::channel();
    let (_frontend_tx, frontend_rx) = mpsc::channel();
    let (event_sender, _runtime_event_rx) = mpsc::channel();
    let runtime = RuntimeController {
        commands: command_tx,
        events: frontend_rx,
        cancel: Arc::new(AtomicBool::new(false)),
        submission,
        event_sender,
        team_control: TeamControlPlane::default(),
        repo: directory.path().to_path_buf(),
        invariants: Arc::new(Mutex::new(RuntimeInvariantRegistry::default())),
    };

    runtime
        .configure_model(ModelConfiguration {
            provider: "minimax".to_owned(),
            model: "MiniMax-M2.7".to_owned(),
            effort: Effort::High,
            api_key: None,
            base_url: None,
        })
        .expect("model changes should queue behind the active turn");
    assert!(matches!(
        command_rx.recv().expect("model configuration command"),
        RuntimeCommand::ConfigureModel(ModelConfiguration {
            effort: Effort::High,
            ..
        })
    ));

    runtime
        .run_command(SlashCommand::Effort {
            effort: Some(Effort::Low),
        })
        .expect("effort changes should queue behind the active turn");
    assert!(matches!(
        command_rx.recv().expect("effort command"),
        RuntimeCommand::Slash(SlashCommand::Effort {
            effort: Some(Effort::Low)
        })
    ));

    assert!(matches!(
        runtime.run_command(SlashCommand::Plan {
            task: Some("start another agent task".to_owned()),
        }),
        Err(RuntimeError::Busy)
    ));
}

fn durable_runtime_session(repo: &Path) -> medusa_agent::AgentSession {
    medusa_agent::AgentSession {
        id: SessionId::new(),
        objective: "runtime event coverage".to_owned(),
        repo: repo.to_path_buf(),
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
        completed: false,
        turn: 0,
        plan: Vec::new(),
        pending_question: None,
        messages: vec![Message {
            role: Role::User,
            content: vec![MessageBlock::Text {
                text: "runtime event coverage".to_owned(),
            }],
        }],
        events: Vec::new(),
        evidence: Vec::new(),
        tool_artifacts: Vec::new(),
        world_model: None,
        approval_grants: Vec::new(),
        approval_receipts: Vec::new(),
        rollback_receipts: Vec::new(),
        codex_thread_id: None,
    }
}

fn push_runtime_event(session: &mut medusa_agent::AgentSession, payload: EventPayload) {
    let event = EventEnvelope::new(
        u64::try_from(session.events.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1),
        session.id.clone(),
        Actor::Coordinator,
        CorrelationId::new(),
        payload,
        session.events.last().map(|event| event.checksum.clone()),
        OffsetDateTime::UNIX_EPOCH,
    )
    .expect("event");
    session.events.push(event);
}

#[test]
fn queued_followups_are_rebuilt_from_canonical_events() {
    let directory = tempdir().expect("temporary directory");
    let mut session = durable_runtime_session(directory.path());
    let draft = PromptDraft {
        text: "also update the documentation".to_owned(),
        ..PromptDraft::default()
    };
    push_runtime_event(
        &mut session,
        EventPayload::UserFollowupQueued {
            command_id: "followup-1".to_owned(),
            prompt: serde_json::to_value(&draft).expect("serialize prompt"),
        },
    );

    let restored = restore_queued_followups(&session).expect("restore queued follow-up");
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].command_id, "followup-1");
    assert_eq!(restored[0].draft, draft);
    assert!(restored[0].durably_recorded);

    push_runtime_event(
        &mut session,
        EventPayload::UserFollowupDequeued {
            command_id: "followup-1".to_owned(),
            text: "also update the documentation".to_owned(),
        },
    );
    assert!(
        restore_queued_followups(&session)
            .expect("restore after dequeue")
            .is_empty()
    );
}

#[test]
fn terminal_controller_events_clear_recovered_followups() {
    let directory = tempdir().expect("temporary directory");
    let mut session = durable_runtime_session(directory.path());
    push_runtime_event(
        &mut session,
        EventPayload::UserFollowupQueued {
            command_id: "followup-1".to_owned(),
            prompt: serde_json::to_value(PromptDraft {
                text: "queued".to_owned(),
                ..PromptDraft::default()
            })
            .expect("serialize prompt"),
        },
    );
    push_runtime_event(
        &mut session,
        EventPayload::RuntimeFailed {
            message: "terminal failure".to_owned(),
        },
    );
    assert!(
        restore_queued_followups(&session)
            .expect("restore after failure")
            .is_empty()
    );
}

#[test]
fn controller_event_dispatch_commits_before_frontend_publication() {
    let directory = tempdir().expect("temporary directory");
    let mut session = durable_runtime_session(directory.path());
    let objective = session.objective.clone();
    medusa_agent::record_session_event(
        &mut session,
        Actor::Coordinator,
        EventPayload::SessionCreated { objective },
    )
    .expect("persist session");
    let session_id = session.id.to_string();
    let submission = Arc::new(Mutex::new(SubmissionState {
        active_session_id: Some(session_id.clone()),
        ..SubmissionState::default()
    }));
    let (runtime_tx, runtime_rx) = mpsc::channel();
    let (frontend_tx, frontend_rx) = mpsc::channel();
    let repo = directory.path().to_path_buf();
    let dispatch_submission = Arc::clone(&submission);
    let dispatcher = thread::spawn(move || {
        dispatch_runtime_events(&repo, &dispatch_submission, runtime_rx, &frontend_tx);
    });

    runtime_tx
        .send(RuntimeEvent::Team(TeamSnapshot::default()))
        .expect("send runtime event");
    assert!(matches!(
        frontend_rx.recv().expect("frontend event"),
        RuntimeEvent::Team(_)
    ));
    let persisted = medusa_agent::session_browser::load_session(directory.path(), &session_id)
        .expect("load committed session");
    assert!(matches!(
        persisted.events.last().map(|event| &event.payload),
        Some(EventPayload::TeamStateChanged { .. })
    ));

    drop(runtime_tx);
    dispatcher.join().expect("dispatcher joins");
}

#[test]
fn runtime_event_durability_classification_is_explicit() {
    assert!(matches!(
        RuntimeEvent::Started.durability(),
        RuntimeEventDurability::PresentationOnly(_)
    ));
    assert!(matches!(
        RuntimeEvent::AssistantText("answer".to_owned()).durability(),
        RuntimeEventDurability::CanonicalJournal("assistant_message_recorded")
    ));
    assert!(matches!(
        RuntimeEvent::TurnFinished.durability(),
        RuntimeEventDurability::CanonicalJournal("runtime_turn_finished")
    ));
    assert!(matches!(
        RuntimeEvent::Cancelled.durability(),
        RuntimeEventDurability::SessionBoundCanonical { .. }
    ));
    assert!(matches!(
        RuntimeEvent::Failed("startup".to_owned()).durability(),
        RuntimeEventDurability::SessionBoundCanonical { .. }
    ));
}

#[test]
fn initial_submit_waits_for_session_acceptance_before_returning() {
    let directory = tempdir().expect("temporary directory");
    let submission = std::sync::Arc::new(std::sync::Mutex::new(SubmissionState::default()));
    let (command_tx, command_rx) = mpsc::channel();
    let (_frontend_tx, frontend_rx) = mpsc::channel();
    let (event_sender, _runtime_event_rx) = mpsc::channel();
    let runtime = RuntimeController {
        commands: command_tx,
        events: frontend_rx,
        cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        submission: std::sync::Arc::clone(&submission),
        event_sender,
        team_control: TeamControlPlane::default(),
        repo: directory.path().to_path_buf(),
        invariants: std::sync::Arc::new(std::sync::Mutex::new(RuntimeInvariantRegistry::default())),
    };
    let worker_submission = std::sync::Arc::clone(&submission);
    let worker = thread::spawn(move || {
        let RuntimeCommand::Submit { draft, accepted } =
            command_rx.recv().expect("submission command")
        else {
            panic!("expected submission command");
        };
        assert_eq!(draft.text, "start a durable session");
        let mut state = worker_submission.lock().expect("submission state");
        assert!(state.busy);
        state.active_session_id = Some("session-accepted".to_owned());
        drop(state);
        accepted.send(Ok(())).expect("accept submission");
    });

    assert_eq!(
        runtime
            .submit(PromptDraft {
                text: "start a durable session".to_owned(),
                ..PromptDraft::default()
            })
            .expect("accepted submission"),
        SubmitDisposition::Started
    );
    worker.join().expect("worker joins");
    assert_eq!(
        submission
            .lock()
            .expect("submission state")
            .active_session_id
            .as_deref(),
        Some("session-accepted")
    );
}

#[test]
fn followup_queues_until_a_durable_session_identity_exists() {
    let directory = tempdir().expect("temporary directory");
    let submission = std::sync::Arc::new(std::sync::Mutex::new(SubmissionState {
        busy: true,
        active_session_id: None,
        ..SubmissionState::default()
    }));
    let (command_tx, command_rx) = mpsc::channel();
    let (_frontend_tx, frontend_rx) = mpsc::channel();
    let (event_sender, _runtime_event_rx) = mpsc::channel();
    let runtime = RuntimeController {
        commands: command_tx,
        events: frontend_rx,
        cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        submission: std::sync::Arc::clone(&submission),
        event_sender,
        team_control: TeamControlPlane::default(),
        repo: directory.path().to_path_buf(),
        invariants: std::sync::Arc::new(std::sync::Mutex::new(RuntimeInvariantRegistry::default())),
    };

    assert_eq!(
        runtime
            .submit(PromptDraft {
                text: "queue this until durability".to_owned(),
                ..PromptDraft::default()
            })
            .expect("follow-up queues"),
        SubmitDisposition::Queued
    );
    assert!(command_rx.try_recv().is_err());
    assert!(
        submission
            .lock()
            .expect("submission state")
            .followups
            .is_empty()
    );
    assert_eq!(
        submission
            .lock()
            .expect("submission state")
            .pre_session_followups
            .len(),
        1
    );
}
