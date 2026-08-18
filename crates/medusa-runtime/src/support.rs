use std::{
    collections::{BTreeSet, VecDeque},
    env, fs,
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    time::Instant,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use medusa_agent::AgentUpdate;
use medusa_protocol::EventPayload;
use medusa_provider::{ImageSource, MessageBlock};
use serde_json::Value;

use crate::{
    commands::{Effort, ModelConfiguration},
    prompt::{
        ImageAttachment, MAX_IMAGE_BYTES, MAX_IMAGE_PIXELS, PromptAttachment, PromptDraft,
    },
};

use super::{
    RuntimeActivity, RuntimeActivityKind, RuntimeError, RuntimeEvent, RuntimeState, TurnUsage,
    UsageProvenance,
};

const MAX_FILE_CONTEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_SKILL_CONTEXT_BYTES: usize = 64_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SelectedSkill {
    pub(super) name: String,
    pub(super) scope: String,
    pub(super) content: String,
}

impl SelectedSkill {
    pub(super) fn label(&self) -> String {
        format!("{} ({})", self.name, self.scope)
    }

    pub(super) fn prompt_context(&self) -> String {
        format!(
            "The user explicitly selected the following skill for this turn. Follow it unless it conflicts with system rules or the user's task.\n\n--- selected skill: {} ({}) ---\n{}\n--- end selected skill ---",
            self.name, self.scope, self.content
        )
    }
}

pub(super) fn configure_model(
    state: &mut RuntimeState,
    configuration: ModelConfiguration,
    events: &Sender<RuntimeEvent>,
) -> Result<(), RuntimeError> {
    if !is_supported_provider(&configuration.provider) {
        return Err(RuntimeError::InvalidCommand(format!(
            "supported providers are {}",
            SUPPORTED_PROVIDERS.join(", ")
        )));
    }
    state.config.model.protocol = protocol_for_provider(&configuration.provider).to_owned();
    state.config.model.context_window_tokens = model_context_window_tokens(
        &configuration.provider,
        &configuration.model,
        state.base_config.model.context_window_tokens,
    );
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
        Effort::Auto => 200,
    }
}

pub(super) const SUPPORTED_PROVIDERS: [&str; 8] = medusa_config::PROVIDER_CATALOG_IDS;

pub(super) fn is_supported_provider(provider: &str) -> bool {
    SUPPORTED_PROVIDERS.contains(&provider)
}

pub(super) fn protocol_for_provider(provider: &str) -> &'static str {
    medusa_config::provider_runtime_protocol(provider).unwrap_or("openai")
}

pub(super) fn model_context_window_tokens(
    provider: &str,
    model: &str,
    configured_default: u64,
) -> u64 {
    medusa_config::model_registry::model_context_limit(provider, model).unwrap_or(configured_default)
}

pub(super) fn should_auto_compact(
    current_tokens: u64,
    context_window_tokens: u64,
    threshold_percent: u8,
) -> bool {
    context_window_tokens > 0
        && u128::from(current_tokens).saturating_mul(100)
            >= u128::from(context_window_tokens).saturating_mul(u128::from(threshold_percent))
}

pub(super) fn model_configuration_details(state: &RuntimeState) -> Vec<String> {
    let credential = if state.session_api_key.is_some()
        || medusa_config::credential_environment(&state.config.model.provider)
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
        format!(
            "set provider: /model provider <{}>",
            SUPPORTED_PROVIDERS.join("|")
        ),
        "set model: /model <model-name>".to_owned(),
        "set session key: /model key <api-key>".to_owned(),
    ]
}

pub(super) fn credential_environment(provider: &str) -> Option<&'static str> {
    medusa_config::credential_environment(provider)
}

