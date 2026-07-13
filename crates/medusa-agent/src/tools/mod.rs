mod filesystem;
mod git;
mod intelligence;
mod shell;
mod skills;
mod web;

use std::path::Path;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_provider::ToolDefinition;
use serde_json::{Value, json};

const MAX_TOOL_OUTPUT_BYTES: usize = 1_000_000;

pub(crate) fn available_skills(repo: &Path) -> Vec<skills::SkillSummary> {
    skills::summaries(repo)
}

pub(crate) fn built_in_tools() -> Vec<ToolDefinition> {
    vec![
        tool(
            "fs_read",
            "Read a UTF-8 file inside the repository. Use path `.` to list repository files.",
            json!({
                "type": "object", "properties": {"path": {"type": "string"}},
                "required": ["path"], "additionalProperties": false
            }),
        ),
        tool(
            "fs_write",
            "Atomically write a UTF-8 file inside the repository.",
            json!({
                "type": "object", "properties": {
                    "path": {"type": "string"}, "content": {"type": "string"}
                }, "required": ["path", "content"], "additionalProperties": false
            }),
        ),
        tool(
            "search_text",
            "Search UTF-8 repository files for an exact text fragment.",
            json!({
                "type": "object", "properties": {"query": {"type": "string"}},
                "required": ["query"], "additionalProperties": false
            }),
        ),
        tool(
            "code_index",
            "Build the Tree-sitter Rust symbol/reference index and optionally query one identifier.",
            json!({
                "type": "object", "properties": {"name": {"type": "string"}},
                "additionalProperties": false
            }),
        ),
        tool(
            "patch_apply",
            "Apply a guarded atomic multi-file byte-range patch transaction.",
            json!({
                "type": "object", "properties": {"edits": {"type": "array", "items": {
                    "type": "object", "properties": {
                        "path": {"type": "string"},
                        "start_byte": {"type": "integer", "minimum": 0},
                        "end_byte": {"type": "integer", "minimum": 0},
                        "expected": {"type": "string"},
                        "replacement": {"type": "string"}
                    }, "required": ["path", "start_byte", "end_byte", "expected", "replacement"],
                    "additionalProperties": false
                }}}, "required": ["edits"], "additionalProperties": false
            }),
        ),
        tool(
            "symbol_rename",
            "Rename one Rust identifier across indexed definitions and references using a guarded transaction.",
            json!({
                "type": "object", "properties": {
                    "old_name": {"type": "string"}, "new_name": {"type": "string"}
                }, "required": ["old_name", "new_name"], "additionalProperties": false
            }),
        ),
        tool(
            "shell_run",
            "Run a non-destructive command in the repository and capture output.",
            json!({
                "type": "object", "properties": {
                    "program": {"type": "string"},
                    "args": {"type": "array", "items": {"type": "string"}}
                }, "required": ["program", "args"], "additionalProperties": false
            }),
        ),
        tool(
            "web_search",
            "Search the public web for current information. Optionally restrict or exclude domains.",
            json!({
                "type": "object", "properties": {
                    "query": {"type": "string"},
                    "allowed_domains": {"type": "array", "items": {"type": "string"}},
                    "blocked_domains": {"type": "array", "items": {"type": "string"}}
                }, "required": ["query"], "additionalProperties": false
            }),
        ),
        tool(
            "web_fetch",
            "Fetch a public HTTP(S) page and return readable text. Use `prompt` to state what information to extract.",
            json!({
                "type": "object", "properties": {
                    "url": {"type": "string"},
                    "prompt": {"type": "string"}
                }, "required": ["url"], "additionalProperties": false
            }),
        ),
        tool(
            "skill_read",
            "Read an available Medusa or Claude skill's instructions before applying that skill. Use the skill name and optional project or user scope.",
            json!({
                "type": "object", "properties": {
                    "name": {"type": "string"},
                    "scope": {"type": "string", "enum": ["project", "user"]}
                }, "required": ["name"], "additionalProperties": false
            }),
        ),
        tool(
            "update_plan",
            "Create or update the visible task plan. Use it before meaningful work, and update statuses as work progresses. This does not modify repository files.",
            json!({
                "type": "object", "properties": {
                    "steps": {"type": "array", "minItems": 1, "maxItems": 8, "items": {
                        "type": "object", "properties": {
                            "title": {"type": "string"},
                            "status": {"type": "string", "enum": ["pending", "in_progress", "completed", "failed"]}
                        }, "required": ["title", "status"], "additionalProperties": false
                    }}
                }, "required": ["steps"], "additionalProperties": false
            }),
        ),
        tool(
            "ask_user_question",
            "Ask one to four blocking multiple-choice clarification questions. The session pauses until the user reviews and confirms every answer; never ask blocking questions in ordinary assistant text.",
            json!({
                "type": "object", "properties": {
                    "questions": {"type": "array", "minItems": 1, "maxItems": 4, "items": {
                        "type": "object", "properties": {
                            "header": {"type": "string", "maxLength": 12},
                            "question": {"type": "string"},
                            "options": {"type": "array", "minItems": 2, "maxItems": 4, "items": {
                                "type": "object", "properties": {
                                    "label": {"type": "string"},
                                    "description": {"type": "string"}
                                }, "required": ["label", "description"], "additionalProperties": false
                            }},
                            "multiSelect": {"type": "boolean"}
                        }, "required": ["header", "question", "options"], "additionalProperties": false
                    }}
                }, "required": ["questions"], "additionalProperties": false
            }),
        ),
        tool(
            "git_checkpoint",
            "Stage all changes and create a Git checkpoint commit.",
            json!({
                "type": "object", "properties": {"message": {"type": "string"}},
                "required": ["message"], "additionalProperties": false
            }),
        ),
    ]
}

