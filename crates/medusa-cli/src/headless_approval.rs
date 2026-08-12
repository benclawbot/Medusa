use std::{collections::BTreeSet, fs, path::{Path, PathBuf}};

use medusa_agent::AgentQuestion;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

#[derive(Debug)]
pub(crate) struct HeadlessApprovalPolicy {
    source: PathBuf,
    commands: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ApprovalMatch {
    Approved(String),
    Missing(String),
    NotApproval,
}

impl HeadlessApprovalPolicy {
    pub(crate) fn load(
        non_interactive: bool,
        approve_allowlist: Option<&Path>,
    ) -> MedusaResult<Option<Self>> {
        match (non_interactive, approve_allowlist) {
            (false, None) => Ok(None),
            (true, Some(path)) => {
                let text = fs::read_to_string(path).map_err(|error| {
                    MedusaError::new(
                        ErrorCode::InvalidConfiguration,
                        ErrorCategory::Validation,
                        format!(
                            "failed to read approval allowlist {}: {error}",
                            path.display()
                        ),
                    )
                })?;
                let commands = text
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .map(normalize_command)
                    .collect::<BTreeSet<_>>();
                Ok(Some(Self {
                    source: path.to_path_buf(),
                    commands,
                }))
            }
            _ => Err(MedusaError::new(
                ErrorCode::InvalidConfiguration,
                ErrorCategory::Validation,
                "--non-interactive and --approve-allowlist must be used together",
            )),
        }
    }

    pub(crate) fn matches(&self, question: &AgentQuestion) -> ApprovalMatch {
        let Some(command) = approval_command(question) else {
            return ApprovalMatch::NotApproval;
        };
        if self.commands.contains(&normalize_command(&command)) {
            ApprovalMatch::Approved(command)
        } else {
            ApprovalMatch::Missing(command)
        }
    }

    pub(crate) fn source(&self) -> &Path {
        &self.source
    }
}

fn approval_command(question: &AgentQuestion) -> Option<String> {
    let encoded = serde_json::to_value(question).ok()?;
    let approval = encoded.get("approval")?;
    if approval.get("tool")?.as_str()? != "shell_run" {
        return None;
    }
    let input = approval.get("input")?;
    let program = input.get("program")?.as_str()?.trim();
    let args = input
        .get("args")?
        .as_array()?
        .iter()
        .map(serde_json::Value::as_str)
        .collect::<Option<Vec<_>>>()?;
    Some(
        std::iter::once(program)
            .chain(args)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn normalize_command(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn approval_question(program: &str, args: &[&str]) -> AgentQuestion {
        let input = json!({"program": program, "args": args});
        let value = json!({
            "tool_use_id": "tool-1",
            "questions": [{
                "header": "Permission",
                "question": "Allow Medusa to run the command?",
                "options": [],
                "multi_select": false
            }],
            "approval": {
                "tool_use_id": "tool-1",
                "tool": "shell_run",
                "input": input,
                "grant": {
                    "scope": {
                        "tool": "shell_run",
                        "action_fingerprint": "fixture-action",
                        "plan_fingerprint": "fixture-plan"
                    },
                    "approved_at": "2026-07-27T12:00:00Z",
                    "expires_at": "2026-07-27T12:05:00Z"
                }
            }
        });
        serde_json::from_value(value).expect("approval question")
    }

    #[test]
    fn exact_shell_command_is_extracted_from_approval_payload() {
        assert_eq!(
            approval_command(&approval_question("cargo", &["test", "-p", "medusa-cli"])),
            Some("cargo test -p medusa-cli".to_owned())
        );
    }

    #[test]
    fn whitespace_is_normalized_for_allowlist_matching() {
        assert_eq!(normalize_command(" cargo   test  "), "cargo test");
    }
}

// Workflow synchronization marker for issue #380.
