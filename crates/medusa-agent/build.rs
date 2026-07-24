use std::{env, error::Error, fs, path::PathBuf};

const ORIGINAL_REQUEST_BLOCK: &str = r#"        let response = self.provider.complete(&ModelRequest {
            system: system_prompt_with_context(
                self.config.agent.mode,
                &session.repo,
                additional_system_context,
            ),
            messages: session.messages.clone(),
            tools: available_tools(self.config.agent.mode, &self.desktop_commander_settings),
            max_tokens: self.config.model.max_output_tokens,
            temperature_milli: self.config.model.temperature_milli,
        })?;
"#;

const CONTEXT_RECOVERY_REQUEST_BLOCK: &str = r#"        if let Some(refresh) = repository_index::refresh(&session.repo)? {
            observer(&AgentUpdate::ToolOutput {
                tool: "code_index".to_owned(),
                output: repository_index::summary(&refresh),
                is_error: false,
            });
        }
        let system = coding_policy::apply(
            system_prompt_with_context(
                self.config.agent.mode,
                &session.repo,
                additional_system_context,
            ),
            self.config.agent.mode,
        );
        let tools = available_tools(self.config.agent.mode, &self.desktop_commander_settings);
        let budget = context_budget::PromptBudget::for_request(
            &system,
            &session.messages,
            &tools,
            self.config.model.max_output_tokens,
            context_budget::configured_context_window_tokens(),
        );
        let _remaining_context_tokens = budget.remaining_tokens();
        let _request_exceeds_context_window = budget.exceeds_context_window();
        let mut compacted = false;
        if matches!(
            budget.decision(),
            context_budget::PromptBudgetDecision::Compact
        ) {
            compact_session(
                session,
                Some("preserve the current objective, decisions, tool results, and pending work"),
            )?;
            validate_messages(&session.messages, &self.provider.capabilities())?;
            compacted = true;
        }
        let mut request = ModelRequest {
            system,
            messages: session.messages.clone(),
            tools,
            max_tokens: self.config.model.max_output_tokens,
            temperature_milli: self.config.model.temperature_milli,
        };
        let request_started = std::time::Instant::now();
        let response = match self.provider.complete(&request) {
            Ok(response) => response,
            Err(error) if context_budget::is_context_limit_rejection(&error.to_string()) => {
                if !compacted {
                    compact_session(
                        session,
                        Some(
                            "recover from the provider context limit while preserving the current objective, decisions, tool results, and pending work",
                        ),
                    )?;
                    validate_messages(&session.messages, &self.provider.capabilities())?;
                    request.messages = session.messages.clone();
                }
                self.provider.complete(&request)?
            }
            Err(error) => return Err(error),
        };
        let turn_usage = crate::session::record_turn_usage(
            session.turn,
            &request,
            &response,
            request_started.elapsed(),
        );
"#;

const ORIGINAL_USAGE_EVENT: &str =
    "                usage: serde_json::to_value(response.usage).map_err(json_error)?,";
const ACCOUNTED_USAGE_EVENT: &str =
    "                usage: serde_json::to_value(turn_usage).map_err(json_error)?,";

fn replace_once(
    source: String,
    original: &str,
    replacement: &str,
    description: &str,
    source_path: &std::path::Path,
) -> Result<String, Box<dyn Error>> {
    let occurrences = source.matches(original).count();
    if occurrences != 1 {
        return Err(format!(
            "expected exactly one {description} in {}, found {occurrences}",
            source_path.display()
        )
        .into());
    }
    Ok(source.replacen(original, replacement, 1))
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=src/engine_base.rs");
    println!("cargo:rerun-if-changed=src/context_budget.rs");
    println!("cargo:rerun-if-changed=src/coding_policy.rs");
    println!("cargo:rerun-if-changed=src/repository_index.rs");
    println!("cargo:rerun-if-changed=src/usage.rs");
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let source_path = manifest_dir.join("src/engine_base.rs");
    let source = fs::read_to_string(&source_path)?.replace("\r\n", "\n");
    let engine = replace_once(
        source,
        ORIGINAL_REQUEST_BLOCK,
        CONTEXT_RECOVERY_REQUEST_BLOCK,
        "model request block",
        &source_path,
    )?;
    let engine = replace_once(
        engine,
        ORIGINAL_USAGE_EVENT,
        ACCOUNTED_USAGE_EVENT,
        "model usage event",
        &source_path,
    )?;
    let generated = format!(
        "mod context_budget {{ include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/context_budget.rs\")); }}\nmod coding_policy {{ include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/coding_policy.rs\")); }}\nmod repository_index {{ include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/repository_index.rs\")); }}\n{engine}"
    );
    let output_path = PathBuf::from(env::var("OUT_DIR")?).join("engine.rs");
    fs::write(output_path, generated)?;
    Ok(())
}
