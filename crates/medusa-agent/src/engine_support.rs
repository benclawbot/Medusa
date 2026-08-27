use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::Path,
};

use medusa_capabilities::CapabilityRegistry;
use medusa_config::Mode;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_extensions::{DesktopCommanderSettings, desktop_commander_tool_is_mutating};
use medusa_protocol::{Actor, EventPayload};
use medusa_provider::{
    ImageSource, Message, MessageBlock, ProviderCapabilities, ProviderExecutionPhase,
};
use time::OffsetDateTime;

use crate::{
    engine::{AgentUpdate, PLAN_SYSTEM_PROMPT, SYSTEM_PROMPT},
    evidence::append_event,
    output_envelope::{EnvelopeConfig, OutputEnvelope},
    session::{
        AgentPlanStep, AgentPlanStepStatus, AgentQuestion, AgentQuestionItem, AgentQuestionOption,
        AgentSession, persist,
    },
    tools::{available_skills, built_in_tools},
};

const MAX_REPOSITORY_INSTRUCTIONS_BYTES: usize = 32_000;

pub(crate) fn content_with_session_goal(
    mut content: Vec<MessageBlock>,
    objective: &str,
) -> Vec<MessageBlock> {
    content.insert(
        0,
        MessageBlock::Text {
            text: format!("Current session goal: {objective}"),
        },
    );
    content
}

pub(crate) const fn provider_execution_phase(mode: Mode) -> ProviderExecutionPhase {
    match mode {
        Mode::ReadOnly => ProviderExecutionPhase::Planning,
        Mode::Review => ProviderExecutionPhase::HighRiskReview,
        Mode::Yolo => ProviderExecutionPhase::Implementation,
    }
}

#[cfg(test)]
pub(crate) fn system_prompt_with_context(
    mode: Mode,
    repo: &Path,
    additional_context: Option<&str>,
) -> String {
    match CapabilityRegistry::discover(repo) {
        Ok(registry) => system_prompt_with_registry(mode, repo, additional_context, &registry),
        Err(error) => {
            system_prompt_with_discovery_error(mode, repo, additional_context, &error.to_string())
        }
    }
}

pub(crate) fn system_prompt_with_registry(
    mode: Mode,
    repo: &Path,
    additional_context: Option<&str>,
    registry: &CapabilityRegistry,
) -> String {
    system_prompt_with_capability_state(mode, repo, additional_context, Some(registry), None)
}

pub(crate) fn system_prompt_with_discovery_error(
    mode: Mode,
    repo: &Path,
    additional_context: Option<&str>,
    error: &str,
) -> String {
    system_prompt_with_capability_state(mode, repo, additional_context, None, Some(error))
}

fn system_prompt_with_capability_state(
    mode: Mode,
    repo: &Path,
    additional_context: Option<&str>,
    registry: Option<&CapabilityRegistry>,
    discovery_error: Option<&str>,
) -> String {
    let base = if mode == Mode::ReadOnly {
        PLAN_SYSTEM_PROMPT
    } else {
        SYSTEM_PROMPT
    };
    let mut prompt = format!("{base}\n\nWorkspace: {}", repo.display());
    prompt.push_str("\n\nGENERAL CONVERSATION RULE — If the user asks casual conversation, a factual explanation, a confirmation, or research rather than repository work, answer directly in one turn. Do not inspect the repository, make a plan, or call coding/file/shell tools unless the request actually requires them. Use web tools only when current or source-linked information is needed. A clear text answer is complete; do not invent follow-up work.");
    if let Some(registry) = registry {
        prompt.push_str("\n\nRuntime capabilities (shared with every Medusa frontend):\n");
        prompt.push_str(&registry.prompt_summary());
    } else if let Some(error) = discovery_error {
        prompt.push_str(&format!(
            "\n\nRuntime capability discovery unavailable: {error}"
        ));
    }
    let instructions = repository_instructions(repo);
    if instructions.is_empty() {
        prompt.push_str("\n\nNo repository instruction files were found.");
    } else {
        prompt.push_str("\n\nRepository instructions (follow them as project constraints; the user request and system rules take precedence):\n");
        prompt.push_str(&instructions);
    }
    let skills = available_skills(repo);
    if skills.is_empty() {
        prompt.push_str("\n\nNo Medusa skills are installed for this workspace or user.");
    } else {
        prompt.push_str(
            "\n\nAvailable skills: call `skill_read` before applying a relevant skill.\n",
        );
        for skill in skills {
            prompt.push_str(&format!(
                "- {} ({}){}\n",
                skill.name,
                skill.scope,
                skill
                    .description
                    .map(|description| format!(": {description}"))
                    .unwrap_or_default()
            ));
        }
    }
    if let Some(context) = additional_context
        .map(str::trim)
        .filter(|context| !context.is_empty())
    {
        prompt.push_str("\n\nExplicit user-selected context for the current agent turn:\n");
        prompt.push_str(context);
    }
    prompt
}

