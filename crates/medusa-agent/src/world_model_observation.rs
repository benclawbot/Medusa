use std::path::PathBuf;

use medusa_world_model::{ObservationSource, load, persist};

use crate::session::AgentSession;

const MAX_OBSERVATION_CHARS: usize = 2_000;

pub(crate) fn record_tool_observation(
    session: &mut AgentSession,
    tool: &str,
    input: &serde_json::Value,
    output: &str,
    exit_code: i32,
) {
    let Some(reference) = session.world_model.as_mut() else {
        return;
    };
    let Ok(mut model) = load(&session.repo, reference) else {
        return;
    };
    model.record_observation(
        observation_source(tool, input, exit_code),
        bounded_statement(tool, output),
    );
    if persist(&session.repo, &reference.relative_path, &model).is_ok() {
        reference.revision = model.revision;
    }
}

fn observation_source(
    tool: &str,
    input: &serde_json::Value,
    exit_code: i32,
) -> ObservationSource {
    match tool {
        "fs_read" => ObservationSource::FileRead {
            path: input
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".")),
            content_hash: None,
        },
        "search_text" => ObservationSource::SearchResult {
            query: input
                .get("query")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        "shell_run" => {
            let program = input
                .get("program")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let args = input
                .get("args")
                .and_then(serde_json::Value::as_array)
                .map(|args| {
                    args.iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            ObservationSource::ShellCommand {
                command: format!("{program} {args}").trim().to_owned(),
                exit_code,
            }
        }
        _ => ObservationSource::Derived,
    }
}

fn bounded_statement(tool: &str, output: &str) -> String {
    let mut statement = format!("{tool} returned: {output}");
    if statement.chars().count() > MAX_OBSERVATION_CHARS {
        statement = statement.chars().take(MAX_OBSERVATION_CHARS).collect();
        statement.push_str("…[truncated]");
    }
    statement
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_observations_preserve_command_and_exit_code() {
        let source = observation_source(
            "shell_run",
            &serde_json::json!({"program": "cargo", "args": ["test", "-q"]}),
            1,
        );
        assert_eq!(
            source,
            ObservationSource::ShellCommand {
                command: "cargo test -q".to_owned(),
                exit_code: 1,
            }
        );
    }

    #[test]
    fn observation_statements_are_bounded() {
        let statement = bounded_statement("fs_read", &"x".repeat(MAX_OBSERVATION_CHARS * 2));
        assert!(statement.chars().count() <= MAX_OBSERVATION_CHARS + 20);
        assert!(statement.ends_with("…[truncated]"));
    }
}
