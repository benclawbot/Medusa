use std::{
    collections::{BTreeSet, VecDeque},
    env, fs,
    path::{Path, PathBuf},
    sync::mpsc::Sender,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use medusa_agent::{AgentPlanStep, AgentPlanStepStatus, AgentQuestion, AgentUpdate};
use medusa_protocol::EventPayload;
use medusa_provider::{ImageSource, MessageBlock};
use serde_json::Value;

use crate::{
    app::{
        QuestionOption, QuestionPrompt, TranscriptPlan, TranscriptPlanStep, TranscriptPlanStepState,
    },
    clipboard::{ImageAttachment, PromptAttachment, PromptDraft},
    commands::{Effort, ModelConfiguration},
};

use super::{
    RuntimeActivity, RuntimeActivityKind, RuntimeError, RuntimeEvent, RuntimeQuestion, RuntimeState,
};

const MAX_FILE_CONTEXT_BYTES: usize = 2 * 1024 * 1024;

pub(super) fn configure_model(
    state: &mut RuntimeState,
    configuration: ModelConfiguration,
    events: &Sender<RuntimeEvent>,
) -> Result<(), RuntimeError> {
    if !is_supported_provider(&configuration.provider) {
        return Err(RuntimeError::InvalidCommand(
            "supported providers are minimax, anthropic, and anthropic-compatible".to_owned(),
        ));
    }
    state.config.model.provider = configuration.provider;
    state.config.model.name = configuration.model;
    state.effort = configuration.effort;
    state.config.agent.max_turns = match configuration.effort {
        Effort::Auto => state.base_config.agent.max_turns,
        effort => turns_for_effort(effort),
    };
    if let Some(api_key) = configuration.api_key {
        state.session_api_key = Some(api_key);
    }
    let _ = events.send(state.settings_event());
    let _ = events.send(RuntimeEvent::Notice {
        title: "Model configuration updated".to_owned(),
        details: model_configuration_details(state),
    });
    Ok(())
}

pub(super) fn effort_for_turns(max_turns: u32) -> Effort {
    match max_turns {
        0..=99 => Effort::Low,
        100..=299 => Effort::Medium,
        _ => Effort::High,
    }
}

pub(super) fn turns_for_effort(effort: Effort) -> u32 {
    match effort {
        Effort::Low => 64,
        Effort::Medium => 200,
        Effort::High => 500,
        Effort::Auto => unreachable!("auto resolves to the configured default"),
    }
}

pub(super) fn is_supported_provider(provider: &str) -> bool {
    matches!(provider, "minimax" | "anthropic" | "anthropic-compatible")
}

pub(super) fn model_configuration_details(state: &RuntimeState) -> Vec<String> {
    let credential = if state.session_api_key.is_some()
        || credential_environment(&state.config.model.provider)
            .is_some_and(|name| env::var(name).is_ok())
    {
        "credential: configured"
    } else {
        "credential: missing"
    };
    vec![
        format!("provider: {}", state.config.model.provider),
        format!("model: {}", state.config.model.name),
        credential.to_owned(),
        "set provider: /model provider <minimax|anthropic|anthropic-compatible>".to_owned(),
        "set model: /model <model-name>".to_owned(),
        "set session key: /model key <api-key>".to_owned(),
    ]
}

pub(super) fn credential_environment(provider: &str) -> Option<&'static str> {
    match provider {
        "minimax" => Some("MINIMAX_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "anthropic-compatible" => Some("MEDUSA_API_KEY"),
        _ => None,
    }
}

pub(super) fn discover_skills(repo: &Path) -> Vec<String> {
    let mut roots = vec![
        ("project", repo.join(".medusa/skills")),
        ("project", repo.join(".claude/skills")),
    ];
    if let Some(home) = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
        roots.push(("user", home.join(".medusa/skills")));
        roots.push(("user", home.join(".claude/skills")));
    }
    let mut skills = BTreeSet::new();
    for (scope, root) in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let skill = path.join("SKILL.md");
            if skill.is_file() {
                let description = skill_description(&skill);
                skills.insert(format!(
                    "{} ({scope}){}",
                    entry.file_name().to_string_lossy(),
                    description
                        .map(|description| format!(" - {description}"))
                        .unwrap_or_default()
                ));
            }
        }
    }
    skills.into_iter().collect()
}

fn skill_description(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().and_then(|text| {
        text.lines().find_map(|line| {
            line.strip_prefix("description:")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.trim_matches('"').to_owned())
        })
    })
}

