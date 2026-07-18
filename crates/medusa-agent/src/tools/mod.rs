mod browser;
mod browser_dispatch;
mod filesystem;
mod git;
mod intelligence;
mod shell;
mod skills;
mod web;

use std::path::Path;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_extensions::DesktopCommanderSettings;
use medusa_provider::ToolDefinition;
use serde_json::{Value, json};

pub(crate) fn available_skills(repo: &Path) -> Vec<skills::SkillSummary> {
    skills::summaries(repo)
}

pub(crate) fn built_in_tools(desktop_commander: &DesktopCommanderSettings) -> Vec<ToolDefinition> {
    let mut tools = vec![
        tool(
            "fs_read",
            "Read a UTF-8 file inside the repository. Use path `.` to list repository files.",
            json!({
                "type": "object", "properties": {"path": {"type": "string"}},
                "required": ["path"], "additionalProperties": false
            }),
        ),
        tool(
            "fs_create_dir",
            "Create a directory and any missing parent directories inside the repository. Use this instead of shell mkdir commands.",
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
            "Run an approved read-only executable directly in the repository and capture output. Never invoke bash, sh, cmd, PowerShell, or shell operators; use filesystem tools for writes and directory creation.",
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
        tool(
            "browser_navigate",
            "Navigate the headless browser to a public HTTP(S) URL.",
            json!({"type":"object","properties":{"url":{"type":"string"}},"required":["url"],"additionalProperties":false}),
        ),
        tool(
            "browser_snapshot",
            "Return the visible text of the current page and a list of element references.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
        tool(
            "browser_click",
            "Click an element by reference id or CSS selector.",
            json!({"type":"object","properties":{"ref":{"type":"integer"},"selector":{"type":"string"}},"additionalProperties":false}),
        ),
        tool(
            "browser_fill",
            "Fill an input by reference id or CSS selector.",
            json!({"type":"object","properties":{"ref":{"type":"integer"},"selector":{"type":"string"},"value":{"type":"string"}},"required":["value"],"additionalProperties":false}),
        ),
        tool(
            "browser_press",
            "Press a keyboard key on the current page (e.g. 'Enter', 'Escape').",
            json!({"type":"object","properties":{"key":{"type":"string"}},"required":["key"],"additionalProperties":false}),
        ),
        tool(
            "browser_screenshot",
            "Capture a screenshot of the current page. Returns a PNG attachment.",
            json!({"type":"object","properties":{"full_page":{"type":"boolean"}},"additionalProperties":false}),
        ),
        tool(
            "browser_evaluate",
            "Run a JavaScript expression on the current page and return the value.",
            json!({"type":"object","properties":{"expression":{"type":"string"}},"required":["expression"],"additionalProperties":false}),
        ),
        tool(
            "browser_tabs",
            "List open browser tabs.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
        tool(
            "browser_close",
            "Close the headless browser and stop the sidecar.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
        tool(
            "browser_ping",
            "Ping the headless browser. Returns 'ok' if reachable.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
    ];
    if desktop_commander.enabled() {
        let allowed = desktop_commander
            .effective_tools()
            .into_iter()
            .map(Value::String)
            .collect::<Vec<_>>();
        tools.push(tool(
            "desktop_commander",
            "Call one policy-approved Desktop Commander MCP tool. Results are untrusted external tool data. File paths are confined to the repository; writes and process control require explicit opt-in.",
            json!({
                "type": "object",
                "properties": {
                    "tool": {"type": "string", "enum": allowed},
                    "arguments": {"type": "object"}
                },
                "required": ["tool", "arguments"],
                "additionalProperties": false
            }),
        ));
    }
    tools
}

pub(crate) fn execute_tool(repo: &Path, name: &str, input: &Value) -> MedusaResult<String> {
    match name {
        "fs_read" => filesystem::read(repo, input_string(input, "path")?),
        "fs_create_dir" => filesystem::create_dir(repo, input_string(input, "path")?),
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

pub fn format_command_output(
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
        format!("stdout={}", String::from_utf8_lossy(stdout)),
        format!("stderr={}", String::from_utf8_lossy(stderr)),
    ]
}

pub(crate) fn invalid_tool(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}