pub(crate) fn available_tools(
    mode: Mode,
    repo: &Path,
    desktop_commander: &DesktopCommanderSettings,
) -> MedusaResult<Vec<medusa_provider::ToolDefinition>> {
    built_in_tools(repo, desktop_commander, mode == Mode::ReadOnly).map(|tools| {
        tools
            .into_iter()
            .filter(|tool| tool_allowed(mode, &tool.name))
            .collect()
    })
}

pub(crate) fn available_tools_from_registry(
    mode: Mode,
    registry: &CapabilityRegistry,
) -> Vec<medusa_provider::ToolDefinition> {
    registry
        .model_tools(mode == Mode::ReadOnly)
        .into_iter()
        .filter(|tool| tool_allowed(mode, &tool.name))
        .collect()
}

pub(crate) fn tool_allowed(mode: Mode, tool: &str) -> bool {
    mode != Mode::ReadOnly
        || matches!(
            tool,
            "fs_read"
                | "search_text"
                | "semantic_capabilities"
                | "code_index"
                | "typescript_semantic"
                | "web_search"
                | "web_fetch"
                | "skill_read"
                | "update_plan"
                | "ask_user_question"
                | "desktop_commander"
        )
}

pub(crate) fn question_from_input(
    tool_use_id: String,
    input: &serde_json::Value,
) -> MedusaResult<AgentQuestion> {
    let questions = input
        .get("questions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid_question("questions must be an array"))?;
    if questions.is_empty() || questions.len() > 4 {
        return Err(invalid_question(
            "ask_user_question accepts between 1 and 4 questions",
        ));
    }
    let questions = questions
        .iter()
        .map(question_item_from_input)
        .collect::<MedusaResult<Vec<_>>>()?;
    Ok(AgentQuestion {
        tool_use_id: Some(tool_use_id),
        questions,
        legacy_question: None,
        legacy_options: Vec::new(),
        approval: None,
    })
}

fn question_item_from_input(input: &serde_json::Value) -> MedusaResult<AgentQuestionItem> {
    let header = input
        .get("header")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_question("every question header must not be empty"))?;
    let header = compact_question_header(header);
    let question = input
        .get("question")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.chars().count() <= 500)
        .ok_or_else(|| invalid_question("every question must be 1 to 500 characters"))?;
    let options = input
        .get("options")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid_question("every question needs an options array"))?;
    if !(2..=4).contains(&options.len()) {
        return Err(invalid_question(
            "every question needs between 2 and 4 options",
        ));
    }
    let options = options
        .iter()
        .map(|option| {
            let label = option
                .get("label")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty() && value.chars().count() <= 80)
                .ok_or_else(|| invalid_question("every option label must be 1 to 80 characters"))?;
            let description = option
                .get("description")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| value.chars().count() <= 240)
                .ok_or_else(|| {
                    invalid_question("every option description must be at most 240 characters")
                })?;
            Ok(AgentQuestionOption {
                label: label.to_owned(),
                description: description.to_owned(),
            })
        })
        .collect::<MedusaResult<Vec<_>>>()?;
    Ok(AgentQuestionItem {
        header,
        question: question.to_owned(),
        options,
        multi_select: input
            .get("multiSelect")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

fn compact_question_header(header: &str) -> String {
    const MAX_HEADER_CHARS: usize = 12;

    let normalized = header.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX_HEADER_CHARS {
        return normalized;
    }

    let mut compact = String::new();
    for word in normalized.split(' ') {
        let candidate = if compact.is_empty() {
            word.to_owned()
        } else {
            format!("{compact} {word}")
        };
        if candidate.chars().count() > MAX_HEADER_CHARS {
            break;
        }
        compact = candidate;
    }
    if compact.is_empty() {
        normalized.chars().take(MAX_HEADER_CHARS).collect()
    } else {
        compact
    }
}

pub(crate) fn question_from_assistant_text(text: &str) -> Option<AgentQuestion> {
    let question = text.lines().find_map(|line| {
        let line = line.trim().trim_start_matches(|character: char| {
            character.is_ascii_digit() || matches!(character, '-' | '*' | ' ' | '#' | '.' | ')')
        });
        let question_end = line.find('?')?;
        let candidate = line[..=question_end]
            .replace("**", "")
            .replace("__", "")
            .replace('`', "");
        let normalized = candidate.to_ascii_lowercase();
        let asks_for_input = [
            "which ", "what ", "where ", "when ", "who ", "why ", "how ", "do you ", "does ",
            "would ", "could ", "can you ", "should ", "please ", "provide ", "confirm ",
        ]
        .iter()
        .any(|prefix| normalized.starts_with(prefix));
        asks_for_input.then_some(candidate)
    })?;
    Some(AgentQuestion {
        tool_use_id: None,
        questions: vec![AgentQuestionItem {
            header: "Question".to_owned(),
            question,
            options: Vec::new(),
            multi_select: false,
        }],
        legacy_question: None,
        legacy_options: Vec::new(),
        approval: None,
    })
}

pub(crate) fn pause_for_question<F>(
    session: &mut AgentSession,
    question: AgentQuestion,
    observer: &mut F,
) -> MedusaResult<()>
where
    F: FnMut(&AgentUpdate),
{
    session.pending_question = Some(question.clone());
    append_observed(
        session,
        EventPayload::QuestionRequested {
            question: serde_json::to_value(&question).map_err(json_error)?,
        },
        observer,
    )?;
    if let Some(approval) = question.approval.as_ref() {
        append_observed(
            session,
            EventPayload::ApprovalRequested {
                request: serde_json::to_value(approval).map_err(json_error)?,
            },
            observer,
        )?;
    }
    append_observed(
        session,
        EventPayload::SessionPaused {
            reason: "waiting for a user response".to_owned(),
        },
        observer,
    )?;
    observer(&AgentUpdate::Question(question));
    session.updated_at = OffsetDateTime::now_utc();
    persist(session)
}

fn invalid_question(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

pub(crate) fn plan_from_input(input: &serde_json::Value) -> Vec<AgentPlanStep> {
    let Some(steps) = input.get("steps").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    steps
        .iter()
        .take(8)
        .enumerate()
        .map(|(index, step)| {
            let title = step
                .get("title")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .map(compact_plan_title)
                .unwrap_or_else(|| format!("Step {}", index.saturating_add(1)));
            let status = match step
                .get("status")
                .and_then(serde_json::Value::as_str)
                .map(|status| status.trim().to_ascii_lowercase())
                .as_deref()
            {
                Some("pending") => AgentPlanStepStatus::Pending,
                Some("in_progress" | "in progress") => AgentPlanStepStatus::InProgress,
                Some("completed") => AgentPlanStepStatus::Completed,
                Some("failed") => AgentPlanStepStatus::Failed,
                _ => AgentPlanStepStatus::Pending,
            };
            AgentPlanStep { title, status }
        })
        .collect()
}

fn compact_plan_title(title: &str) -> String {
    const MAX_TITLE_CHARS: usize = 140;
    title.chars().take(MAX_TITLE_CHARS).collect()
}

/// Default envelope configuration for a session. Artifact root lives inside
/// the session's `.medusa/artifacts/` so the full body is recovered by path.
/// `Task 11/12` will plumb real config knobs; these defaults keep the
/// engine pipeline runnable before then.
pub(crate) fn default_envelope_config(repo: &Path) -> EnvelopeConfig {
    EnvelopeConfig {
        head_bytes: 4 * 1024,
        tail_bytes: 4 * 1024,
        max_artifact_bytes: 8 * 1024 * 1024,
        session_root: repo.join(".medusa"),
    }
}

/// Render an `OutputEnvelope` for the model's tool message. The Display
/// impl is full-fidelity; the compact form drops the path display and is
/// used inside the JSON envelope the model sees.
pub(crate) fn compact_envelope_for_model(envelope: &OutputEnvelope) -> String {
    if envelope.tail.is_empty() {
        return envelope.head.clone();
    }
    format!(
        "{}\n…\n{}\n({} lines, {} bytes, full body at {})",
        envelope.head,
        envelope.tail,
        envelope.line_count,
        envelope.byte_count,
        envelope.path.display()
    )
}

fn tool_call_mutates_repository(tool: &str, input: &serde_json::Value) -> bool {
    if let Some(desktop_tool) = tool.strip_prefix("desktop_commander:") {
        return desktop_commander_tool_is_mutating(desktop_tool);
    }
    crate::tool_dag::invalidates_repository_revision(tool, input)
}

fn successful_mutating_tool_calls(session: &AgentSession) -> Vec<(&str, &serde_json::Value)> {
    let mut pending = HashMap::<&str, VecDeque<&serde_json::Value>>::new();
    let mut successful = Vec::new();
    for event in &session.events {
        match &event.payload {
            EventPayload::ToolCallRequested { tool, arguments } => {
                pending
                    .entry(tool.as_str())
                    .or_default()
                    .push_back(arguments);
            }
            EventPayload::ToolExecutionCompleted { tool, exit_code } => {
                let input = pending.get_mut(tool.as_str()).and_then(VecDeque::pop_back);
                if *exit_code == Some(0)
                    && let Some(input) = input
                    && tool_call_mutates_repository(tool, input)
                {
                    successful.push((tool.as_str(), input));
                }
            }
            _ => {}
        }
    }
    successful
}

pub(crate) fn has_mutating_tool_result(session: &AgentSession) -> bool {
    !successful_mutating_tool_calls(session).is_empty()
}

fn finalize_mutation_paths(mut paths: Vec<String>, has_mutations: bool) -> Vec<String> {
    if paths.is_empty() && has_mutations {
        paths.push(".".to_owned());
    }
    paths.sort();
    paths.dedup();
    paths
}

pub(crate) fn successful_mutation_paths(session: &AgentSession) -> Vec<String> {
    let mutations = successful_mutating_tool_calls(session);
    let mut paths = Vec::new();
    for (_, input) in &mutations {
        collect_mutation_paths(input, &mut paths);
    }
    finalize_mutation_paths(paths, !mutations.is_empty())
}

fn collect_mutation_paths(value: &serde_json::Value, paths: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if matches!(
                    key.as_str(),
                    "path"
                        | "file"
                        | "file_path"
                        | "source"
                        | "destination"
                        | "old_path"
                        | "new_path"
                ) {
                    if let Some(path) = value.as_str() {
                        paths.push(path.to_owned());
                        continue;
                    }
                }
                collect_mutation_paths(value, paths);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_mutation_paths(value, paths);
            }
        }
        _ => {}
    }
}