pub(super) struct UpdateState {
    next_tool_id: u64,
    pending_tools: VecDeque<PendingTool>,
}

impl UpdateState {
    pub(super) fn new() -> Self {
        Self {
            next_tool_id: 0,
            pending_tools: VecDeque::new(),
        }
    }
}

struct PendingTool {
    id: String,
    tool: String,
    title: String,
}

pub(super) fn forward_update(
    update: &AgentUpdate,
    events: &Sender<RuntimeEvent>,
    state: &mut UpdateState,
) {
    match update {
        AgentUpdate::Event(EventPayload::ModelResponseReceived { usage, .. }) => {
            if let Some(output_tokens) = usage.get("output_tokens").and_then(Value::as_u64) {
                let _ = events.send(RuntimeEvent::Usage { output_tokens });
            }
        }
        AgentUpdate::Event(EventPayload::ToolCallRequested { tool, arguments }) => {
            if is_internal_tool(tool) {
                return;
            }
            state.next_tool_id = state.next_tool_id.saturating_add(1);
            let pending = PendingTool {
                id: format!("tool-{}", state.next_tool_id),
                tool: tool.clone(),
                title: tool_title(tool, arguments),
            };
            let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                id: Some(pending.id.clone()),
                kind: RuntimeActivityKind::Tool,
                title: pending.title.clone(),
                details: Vec::new(),
            }));
            state.pending_tools.push_back(pending);
        }
        AgentUpdate::Event(EventPayload::VerificationCompleted { passed, evidence }) => {
            let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                id: None,
                kind: RuntimeActivityKind::Verification,
                title: if *passed {
                    "Verify fixes".to_owned()
                } else {
                    "Verification failed".to_owned()
                },
                details: evidence.iter().map(|line| summarize(line)).collect(),
            }));
        }
        // Keep the assistant's milestone, but not the expanded narrative that follows it.
        // Tool arguments and results remain in the durable session for the model.
        AgentUpdate::AssistantText(text) => {
            if let Some(title) = assistant_title(text) {
                let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {
                    id: None,
                    kind: RuntimeActivityKind::Assistant,
                    title,
                    details: Vec::new(),
                }));
            }
        }
        AgentUpdate::Plan(steps) => {
            let _ = events.send(RuntimeEvent::Plan(transcript_plan(steps)));
        }
        AgentUpdate::Question(_) => {}
        AgentUpdate::ToolOutput {
            tool,
            output: _,
            is_error,
        } => {
            if is_internal_tool(tool) {
                return;
            }
            let pending = state
                .pending_tools
                .iter()
                .position(|pending| pending.tool == *tool)
                .and_then(|index| state.pending_tools.remove(index));
            let activity = pending.map_or_else(
                || RuntimeActivity {
                    id: None,
                    kind: if *is_error {
                        RuntimeActivityKind::Error
                    } else {
                        RuntimeActivityKind::Tool
                    },
                    title: if *is_error {
                        format!("{tool} failed")
                    } else {
                        tool.clone()
                    },
                    details: Vec::new(),
                },
                |pending| RuntimeActivity {
                    id: Some(pending.id),
                    kind: if *is_error {
                        RuntimeActivityKind::Error
                    } else {
                        RuntimeActivityKind::Tool
                    },
                    title: if *is_error {
                        format!("{} failed", pending.title)
                    } else {
                        pending.title
                    },
                    details: Vec::new(),
                },
            );
            let _ = events.send(RuntimeEvent::Activity(activity));
        }
        _ => {}
    }
}

pub(super) fn runtime_question(question: &AgentQuestion) -> RuntimeQuestion {
    RuntimeQuestion {
        questions: question
            .prompts()
            .iter()
            .map(|item| QuestionPrompt {
                header: item.header.clone(),
                question: item.question.clone(),
                options: item
                    .options
                    .iter()
                    .map(|option| QuestionOption {
                        label: option.label.clone(),
                        description: option.description.clone(),
                    })
                    .collect(),
                multi_select: item.multi_select,
            })
            .collect(),
    }
}

fn is_internal_tool(tool: &str) -> bool {
    matches!(tool, "update_plan" | "ask_user_question")
}

