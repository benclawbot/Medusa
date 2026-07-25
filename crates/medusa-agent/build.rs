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
        let mut system = coding_policy::apply(
            system_prompt_with_context(
                self.config.agent.mode,
                &session.repo,
                additional_system_context,
            ),
            self.config.agent.mode,
        );
        let tools = available_tools(self.config.agent.mode, &self.desktop_commander_settings);
        let mut budget = context_budget::PromptBudget::for_request(
            &system,
            &session.messages,
            &tools,
            self.config.model.max_output_tokens,
            context_budget::configured_context_window_tokens(),
        );
        let repository_capacity = budget
            .compaction_threshold_tokens
            .saturating_sub(budget.estimated_total_tokens);
        if let Some(retrieval) = repository_index::retrieve_context(
            &session.repo,
            &session.objective,
            repository_capacity,
        )? {
            system.push_str("\n\n");
            system.push_str(&retrieval.system_fragment);
            observer(&AgentUpdate::ToolOutput {
                tool: "repository_context".to_owned(),
                output: retrieval.status,
                is_error: false,
            });
            budget = context_budget::PromptBudget::for_request(
                &system,
                &session.messages,
                &tools,
                self.config.model.max_output_tokens,
                context_budget::configured_context_window_tokens(),
            );
        }
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

const ORIGINAL_TOOL_RESULT_BLOCK: &str = r#"                let (raw_content, is_error, exit_code) = match result {
                    Ok(output) => (output, false, Some(0)),
                    Err(error) => (error.to_string(), true, Some(1)),
                };
"#;

const OBSERVED_TOOL_RESULT_BLOCK: &str = r#"                let (raw_content, is_error, exit_code) = match result {
                    Ok(output) => (output, false, Some(0)),
                    Err(error) => (error.to_string(), true, Some(1)),
                };
                world_model_observation::record_tool_observation(
                    session,
                    &name,
                    &input,
                    &raw_content,
                    if is_error { 1 } else { 0 },
                );
"#;

const ORIGINAL_BOOTSTRAP_BLOCK: &str = r#"        bootstrap(repo)?;
        let now = OffsetDateTime::now_utc();
"#;
const RECOVERED_BOOTSTRAP_BLOCK: &str = r#"        bootstrap(repo)?;
        medusa_intelligence::recover_patch_transactions(repo)?;
        let now = OffsetDateTime::now_utc();
"#;

const ORIGINAL_LOAD_BLOCK: &str = r#"    pub fn load_session(&self, repo: &Path, session: &str) -> MedusaResult<AgentSession> {
        load(repo, session)
    }
"#;
const RECOVERED_LOAD_BLOCK: &str = r#"    pub fn load_session(&self, repo: &Path, session: &str) -> MedusaResult<AgentSession> {
        medusa_intelligence::recover_patch_transactions(repo)?;
        load(repo, session)
    }
"#;

const ORIGINAL_VERIFICATION_BLOCK: &str = r#"            let verification = targeted_verification_for_paths(
                &session.repo,
                &successful_mutation_paths(session),
            )?;
"#;
const TRANSACTIONAL_VERIFICATION_BLOCK: &str = r#"            let mut verification = targeted_verification_for_paths(
                &session.repo,
                &successful_mutation_paths(session),
            )?;
            let transaction_ids = medusa_intelligence::finalize_patch_transactions(
                &session.repo,
                verification.passed,
            )?;
            verification.evidence.extend(transaction_ids.into_iter().map(|transaction_id| {
                if verification.passed {
                    format!("patch_transaction_committed={transaction_id}")
                } else {
                    format!("patch_transaction_rolled_back={transaction_id}")
                }
            }));
"#;

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

fn module_source(path: &std::path::Path) -> Result<String, Box<dyn Error>> {
    Ok(fs::read_to_string(path)?
        .replace("\r\n", "\n")
        .replacen("//!", "//", 1))
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=src/engine_base.rs");
    println!("cargo:rerun-if-changed=src/autonomous_execution.rs");
    println!("cargo:rerun-if-changed=src/autonomous_engine.rs");
    println!("cargo:rerun-if-changed=src/context_budget.rs");
    println!("cargo:rerun-if-changed=src/coding_policy.rs");
    println!("cargo:rerun-if-changed=src/repository_index.rs");
    println!("cargo:rerun-if-changed=src/usage.rs");
    println!("cargo:rerun-if-changed=src/world_model_observation.rs");
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
    let engine = replace_once(
        engine,
        ORIGINAL_TOOL_RESULT_BLOCK,
        OBSERVED_TOOL_RESULT_BLOCK,
        "tool result observation block",
        &source_path,
    )?;
    let engine = replace_once(
        engine,
        ORIGINAL_BOOTSTRAP_BLOCK,
        RECOVERED_BOOTSTRAP_BLOCK,
        "session bootstrap block",
        &source_path,
    )?;
    let engine = replace_once(
        engine,
        ORIGINAL_LOAD_BLOCK,
        RECOVERED_LOAD_BLOCK,
        "session load block",
        &source_path,
    )?;
    let engine = replace_once(
        engine,
        ORIGINAL_VERIFICATION_BLOCK,
        TRANSACTIONAL_VERIFICATION_BLOCK,
        "verification transaction block",
        &source_path,
    )?;
    let autonomous = module_source(&manifest_dir.join("src/autonomous_execution.rs"))?;
    let generated = format!(
        "#[allow(dead_code)]\nmod autonomous_execution {{\n{autonomous}\n}}\nmod context_budget {{ include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/context_budget.rs\")); }}\nmod coding_policy {{ include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/coding_policy.rs\")); }}\nmod repository_index {{ include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/repository_index.rs\")); }}\nmod world_model_observation {{ include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/world_model_observation.rs\")); }}\n{engine}\ninclude!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/autonomous_engine.rs\"));"
    );
    let output_path = PathBuf::from(env::var("OUT_DIR")?).join("engine.rs");
    fs::write(output_path, generated)?;
    Ok(())
}
