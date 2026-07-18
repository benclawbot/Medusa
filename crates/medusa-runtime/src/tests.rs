use std::sync::{atomic::AtomicBool, mpsc};

use medusa_agent::{AgentPlanStep, AgentPlanStepStatus, AgentUpdate};
use medusa_protocol::EventPayload;
use medusa_provider::{ImageSource, MessageBlock};
use serde_json::json;
use tempfile::tempdir;

use crate::prompt::{ImageAttachment, PromptAttachment};

use super::support::{UpdateState, forward_update, message_blocks};
use super::*;

#[test]
fn text_and_image_prompts_are_frontend_neutral_message_blocks() {
    let text = PromptDraft {
        text: "fix the failing test".to_owned(),
        ..PromptDraft::default()
    };
    assert_eq!(
        message_blocks(&text).expect("text blocks"),
        vec![MessageBlock::Text {
            text: "fix the failing test".to_owned(),
        }]
    );

    let image = PromptDraft {
        attachments: vec![PromptAttachment::Image(ImageAttachment {
            display_name: "screen.png".to_owned(),
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255],
            source_format: Some("image/rgba8".to_owned()),
        })],
        ..PromptDraft::default()
    };
    assert!(matches!(
          &message_blocks(&image).expect("image blocks")[0],
          MessageBlock::Image {
    source: ImageSource::Base64 { media_type, data },
    ..
          } if media_type == "image/png" && !data.is_empty()
      ));
}

#[test]
fn runtime_events_preserve_usage_and_agent_plan_contracts() {
    let (sender, receiver) = mpsc::channel();
    let mut state = UpdateState::new();
    forward_update(
        &AgentUpdate::Event(EventPayload::ModelResponseReceived {
            response_id: Some("response-1".to_owned()),
            usage: json!({"input_tokens": 12, "output_tokens": 3}),
        }),
        &sender,
        &mut state,
    );
    assert!(matches!(
        receiver.recv().expect("usage event"),
        RuntimeEvent::Usage {
            input_tokens: 12,
            output_tokens: 3,
            ..
        }
    ));

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
fn controller_exposes_shared_busy_and_cancel_semantics() {
    let directory = tempdir().expect("temporary directory");
    let runtime = RuntimeController::start(directory.path().to_path_buf());
    assert!(!runtime.is_busy());
    assert!(!runtime.cancel());
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