pub(super) fn transcript_plan(steps: &[AgentPlanStep]) -> TranscriptPlan {
    TranscriptPlan {
        steps: steps
            .iter()
            .map(|step| TranscriptPlanStep {
                title: step.title.clone(),
                state: match step.status {
                    AgentPlanStepStatus::Pending => TranscriptPlanStepState::Pending,
                    AgentPlanStepStatus::InProgress => TranscriptPlanStepState::Active,
                    AgentPlanStepStatus::Completed => TranscriptPlanStepState::Completed,
                    AgentPlanStepStatus::Failed => TranscriptPlanStepState::Failed,
                },
            })
            .collect(),
    }
}

pub(super) fn tool_title(tool: &str, arguments: &Value) -> String {
    match tool {
        "fs_read" => format!("Read({})", json_string(arguments, "path")),
        "fs_create_dir" => format!("Mkdir({})", json_string(arguments, "path")),
        "fs_write" => format!("Write({})", json_string(arguments, "path")),
        "search_text" => format!("Search({})", json_string(arguments, "query")),
        "code_index" => {
            let name = json_string(arguments, "name");
            if name.is_empty() {
                "Index repository".to_owned()
            } else {
                format!("Index({name})")
            }
        }
        "patch_apply" => "Edit files".to_owned(),
        "symbol_rename" => format!(
            "Rename({} -> {})",
            json_string(arguments, "old_name"),
            json_string(arguments, "new_name")
        ),
        "shell_run" => format!("Shell({})", shell_command(arguments)),
        "web_search" => format!("WebSearch({})", json_string(arguments, "query")),
        "web_fetch" => format!("WebFetch({})", json_string(arguments, "url")),
        "git_checkpoint" => format!("Checkpoint({})", json_string(arguments, "message")),
        _ => tool.to_owned(),
    }
}

fn json_string(arguments: &Value, key: &str) -> String {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn shell_command(arguments: &Value) -> String {
    let program = json_string(arguments, "program");
    let args = arguments
        .get("args")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    if args.is_empty() {
        program
    } else {
        format!("{program} {args}")
    }
}

fn summarize(value: &str) -> String {
    let compact = value.replace('\n', " ");
    if compact.chars().count() <= 140 {
        return compact;
    }
    compact.chars().take(137).chain("...".chars()).collect()
}

fn assistant_title(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let title = line
        .trim_start_matches(|character: char| {
            character.is_ascii_whitespace() || matches!(character, '-' | '*' | '#' | '>')
        })
        .trim();
    (!title.is_empty()).then(|| summarize(title))
}

pub(super) fn objective_for(draft: &PromptDraft) -> String {
    let trimmed = draft.text.trim();
    if trimmed.is_empty() {
        format!(
            "Use the {} attached item(s) as context and complete the coding task.",
            draft.attachments.len()
        )
    } else {
        trimmed.to_owned()
    }
}

pub(super) fn message_blocks(draft: &PromptDraft) -> Result<Vec<MessageBlock>, RuntimeError> {
    let mut blocks = Vec::new();
    if !draft.text.is_empty() {
        blocks.push(MessageBlock::Text {
            text: draft.text.clone(),
        });
    }
    for attachment in &draft.attachments {
        match attachment {
            PromptAttachment::PastedText(text) => blocks.push(MessageBlock::Text {
                text: format!(
                    "<pasted_text name=\"{}\">\n{}\n</pasted_text>",
                    text.display_name, text.text
                ),
            }),
            PromptAttachment::Image(image) => blocks.push(image_block(image)?),
            PromptAttachment::File(file) => {
                let bytes = fs::read(&file.path)?;
                if bytes.len() > MAX_FILE_CONTEXT_BYTES {
                    return Err(RuntimeError::FileTooLarge {
                        path: file.path.clone(),
                        bytes: bytes.len(),
                    });
                }
                let text = String::from_utf8(bytes).map_err(|_| RuntimeError::BinaryFile {
                    path: file.path.clone(),
                })?;
                blocks.push(MessageBlock::Text {
                    text: format!(
                        "<attached_file path=\"{}\">\n{}\n</attached_file>",
                        file.path.display(),
                        text
                    ),
                });
            }
        }
    }
    if blocks.is_empty() {
        return Err(RuntimeError::EmptyPrompt);
    }
    Ok(blocks)
}

fn image_block(image: &ImageAttachment) -> Result<MessageBlock, RuntimeError> {
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, image.width, image.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(RuntimeError::png)?;
        writer
            .write_image_data(&image.rgba)
            .map_err(RuntimeError::png)?;
    }
    Ok(MessageBlock::Image {
        source: ImageSource::Base64 {
            media_type: "image/png".to_owned(),
            data: STANDARD.encode(encoded),
        },
        alt_text: Some(image.display_name.clone()),
    })
}