pub(crate) fn execute_tool(repo: &Path, name: &str, input: &Value) -> MedusaResult<String> {
    match name {
        "fs_read" => filesystem::read(repo, input_string(input, "path")?),
        "fs_write" => filesystem::write(
            repo,
            input_string(input, "path")?,
            input_string(input, "content")?,
        ),
        "search_text" => filesystem::search(repo, input_string(input, "query")?),
        "code_index" => intelligence::code_index(repo, input),
        "patch_apply" => intelligence::patch_apply(repo, input),
        "symbol_rename" => intelligence::symbol_rename(repo, input),
        "shell_run" => {
            let program = input_string(input, "program")?;
            let args = input
                .get("args")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid_tool("args must be an array"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| invalid_tool("every arg must be a string"))
                })
                .collect::<MedusaResult<Vec<_>>>()?;
            shell::run(repo, program, &args)
        }
        "web_search" => web::search(
            input_string(input, "query")?,
            input_domains(input, "allowed_domains")?,
            input_domains(input, "blocked_domains")?,
        ),
        "web_fetch" => web::fetch(
            input_string(input, "url")?,
            input.get("prompt").and_then(Value::as_str),
        ),
        "skill_read" => skills::read(
            repo,
            input_string(input, "name")?,
            input.get("scope").and_then(Value::as_str),
        ),
        "git_checkpoint" => git::checkpoint(repo, input_string(input, "message")?),
        _ => Err(invalid_tool(format!("unknown tool: {name}"))),
    }
}

fn input_domains(input: &Value, key: &str) -> MedusaResult<Vec<String>> {
    let Some(domains) = input.get(key) else {
        return Ok(Vec::new());
    };
    domains
        .as_array()
        .ok_or_else(|| invalid_tool(format!("{key} must be an array")))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(|domain| domain.trim().to_ascii_lowercase())
                .filter(|domain| !domain.is_empty())
                .ok_or_else(|| {
                    invalid_tool(format!("every {key} entry must be a non-empty string"))
                })
        })
        .collect()
}

fn tool(name: &str, description: &str, input_schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        input_schema,
    }
}

pub(crate) fn input_string<'a>(input: &'a Value, key: &str) -> MedusaResult<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_tool(format!("{key} must be a string")))
}

pub(crate) fn input_usize(input: &Value, key: &str) -> MedusaResult<usize> {
    input
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| invalid_tool(format!("{key} must be a non-negative integer")))
}

pub(crate) fn format_command_output(
    program: &str,
    args: &[impl AsRef<str>],
    stdout: &[u8],
    stderr: &[u8],
) -> Vec<String> {
    vec![
        format!(
            "command={} {}",
            program,
            args.iter()
                .map(|arg| arg.as_ref())
                .collect::<Vec<_>>()
                .join(" ")
        ),
        format!(
            "stdout={}",
            truncate(String::from_utf8_lossy(stdout).into_owned())
        ),
        format!(
            "stderr={}",
            truncate(String::from_utf8_lossy(stderr).into_owned())
        ),
    ]
}

pub(crate) fn truncate(mut value: String) -> String {
    if value.len() > MAX_TOOL_OUTPUT_BYTES {
        value.truncate(MAX_TOOL_OUTPUT_BYTES);
        value.push_str("\n[truncated]");
    }
    value
}

pub(crate) fn invalid_tool(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}
