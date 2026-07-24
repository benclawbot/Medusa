use std::{fs, sync::mpsc};

use medusa_agent::{AgentPlanStep, AgentPlanStepStatus, AgentUpdate};
use medusa_protocol::EventPayload;
use medusa_provider::{ImageSource, MessageBlock};
use serde_json::json;
use tempfile::tempdir;

use crate::prompt::{FileAttachment, ImageAttachment, PromptAttachment};

use super::support::{
    UpdateState, discover_skills, forward_update, load_selected_skill, message_blocks,
    model_configuration_details, tool_title,
};
use super::*;

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
        }),
        &sender,
        &mut state,
    );
    forward_update(
        &AgentUpdate::Event(EventPayload::ModelResponseReceived {
            response_id: Some("legacy-response".to_owned()),
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
    assert!(completed.details.is_empty());
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
        },
        &sender,
    )
    .expect("configure model");

    assert_eq!(state.config.model.provider, "anthropic");
    assert_eq!(state.config.model.name, "claude-sonnet-4-6");
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
        &AtomicBool::new(false),
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
        &AtomicBool::new(false),
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
        &AtomicBool::new(false),
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
        followups: VecDeque::new(),
    }));
    {
        let mut state = submission.lock().expect("submission state");
        state.followups.push_back(PromptDraft {
            text: "also update the documentation".to_owned(),
            ..PromptDraft::default()
        });
    }
    let queued = take_followups(&submission);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].text, "also update the documentation");
    assert!(submission.lock().expect("submission state").busy);
}

#[test]
fn runtime_atomically_reopens_input_only_when_followups_are_empty() {
    let submission = Arc::new(Mutex::new(SubmissionState {
        busy: true,
        followups: VecDeque::new(),
    }));
    assert!(finish_or_take_followups(&submission).is_empty());
    assert!(!submission.lock().expect("submission state").busy);
}