pub(crate) fn plan_is_complete(session: &AgentSession) -> bool {
    session.plan.is_empty()
        || session
            .plan
            .iter()
            .all(|step| step.status == AgentPlanStepStatus::Completed)
}

fn repository_instructions(repo: &Path) -> String {
    let mut remaining = MAX_REPOSITORY_INSTRUCTIONS_BYTES;
    let mut output = String::new();
    for name in ["AGENTS.md", "MEDUSA.md", ".medusa/AGENTS.md"] {
        if remaining == 0 {
            break;
        }
        let path = repo.join(name);
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let content = truncate_for_prompt(&content, remaining);
        remaining = remaining.saturating_sub(content.len());
        output.push_str(&format!("\n--- {name} ---\n{content}\n"));
    }
    output
}

fn truncate_for_prompt(content: &str, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content.to_owned();
    }
    let mut end = max_bytes;
    while !content.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}\n[truncated]", &content[..end])
}

/// Updates a session goal without requiring a live model provider.
pub fn update_session_objective(session: &mut AgentSession, objective: String) -> MedusaResult<()> {
    session.objective = objective.clone();
    append_event(
        session,
        Actor::User,
        EventPayload::GoalUpdated { objective },
    )?;
    session.updated_at = OffsetDateTime::now_utc();
    persist(session)
}

