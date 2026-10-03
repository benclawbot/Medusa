use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use medusa_agent::AgentQuestion;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

#[derive(Debug)]
pub(crate) struct HeadlessApprovalPolicy {
    source: PathBuf,
    commands: BTreeSet<Vec<String>>,
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
                let mut commands = BTreeSet::new();
                for (index, line) in text.lines().enumerate() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    // Compare argv, not a flattened command: whitespace within an argument
                    // can change interpreter code, and empty arguments are significant.
                    let command = shlex::split(line)
                        .filter(|words| {
                            words.first().is_some_and(|program| !program.is_empty())
                                && words.iter().all(|word| !word.contains('\0'))
                        })
                        .ok_or_else(|| {
                            MedusaError::new(
                                ErrorCode::InvalidConfiguration,
                                ErrorCategory::Validation,
                                format!("invalid command on approval allowlist line {}", index + 1),
                            )
                        })?;
                    commands.insert(command);
                }
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
        let rendered = match shlex::try_join(command.iter().map(String::as_str)) {
            Ok(rendered) => rendered,
            Err(_) => return ApprovalMatch::Missing("command contains a NUL byte".to_owned()),
        };
        if self.commands.contains(&command) {
            ApprovalMatch::Approved(rendered)
        } else {
            ApprovalMatch::Missing(rendered)
        }
    }

    pub(crate) fn source(&self) -> &Path {
        &self.source
    }
}

fn approval_command(question: &AgentQuestion) -> Option<Vec<String>> {
    let encoded = serde_json::to_value(question).ok()?;
    let approval = encoded.get("approval")?;
    if approval.get("tool")?.as_str()? != "shell_run" {
        return None;
    }
    let input = approval.get("input")?;
    let program = input.get("program")?.as_str()?;
    let args = input
        .get("args")?
        .as_array()?
        .iter()
        .map(serde_json::Value::as_str)
        .collect::<Option<Vec<_>>>()?;
    Some(
        std::iter::once(program)
            .chain(args)
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    )
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
            Some(vec![
                "cargo".to_owned(),
                "test".to_owned(),
                "-p".to_owned(),
                "medusa-cli".to_owned()
            ])
        );
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let policy = policy("\n # comment\n cargo   test \n");
        assert!(matches!(
            policy.matches(&approval_question("cargo", &["test"])),
            ApprovalMatch::Approved(_)
        ));
    }

    fn policy(text: &str) -> HeadlessApprovalPolicy {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("approve.txt");
        fs::write(&path, text).expect("allowlist");
        HeadlessApprovalPolicy::load(true, Some(&path))
            .expect("valid allowlist")
            .expect("headless policy")
    }

    #[test]
    fn allowlist_does_not_approve_newlines_hidden_inside_an_argument() {
        let allowed = "if False: print('noop'); open('approval-sentinel', 'w').close()";
        let altered = "if False: print('noop');\nopen('approval-sentinel', 'w').close()";
        for script in [allowed, altered] {
            medusa_agent::validate_shell_command("python3", &["-c".into(), script.into()])
                .expect("canonical command policy admits both scripts");
        }
        let policy = policy(&format!("python3 -c {allowed}\n"));
        assert!(matches!(
            policy.matches(&approval_question("python3", &["-c", altered])),
            ApprovalMatch::Missing(_)
        ));
    }

    #[test]
    fn quoted_allowlist_preserves_argument_contents_and_boundaries() {
        let policy = policy(
            "python3 -c \"if False: print('noop'); open('approval-sentinel', 'w').close()\"\necho 'two  spaces' ''\n",
        );
        assert!(matches!(
            policy.matches(&approval_question(
                "python3",
                &[
                    "-c",
                    "if False: print('noop'); open('approval-sentinel', 'w').close()"
                ]
            )),
            ApprovalMatch::Approved(_)
        ));
        assert!(matches!(
            policy.matches(&approval_question(
                "python3",
                &[
                    "-c",
                    "if False: print('noop');\nopen('approval-sentinel', 'w').close()"
                ]
            )),
            ApprovalMatch::Missing(_)
        ));
        assert!(matches!(
            policy.matches(&approval_question("echo", &["two  spaces", ""])),
            ApprovalMatch::Approved(_)
        ));
        for args in [
            &["two spaces", ""][..],
            &["two", "spaces", ""],
            &["two  spaces"],
        ] {
            assert!(matches!(
                policy.matches(&approval_question("echo", args)),
                ApprovalMatch::Missing(_)
            ));
        }
    }

    #[test]
    fn unquoted_allowlist_does_not_merge_or_drop_arguments() {
        let policy = policy(" cargo   test -p medusa-cli \n");
        assert!(matches!(
            policy.matches(&approval_question("cargo", &["test", "-p", "medusa-cli"])),
            ApprovalMatch::Approved(_)
        ));
        for args in [
            &["test -p", "medusa-cli"][..],
            &["test", "-p", "medusa-cli", ""],
        ] {
            assert!(matches!(
                policy.matches(&approval_question("cargo", args)),
                ApprovalMatch::Missing(_)
            ));
        }
        assert!(matches!(
            policy.matches(&approval_question(" cargo", &["test", "-p", "medusa-cli"])),
            ApprovalMatch::Missing(_)
        ));
    }

    #[test]
    fn malformed_allowlist_entries_are_rejected() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("approve.txt");
        for text in [
            "echo 'unclosed",
            "echo dangling\\",
            "'' argument",
            "echo nul\0byte",
        ] {
            fs::write(&path, text).expect("allowlist");
            assert!(HeadlessApprovalPolicy::load(true, Some(&path)).is_err());
        }
    }

    #[test]
    fn quoted_windows_paths_preserve_literal_backslashes() {
        let policy = policy("'C:\\Tools Folder\\python.exe' -c 'print(\"ok\")'\n");
        assert!(matches!(
            policy.matches(&approval_question(
                "C:\\Tools Folder\\python.exe",
                &["-c", "print(\"ok\")"]
            )),
            ApprovalMatch::Approved(_)
        ));
    }

    #[test]
    fn requested_nul_bytes_cannot_receive_approval() {
        let policy = policy("echo safe\n");
        assert!(matches!(
            policy.matches(&approval_question("echo", &["safe\0"])),
            ApprovalMatch::Missing(_)
        ));
    }
}

// Workflow synchronization marker for issue #380.
