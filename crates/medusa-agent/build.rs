use std::{env, fs, path::PathBuf};

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

const CONTEXT_RECOVERY_REQUEST_BLOCK: &str = r#"        let system = system_prompt_with_context(
            self.config.agent.mode,
            &session.repo,
            additional_system_context,
        );
        let tools = available_tools(self.config.agent.mode, &self.desktop_commander_settings);
        let budget = crate::context_budget::PromptBudget::for_request(
            &system,
            &session.messages,
            &tools,
            self.config.model.max_output_tokens,
            crate::context_budget::configured_context_window_tokens(),
        );
        let mut compacted = false;
        if budget.requires_compaction() {
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
        let response = match self.provider.complete(&request) {
            Ok(response) => response,
            Err(error)
                if crate::context_budget::is_context_limit_rejection(&error.to_string()) =>
            {
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
"#;

fn main() {
    println!("cargo:rerun-if-changed=src/engine_base.rs");
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let source_path = manifest_dir.join("src/engine_base.rs");
    let source = fs::read_to_string(&source_path).expect("read engine source");
    let occurrences = source.matches(ORIGINAL_REQUEST_BLOCK).count();
    assert_eq!(
        occurrences, 1,
        "expected exactly one model request block in {}",
        source_path.display()
    );
    let generated = source.replacen(
        ORIGINAL_REQUEST_BLOCK,
        CONTEXT_RECOVERY_REQUEST_BLOCK,
        1,
    );
    let output_path = PathBuf::from(env::var_os("OUT_DIR").expect("out dir")).join("engine.rs");
    fs::write(output_path, generated).expect("write generated engine source");
}