/// Compacts durable session history into a crash-safe V2 hybrid manifest.
pub fn compact_session(session: &mut AgentSession, focus: Option<&str>) -> MedusaResult<()> {
    compact_session_with_semantic(session, focus, None)
}

pub(crate) fn compact_session_with_semantic(
    session: &mut AgentSession,
    focus: Option<&str>,
    semantic: Option<crate::compaction_v2::ValidatedSemanticSummary>,
) -> MedusaResult<()> {
    let original_messages = session.messages.len();
    let source_event_sequences = session
        .events
        .iter()
        .map(|event| event.sequence)
        .collect::<Vec<_>>();
    let migrated_v1 = session.messages.iter().flat_map(|message| &message.content).any(|block| matches!(block, MessageBlock::Text { text } if text.starts_with("[medusa-compaction-v1]")));
    let prepared = crate::compaction_v2::prepare_with_semantic(session, focus, semantic)?;
    let generation = prepared.manifest.generation;
    let mut preserved_sections = prepared.preserved_sections;
    preserved_sections.push(format!("migrated_v1={migrated_v1}"));
    if !session.tool_artifacts.contains(&prepared.manifest_path) {
        session.tool_artifacts.push(prepared.manifest_path.clone());
    }
    session.messages = prepared.projection;
    append_event(
        session,
        Actor::Coordinator,
        EventPayload::ConversationCompacted {
            original_messages: u32::try_from(original_messages).unwrap_or(u32::MAX),
            retained_messages: u32::try_from(session.messages.len()).unwrap_or(u32::MAX),
            generation,
            source_event_sequences,
            preserved_sections,
        },
    )?;
    session.updated_at = OffsetDateTime::now_utc();
    persist(session)
}

