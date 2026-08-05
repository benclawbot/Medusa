mod browser;
mod browser_dispatch;
mod filesystem;
mod git;
mod intelligence;
mod shell;
pub(crate) mod skills;
mod web;

use std::path::{Path, PathBuf};

use medusa_capabilities::{CapabilityRegistry, SystemProbe};
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_extensions::DesktopCommanderSettings;
use medusa_provider::ToolDefinition;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

/// Single policy-aware registry for built-in tools shared by every agent frontend.
#[derive(Clone, Debug)]
pub struct ToolManager {
    desktop_commander: DesktopCommanderSettings,
}

impl ToolManager {
    #[must_use]
    pub fn new(desktop_commander: DesktopCommanderSettings) -> Self {
        Self { desktop_commander }
    }

    /// Compatibility projection for callers without a repository context. Discovery failure
    /// fails closed by returning no model tools.
    #[must_use]
    pub fn definitions(&self, read_only: bool) -> Vec<ToolDefinition> {
        CapabilityRegistry::discover_with_desktop(
            PathBuf::from("."),
            &SystemProbe,
            self.desktop_commander.clone(),
        )
        .map_or_else(|_| Vec::new(), |registry| registry.model_tools(read_only))
    }

    pub fn definitions_for(
        &self,
        repo: &Path,
        read_only: bool,
    ) -> MedusaResult<Vec<ToolDefinition>> {
        built_in_tools(repo, &self.desktop_commander, read_only)
    }

    pub fn execute(&self, repo: &Path, name: &str, input: &Value) -> MedusaResult<String> {
        let registry = CapabilityRegistry::discover_with_desktop(
            repo.to_path_buf(),
            &SystemProbe,
            self.desktop_commander.clone(),
        )?;
        let id = format!("tool.{name}");
        let entry = registry
            .entry(&id)
            .ok_or_else(|| invalid_tool(format!("tool is not registered: {name}")))?;
        if !entry.projected_to(medusa_capabilities::CapabilitySurface::Model) {
            return Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                format!("tool is unavailable: {name}: {}", entry.readiness.detail),
            ));
        }
        execute_tool(repo, name, input)
    }

    pub fn execute_approved(&self, repo: &Path, name: &str, input: &Value) -> MedusaResult<String> {
        execute_approved_tool(repo, name, input)
    }
}

pub(crate) fn available_skills(repo: &Path) -> Vec<skills::SkillSummary> {
    skills::summaries(repo)
}

pub(crate) fn built_in_tools(
    repo: &Path,
    desktop_commander: &DesktopCommanderSettings,
    read_only: bool,
) -> MedusaResult<Vec<ToolDefinition>> {
    CapabilityRegistry::discover_with_desktop(
        repo.to_path_buf(),
        &SystemProbe,
        desktop_commander.clone(),
    )
    .map(|registry| registry.model_tools(read_only))
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
        "semantic_capabilities" => intelligence::semantic_capabilities(),
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
            let output_mode = crate::output_envelope::OutputMode::parse(input_optional_string(
                input,
                "output_mode",
            )?)?;
            shell::run(repo, program, &args, output_mode)
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

pub(crate) fn execute_approved_tool(
    repo: &Path,
    name: &str,
    input: &Value,
) -> MedusaResult<String> {
    match name {
        "fs_create_dir" => filesystem::create_dir_approved(input_string(input, "path")?),
        "fs_write" => filesystem::write_approved(
            input_string(input, "path")?,
            input_string(input, "content")?,
        ),
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
            let output_mode = crate::output_envelope::OutputMode::parse(input_optional_string(
                input,
                "output_mode",
            )?)?;
            shell::run_approved(repo, program, &args, output_mode)
        }
        _ => Err(MedusaError::new(
            ErrorCode::PolicyDenied,
            ErrorCategory::Policy,
            format!("{name} cannot be authorized interactively"),
        )),
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

pub(crate) fn input_string<'a>(input: &'a Value, key: &str) -> MedusaResult<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_tool(format!("{key} must be a string")))
}

fn input_optional_string<'a>(input: &'a Value, key: &str) -> MedusaResult<Option<&'a str>> {
    match input.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| invalid_tool(format!("{key} must be a string"))),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_string_rejects_present_non_string_values() {
        assert_eq!(
            input_optional_string(&json!({}), "output_mode").expect("absent mode"),
            None
        );

        let normal = json!({"output_mode": "normal"});
        assert_eq!(
            input_optional_string(&normal, "output_mode").expect("string mode"),
            Some("normal")
        );

        for invalid in [Value::Null, json!(42), json!({"mode": "compact"})] {
            let input = json!({"output_mode": invalid});
            let error = input_optional_string(&input, "output_mode")
                .expect_err("present non-string mode must fail");
            assert!(error.to_string().contains("output_mode must be a string"));
        }
    }
}
