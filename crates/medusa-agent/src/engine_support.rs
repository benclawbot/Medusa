use std::{collections::HashMap, fs, path::Path};

use medusa_capabilities::CapabilityRegistry;
use medusa_config::Mode;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_extensions::{DesktopCommanderSettings, desktop_commander_tool_is_mutating};
use medusa_protocol::{Actor, EventPayload};
use medusa_provider::{ImageSource, Message, MessageBlock, ProviderCapabilities, Role};
use time::OffsetDateTime;

#[path = "coding_policy.rs"]
mod coding_policy;

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

pub(crate) fn system_prompt_with_context(
    mode: Mode,
    repo: &Path,
    additional_context: Option<&str>,
) -> String {
    let base = if mode == Mode::ReadOnly {
        PLAN_SYSTEM_PROMPT
    } else {
        SYSTEM_PROMPT
    };
    let mut prompt = format!("{base}\n\nWorkspace: {}", repo.display());
    if mode != Mode::ReadOnly {
        let coding_policy = coding_policy::prompt_fragment();
        if !coding_policy.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&coding_policy);
        }
    }
    match CapabilityRegistry::discover(repo) {
        Ok(registry) => {
            prompt.push_str("\n\nRuntime capabilities (shared with every Medusa frontend):\n");
            prompt.push_str(&registry.prompt_summary());
        }
        Err(error) => prompt.push_str(&format!(
            "\n\nRuntime capability discovery unavailable: {error}"
        )),
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
    desktop_commander: &DesktopCommanderSettings,
) -> Vec<medusa_provider::ToolDefinition> {
    built_in_tools(desktop_commander, mode == Mode::ReadOnly)
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
                | "code_index"
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