pub(crate) fn compact_message_text(content: &[MessageBlock]) -> String {
    content
        .iter()
        .map(compact_block_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn compact_block_text(block: &MessageBlock) -> String {
    const MAX_BLOCK_CHARS: usize = 600;
    let text = match block {
        MessageBlock::Text { text } => text.clone(),
        MessageBlock::Image { alt_text, .. } => {
            format!("[image: {}]", alt_text.as_deref().unwrap_or("attachment"))
        }
        MessageBlock::ToolUse { name, input, .. } => format!("used {name} with {input}"),
        MessageBlock::ToolResult {
            content, is_error, ..
        } => format!(
            "tool {}: {content}",
            if *is_error { "error" } else { "result" }
        ),
    };
    let compact = text.replace('\n', " ");
    if compact.chars().count() <= MAX_BLOCK_CHARS {
        compact
    } else {
        compact
            .chars()
            .take(MAX_BLOCK_CHARS.saturating_sub(3))
            .chain("...".chars())
            .collect()
    }
}

pub(crate) fn append_observed<F>(
    session: &mut AgentSession,
    payload: EventPayload,
    observer: &mut F,
) -> MedusaResult<()>
where
    F: FnMut(&AgentUpdate),
{
    append_event(session, Actor::Coordinator, payload.clone())?;
    session.updated_at = OffsetDateTime::now_utc();
    persist(session)?;
    observer(&AgentUpdate::Event(payload));
    Ok(())
}

pub(crate) fn validate_messages(
    messages: &[Message],
    capabilities: &ProviderCapabilities,
) -> MedusaResult<()> {
    for message in messages {
        validate_user_content(&message.content, capabilities)?;
    }
    Ok(())
}

pub(crate) fn validate_user_content(
    content: &[MessageBlock],
    capabilities: &ProviderCapabilities,
) -> MedusaResult<()> {
    let images = content
        .iter()
        .filter_map(|block| match block {
            MessageBlock::Image { source, .. } => Some(source),
            _ => None,
        })
        .collect::<Vec<_>>();
    if images.is_empty() {
        return Ok(());
    }
    if !capabilities.image_input {
        return Err(MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Validation,
            "the active provider cannot consume image attachments; submission was blocked",
        ));
    }
    if capabilities
        .max_images_per_request
        .is_some_and(|limit| images.len() > limit as usize)
    {
        return Err(MedusaError::new(
            ErrorCode::DependencyUnavailable,
            ErrorCategory::Validation,
            format!(
                "prompt contains {} images, exceeding the provider limit",
                images.len()
            ),
        ));
    }
    for source in images {
        if let ImageSource::Base64 { media_type, data } = source {
            if !capabilities.supported_image_media_types.is_empty()
                && !capabilities
                    .supported_image_media_types
                    .iter()
                    .any(|supported| supported == media_type)
            {
                return Err(MedusaError::new(
                    ErrorCode::DependencyUnavailable,
                    ErrorCategory::Validation,
                    format!("provider does not support image media type {media_type}"),
                ));
            }
            if capabilities
                .max_image_bytes
                .is_some_and(|limit| estimated_base64_bytes(data) > limit)
            {
                return Err(MedusaError::new(
                    ErrorCode::DependencyUnavailable,
                    ErrorCategory::Validation,
                    "image attachment exceeds the provider byte limit",
                ));
            }
        }
    }
    Ok(())
}

fn estimated_base64_bytes(data: &str) -> u64 {
    let padding = data
        .as_bytes()
        .iter()
        .rev()
        .take_while(|byte| **byte == b'=')
        .count() as u64;
    (data.len() as u64).saturating_mul(3) / 4 - padding.min(2)
}

pub(crate) fn json_error(error: serde_json::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn image_block(media_type: &str, data: &str) -> MessageBlock {
        MessageBlock::Image {
            source: ImageSource::Base64 {
                media_type: media_type.to_owned(),
                data: data.to_owned(),
            },
            alt_text: Some("screenshot".to_owned()),
        }
    }

    #[test]
    fn image_submission_is_blocked_for_text_only_provider() {
        let error = validate_user_content(
            &[image_block("image/png", "AAEC")],
            &ProviderCapabilities::default(),
        )
        .expect_err("block unsupported image");
        assert!(error.message.contains("cannot consume image"));
    }

    #[test]
    fn supported_image_content_passes_validation() {
        let capabilities = ProviderCapabilities {
            image_input: true,
            supported_image_media_types: vec!["image/png".to_owned()],
            max_image_bytes: Some(1024),
            max_images_per_request: Some(2),
            tool_calling: true,
            streaming: false,
        };
        validate_user_content(&[image_block("image/png", "AAEC")], &capabilities)
            .expect("accept supported image");
    }

    #[test]
    fn unsupported_media_type_is_rejected() {
        let capabilities = ProviderCapabilities {
            image_input: true,
            supported_image_media_types: vec!["image/png".to_owned()],
            max_image_bytes: None,
            max_images_per_request: None,
            tool_calling: true,
            streaming: false,
        };
        let error = validate_user_content(&[image_block("image/tiff", "AAEC")], &capabilities)
            .expect_err("reject unsupported type");
        assert!(error.message.contains("image/tiff"));
    }

    #[test]
    fn provider_phase_tracks_production_agent_mode() {
        assert_eq!(
            provider_execution_phase(Mode::ReadOnly),
            ProviderExecutionPhase::Planning
        );
        assert_eq!(
            provider_execution_phase(Mode::Review),
            ProviderExecutionPhase::HighRiskReview
        );
        assert_eq!(
            provider_execution_phase(Mode::Yolo),
            ProviderExecutionPhase::Implementation
        );
    }

    #[test]
    fn web_tools_are_available_in_standard_and_planning_modes() {
        for mode in [Mode::Yolo, Mode::ReadOnly] {
            let directory = tempfile::tempdir().expect("tempdir");
            let tools =
                available_tools(mode, directory.path(), &DesktopCommanderSettings::default())
                    .expect("available tools")
                    .into_iter()
                    .map(|tool| tool.name)
                    .collect::<Vec<_>>();
            assert!(tools.contains(&"web_search".to_owned()));
            assert!(tools.contains(&"web_fetch".to_owned()));
            assert!(tools.contains(&"skill_read".to_owned()));
            assert!(tools.contains(&"update_plan".to_owned()));
        }
    }

    #[test]
    fn initial_model_turn_includes_the_durable_session_goal() {
        let content = content_with_session_goal(
            vec![MessageBlock::Text {
                text: "Build the portfolio page".to_owned(),
            }],
            "Create a responsive portfolio",
        );
        assert!(matches!(
            content.first(),
            Some(MessageBlock::Text { text }) if text == "Current session goal: Create a responsive portfolio"
        ));
        assert!(matches!(
            content.get(1),
            Some(MessageBlock::Text { text }) if text == "Build the portfolio page"
        ));
    }

    #[test]
    fn workspace_instructions_and_skills_are_added_to_the_model_context() {
        let directory = tempdir().expect("temporary directory");
        fs::write(
            directory.path().join("AGENTS.md"),
            "Run the focused test suite.",
        )
        .expect("write instructions");
        let skill = directory.path().join(".medusa/skills/release/SKILL.md");
        fs::create_dir_all(skill.parent().expect("skill directory"))
            .expect("create skill directory");
        fs::write(
            &skill,
            "description: Release preparation\nUse the release checklist.",
        )
        .expect("write skill");

        let prompt = system_prompt_with_context(Mode::Yolo, directory.path(), None);
        assert!(prompt.contains("Runtime capabilities (shared with every Medusa frontend):"));
        assert!(prompt.contains("Run the focused test suite."));
        assert!(prompt.contains("release (project): Release preparation"));
        assert!(prompt.contains("call `skill_read`"));
    }

    #[test]
    fn plain_text_clarification_falls_back_to_one_clean_question() {
        let question = question_from_assistant_text(
            "Please answer these:\n1. **What kind of website is it?**\n2. **Who is the audience?**",
        )
        .expect("question");
        assert_eq!(question.tool_use_id, None);
        assert_eq!(question.questions.len(), 1);
        assert_eq!(
            question.questions[0].question,
            "What kind of website is it?"
        );
    }

    #[test]
    fn production_mutation_classifier_covers_all_repository_write_lanes() {
        assert!(tool_call_mutates_repository(
            "apply_structured_patch",
            &json!({"repository_revision":"r","edits":[{"path":"src/lib.rs"}]})
        ));
        assert!(tool_call_mutates_repository(
            "shell_run",
            &json!({"program":"cargo","args":["fmt"]})
        ));
        assert!(tool_call_mutates_repository(
            "desktop_commander:write_file",
            &json!({"tool":"write_file","arguments":{"path":"src/lib.rs"}})
        ));
        assert!(!tool_call_mutates_repository(
            "inspect_target",
            &json!({"path":"src/lib.rs"})
        ));
    }

    #[test]
    fn mutation_path_collection_handles_structured_and_nested_adapters() {
        let mut paths = Vec::new();
        collect_mutation_paths(
            &json!({
                "edits": [{"path":"src/lib.rs"}, {"file_path":"src/main.rs"}],
                "arguments": {"destination":"generated/out.rs"}
            }),
            &mut paths,
        );
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "generated/out.rs".to_owned(),
                "src/lib.rs".to_owned(),
                "src/main.rs".to_owned()
            ]
        );
    }

    #[test]
    fn pathless_mutation_uses_repository_wide_verification_scope() {
        assert_eq!(finalize_mutation_paths(Vec::new(), true), vec!["."]);
        assert!(finalize_mutation_paths(Vec::new(), false).is_empty());
    }
}