pub(super) fn discover_skills(repo: &Path) -> Vec<String> {
    let mut skills = BTreeSet::new();
    for (scope, root) in skill_roots(repo) {
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

pub(super) fn load_selected_skill(
    repo: &Path,
    selector: &str,
) -> Result<SelectedSkill, RuntimeError> {
    let selector = selector.trim();
    let (name, requested_scope) = selector
        .rsplit_once('@')
        .map_or((selector, None), |(name, scope)| (name, Some(scope)));
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', '@'])
        || name.contains("..")
    {
        return Err(RuntimeError::InvalidCommand(
            "skill names must be single directory names".to_owned(),
        ));
    }
    if requested_scope.is_some_and(|scope| !matches!(scope, "project" | "user")) {
        return Err(RuntimeError::InvalidCommand(
            "skill scope must be project or user".to_owned(),
        ));
    }

    let mut matches = Vec::new();
    for (scope, root) in skill_roots(repo) {
        if requested_scope.is_some_and(|requested| requested != scope) {
            continue;
        }
        let skill = root.join(name).join("SKILL.md");
        if !skill.is_file() {
            continue;
        }
        let canonical_root = fs::canonicalize(&root)?;
        let canonical_skill = fs::canonicalize(&skill)?;
        if !canonical_skill.starts_with(&canonical_root) {
            return Err(RuntimeError::InvalidCommand(format!(
                "skill {name} escapes its configured skill root"
            )));
        }
        matches.push((scope, canonical_skill));
    }

    if matches.is_empty() {
        return Err(RuntimeError::InvalidCommand(format!(
            "skill {name} was not found; use /skills to list installed skills"
        )));
    }
    if matches.len() > 1 {
        let scopes = matches
            .iter()
            .map(|(scope, _)| *scope)
            .collect::<BTreeSet<_>>();
        let hint = if scopes.len() > 1 {
            format!("use /{name}@project or /{name}@user")
        } else {
            format!(
                "remove the duplicate {name} definitions in the {0} scope",
                matches[0].0
            )
        };
        return Err(RuntimeError::InvalidCommand(format!(
            "skill {name} is ambiguous; {hint}"
        )));
    }

    let Some((scope, path)) = matches.pop() else {
        return Err(RuntimeError::InvalidCommand(format!(
            "skill {name} disappeared while resolving its path"
        )));
    };
    let approved_root = repo.join(".medusa/skills");
    if scope == "project" && approved_root.is_dir() {
        let canonical_root = fs::canonicalize(&approved_root)?;
        if path.starts_with(&canonical_root) {
            let resolved = crate::skill_dependencies::resolve_project_skill(
                &approved_root,
                name,
                MAX_SKILL_CONTEXT_BYTES,
            )
            .map_err(RuntimeError::InvalidCommand)?;
            return Ok(SelectedSkill {
                name: name.to_owned(),
                scope: scope.to_owned(),
                content: resolved.content,
            });
        }
    }
    let bytes = fs::read(&path)?;
    if bytes.len() > MAX_SKILL_CONTEXT_BYTES {
        return Err(RuntimeError::FileTooLarge {
            path,
            bytes: bytes.len(),
        });
    }
    let content =
        String::from_utf8(bytes).map_err(|_| RuntimeError::BinaryFile { path: path.clone() })?;
    Ok(SelectedSkill {
        name: name.to_owned(),
        scope: scope.to_owned(),
        content,
    })
}

fn skill_roots(repo: &Path) -> Vec<(&'static str, PathBuf)> {
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
    roots
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
    model_started_at: Option<Instant>,
    pub(super) current_context_tokens: u64,
    suppress_model_plan: bool,
}

impl UpdateState {
    pub(super) fn new() -> Self {
        Self {
            next_tool_id: 0,
            pending_tools: VecDeque::new(),
            model_started_at: None,
            current_context_tokens: 0,
            suppress_model_plan: false,
        }
    }

    pub(super) fn suppress_model_plan(&mut self) {
        self.suppress_model_plan = true;
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
        AgentUpdate::Event(EventPayload::ModelRequestStarted { .. }) => {
            state.model_started_at = Some(Instant::now());
        }
        AgentUpdate::Event(EventPayload::ModelResponseReceived { usage, .. }) => {
            let measured_duration_ms = state.model_started_at.take().map_or(0, |started_at| {
                u64::try_from(started_at.elapsed().as_millis())
                    .unwrap_or(u64::MAX)
                    .max(1)
            });
            let usage = serde_json::from_value::<TurnUsage>(usage.clone()).unwrap_or_else(|_| {
                let input_tokens = usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let output_tokens = usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let cache_read_input_tokens = usage
                    .get("cache_read_input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let cache_creation_input_tokens = usage
                    .get("cache_creation_input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let total_tokens = input_tokens
                    .saturating_add(output_tokens)
                    .saturating_add(cache_read_input_tokens)
                    .saturating_add(cache_creation_input_tokens);
                TurnUsage {
                    turn: 0,
                    input_tokens,
                    output_tokens,
                    cache_read_input_tokens,
                    cache_creation_input_tokens,
                    total_tokens,
                    duration_ms: measured_duration_ms,
                    tokens_per_second_milli: if measured_duration_ms == 0 {
                        0
                    } else {
                        total_tokens.saturating_mul(1_000_000) / measured_duration_ms
                    },
                    estimated_cost_microusd: 0,
                    provenance: if total_tokens == 0 {
                        UsageProvenance::Estimated
                    } else {
                        UsageProvenance::ProviderReported
                    },
                }
            });
            state.current_context_tokens = usage
                .input_tokens
                .saturating_add(usage.cache_read_input_tokens)
                .saturating_add(usage.cache_creation_input_tokens);
            let _ = events.send(RuntimeEvent::Usage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_input_tokens: usage.cache_read_input_tokens,
                cache_creation_input_tokens: usage.cache_creation_input_tokens,
                total_tokens: usage.total_tokens,
                duration_ms: usage.duration_ms,
                tokens_per_second_milli: usage.tokens_per_second_milli,
                estimated_cost_microusd: usage.estimated_cost_microusd,
                provenance: usage.provenance,
            });
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
        AgentUpdate::AssistantText(text) => {
            if !text.trim().is_empty() {
                let _ = events.send(RuntimeEvent::AssistantText(text.clone()));
            }
        }
        AgentUpdate::Plan(steps) => {
            if !state.suppress_model_plan {
                let _ = events.send(RuntimeEvent::Plan(steps.clone()));
            }
        }
        AgentUpdate::Question(_) => {}
        AgentUpdate::ToolOutput {
            tool,
            output,
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
                    details: tool_output_details(output),
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
                    details: tool_output_details(output),
                },
            );
            let _ = events.send(RuntimeEvent::Activity(activity));
        }
        _ => {}
    }
}

fn is_internal_tool(tool: &str) -> bool {
    matches!(tool, "update_plan" | "ask_user_question")
}

pub(super) fn tool_title(tool: &str, arguments: &Value) -> String {
    match tool {
        "fs_read" => format!("Read({})", json_string(arguments, "path")),
        "fs_create_dir" => format!("Mkdir({})", json_string(arguments, "path")),
        "fs_write" => format!("Write({})", json_string(arguments, "path")),
        "search_text" => format!("Search({})", json_string(arguments, "query")),
        "semantic_capabilities" => "Semantic capability report".to_owned(),
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

fn tool_output_details(output: &str) -> Vec<String> {
    let output = output.trim();
    if output.is_empty() {
        Vec::new()
    } else {
        output.lines().map(str::to_owned).collect()
    }
}

fn summarize(value: &str) -> String {
    let compact = value.replace('\n', " ");
    if compact.chars().count() <= 140 {
        return compact;
    }
    compact.chars().take(137).chain("...".chars()).collect()
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
                if let Some(image) = encoded_image_info(&bytes, &file.path)? {
                    blocks.push(MessageBlock::Image {
                        source: ImageSource::Base64 {
                            media_type: image.media_type.to_owned(),
                            data: STANDARD.encode(bytes),
                        },
                        alt_text: file
                            .path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .map(str::to_owned),
                    });
                    continue;
                }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EncodedImageInfo {
    media_type: &'static str,
}

fn encoded_image_info(
    bytes: &[u8],
    path: &Path,
) -> Result<Option<EncodedImageInfo>, RuntimeError> {
    let info = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(parse_png_dimensions(bytes))
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        Some(parse_jpeg_dimensions(bytes))
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(parse_webp_dimensions(bytes))
    } else {
        None
    };
    let Some(info) = info else {
        return Ok(None);
    };
    let (media_type, width, height) = info.ok_or_else(|| RuntimeError::InvalidImage {
        path: path.to_path_buf(),
    })?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(RuntimeError::FileTooLarge {
            path: path.to_path_buf(),
            bytes: bytes.len(),
        });
    }
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0 || height == 0 || pixels > MAX_IMAGE_PIXELS {
        return Err(RuntimeError::ImagePixelLimit {
            path: path.to_path_buf(),
            pixels,
            limit: MAX_IMAGE_PIXELS,
        });
    }
    Ok(Some(EncodedImageInfo { media_type }))
}

fn parse_png_dimensions(bytes: &[u8]) -> Option<(&'static str, u32, u32)> {
    if bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
        return None;
    }
    Some((
        "image/png",
        u32::from_be_bytes(bytes[16..20].try_into().ok()?),
        u32::from_be_bytes(bytes[20..24].try_into().ok()?),
    ))
}

fn parse_jpeg_dimensions(bytes: &[u8]) -> Option<(&'static str, u32, u32)> {
    let mut cursor = 2_usize;
    while cursor + 1 < bytes.len() {
        while cursor < bytes.len() && bytes[cursor] != 0xff {
            cursor = cursor.saturating_add(1);
        }
        while cursor < bytes.len() && bytes[cursor] == 0xff {
            cursor = cursor.saturating_add(1);
        }
        let marker = *bytes.get(cursor)?;
        cursor = cursor.saturating_add(1);
        if matches!(marker, 0x01 | 0xd0..=0xd9) {
            continue;
        }
        let length = usize::from(u16::from_be_bytes([
            *bytes.get(cursor)?,
            *bytes.get(cursor.saturating_add(1))?,
        ]));
        if length < 2 || cursor.saturating_add(length) > bytes.len() {
            return None;
        }
        if matches!(
            marker,
            0xc0 | 0xc1 | 0xc2 | 0xc3 | 0xc5 | 0xc6 | 0xc7 | 0xc9 | 0xca | 0xcb | 0xcd
                | 0xce | 0xcf
        ) {
            let height = u32::from(u16::from_be_bytes([
                *bytes.get(cursor.saturating_add(3))?,
                *bytes.get(cursor.saturating_add(4))?,
            ]));
            let width = u32::from(u16::from_be_bytes([
                *bytes.get(cursor.saturating_add(5))?,
                *bytes.get(cursor.saturating_add(6))?,
            ]));
            return Some(("image/jpeg", width, height));
        }
        cursor = cursor.saturating_add(length);
    }
    None
}

fn parse_webp_dimensions(bytes: &[u8]) -> Option<(&'static str, u32, u32)> {
    let chunk = bytes.get(12..16)?;
    let payload = bytes.get(20..)?;
    match chunk {
        b"VP8X" if payload.len() >= 10 => {
            let width = 1 + u32::from(payload[4])
                + (u32::from(payload[5]) << 8)
                + (u32::from(payload[6]) << 16);
            let height = 1 + u32::from(payload[7])
                + (u32::from(payload[8]) << 8)
                + (u32::from(payload[9]) << 16);
            Some(("image/webp", width, height))
        }
        b"VP8 " if payload.len() >= 10 && payload[3..6] == [0x9d, 0x01, 0x2a] => {
            let width = u32::from(u16::from_le_bytes([payload[6], payload[7]]) & 0x3fff);
            let height = u32::from(u16::from_le_bytes([payload[8], payload[9]]) & 0x3fff);
            Some(("image/webp", width, height))
        }
        b"VP8L" if payload.len() >= 5 && payload[0] == 0x2f => {
            let bits = u32::from_le_bytes([payload[1], payload[2], payload[3], payload[4]]);
            let width = (bits & 0x3fff) + 1;
            let height = ((bits >> 14) & 0x3fff) + 1;
            Some(("image/webp", width, height))
        }
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn effort_and_provider_helpers_cover_all_variants() {
        assert_eq!(turns_for_effort(Effort::Low), 64);
        assert_eq!(turns_for_effort(Effort::Medium), 200);
        assert_eq!(turns_for_effort(Effort::High), 500);
        assert!(is_supported_provider("minimax"));
        assert!(is_supported_provider("anthropic"));
        assert!(is_supported_provider("anthropic-compatible"));
        assert!(is_supported_provider("openai"));
        assert!(is_supported_provider("openai-oauth"));
        assert!(is_supported_provider("openai-compatible"));
        assert!(is_supported_provider("omniroute"));
        assert!(is_supported_provider("local"));
        assert!(!is_supported_provider("other"));
        assert_eq!(protocol_for_provider("anthropic"), "anthropic");
        assert_eq!(protocol_for_provider("openai"), "openai");
        assert_eq!(credential_environment("minimax"), Some("MINIMAX_API_KEY"));
        assert_eq!(
            credential_environment("anthropic"),
            Some("ANTHROPIC_API_KEY")
        );
        assert_eq!(
            credential_environment("anthropic-compatible"),
            Some("MEDUSA_API_KEY")
        );
        assert_eq!(credential_environment("openai"), Some("OPENAI_API_KEY"));
        assert_eq!(
            credential_environment("openai-compatible"),
            Some("MEDUSA_API_KEY")
        );
        assert_eq!(credential_environment("openai-oauth"), None);
        assert_eq!(credential_environment("omniroute"), None);
        assert_eq!(credential_environment("local"), None);
        assert_eq!(credential_environment("other"), None);
        assert!(!should_auto_compact(399_999, 1_000_000, 40));
        assert!(should_auto_compact(400_000, 1_000_000, 40));
    }

    #[test]
    fn runtime_context_budget_uses_every_fixed_vendor_limit() {
        for provider in ["minimax", "anthropic", "openai", "openai-oauth"] {
            let catalog =
                medusa_config::provider_catalog_entry(provider).expect("fixed vendor catalog");
            for model in catalog.known_models {
                let expected = medusa_config::model_registry::model_context_limit(provider, model)
                    .expect("fixed vendor model must have context metadata");
                assert_eq!(
                    model_context_window_tokens(provider, model, 777_777),
                    expected
                );
            }
        }
    }

    #[test]
    fn runtime_context_budget_preserves_non_authoritative_provider_defaults() {
        assert_eq!(
            model_context_window_tokens("local", "MiniMax-M3", 777_777),
            777_777
        );
        assert_eq!(
            model_context_window_tokens("openai-compatible", "gpt-5.1", 333_333),
            333_333
        );
        assert_eq!(
            model_context_window_tokens("anthropic-compatible", "claude-opus-4-6", 222_222),
            222_222
        );
        assert_eq!(
            model_context_window_tokens("omniroute", "gpt-5", 111_111),
            111_111
        );
        assert_eq!(
            model_context_window_tokens("openai", "custom-model", 999_999),
            999_999
        );
    }

    #[test]
    fn formatting_helpers_cover_empty_and_non_empty_inputs() {
        assert_eq!(
            json_string(&json!({"path": "src/lib.rs"}), "path"),
            "src/lib.rs"
        );
        assert_eq!(json_string(&json!({}), "path"), "");
        assert_eq!(shell_command(&json!({"program": "cargo"})), "cargo");
        assert_eq!(
            shell_command(&json!({"program": "cargo", "args": ["test", "-q"]})),
            "cargo test -q"
        );
        assert_eq!(summarize("short line"), "short line");
        assert!(summarize(&"x".repeat(150)).ends_with("..."));
        assert!(tool_output_details("  \n").is_empty());
        assert_eq!(
            tool_output_details("stdout line\nstderr: command failed\n"),
            vec!["stdout line", "stderr: command failed"]
        );
    }

    #[test]
    fn objective_helper_handles_text_and_attachment_only_prompts() {
        let text = PromptDraft {
            text: "  fix it  ".to_owned(),
            ..PromptDraft::default()
        };
        assert_eq!(objective_for(&text), "fix it");
        let attachments = PromptDraft {
            attachments: vec![PromptAttachment::Image(ImageAttachment {
                display_name: "screen.png".to_owned(),
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 255],
                source_format: Some("image/rgba8".to_owned()),
            })],
            ..PromptDraft::default()
        };
        assert_eq!(
            objective_for(&attachments),
            "Use the 1 attached item(s) as context and complete the coding task."
        );
    }

    #[test]
    fn encoded_jpeg_dimensions_are_detected() {
        let bytes = vec![
            0xff, 0xd8, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x02, 0x00, 0x03, 0x03, 0x01,
            0x11, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00, 0xff, 0xd9,
        ];
        assert_eq!(
            parse_jpeg_dimensions(&bytes),
            Some(("image/jpeg", 3, 2))
        );
    }

    #[test]
    fn encoded_image_dimensions_are_bounded() {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&100_000_u32.to_be_bytes());
        bytes.extend_from_slice(&100_000_u32.to_be_bytes());
        let error = encoded_image_info(&bytes, Path::new("large.png"))
            .expect_err("reject large image");
        assert!(matches!(error, RuntimeError::ImagePixelLimit { .. }));
    }
}
