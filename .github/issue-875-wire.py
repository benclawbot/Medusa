from pathlib import Path

lib = Path("crates/medusa-runtime/src/lib.rs")
text = lib.read_text()
module_anchor = "mod mutating_worker_coordinator;\n"
if "mod repository_context;\n" not in text:
    if module_anchor not in text:
        raise SystemExit("module anchor missing")
    text = text.replace(module_anchor, module_anchor + "mod repository_context;\n", 1)

if "let repository_context = crate::repository_context::assemble_and_render(" not in text:
    trajectory = text.index("                let trajectory_context = crate::coding_trajectory::sync_and_render(")
    turn_start = text.index("                let turn_context =", trajectory)
    call_start = text.index(
        "                match engine.step_with_observer_and_context_and_turn_instruction_for_phase(",
        turn_start,
    )
    replacement = """                let repository_context = crate::repository_context::assemble_and_render(
                    &state.repo,
                    &session,
                    &draft.text,
                )?;
                let turn_context = format!(
                    "{skill_context}\\n\\n{trajectory_context}\\n\\n{repository_context}"
                );
"""
    text = text[:turn_start] + replacement + text[call_start:]
lib.write_text(text)

module = Path("crates/medusa-runtime/src/repository_context.rs")
source = module.read_text()
source = source.replace("step.description.trim()", "step.title.trim()")
source = source.replace(
    "    use medusa_agent::AgentEngine;\n    use medusa_config::Config;\n    use medusa_provider::ScriptedProvider;\n",
    "    use medusa_agent::AgentEngine;\n    use medusa_config::Config;\n    use medusa_core::MedusaResult;\n    use medusa_provider::{ModelProvider, ModelRequest, ModelResponse};\n",
)
source = source.replace(
    "    fn session(repo: &Path, objective: &str) -> AgentSession {\n        let engine = AgentEngine::new(ScriptedProvider::new(Vec::new()), Config::default());\n        engine.create_session(repo, objective).expect(\"session\")\n    }\n",
    "    struct UnusedProvider;\n\n    impl ModelProvider for UnusedProvider {\n        fn complete(&self, _: &ModelRequest) -> MedusaResult<ModelResponse> {\n            unreachable!(\"not used\")\n        }\n    }\n\n    fn session(repo: &Path, objective: &str) -> AgentSession {\n        let engine = AgentEngine::new(UnusedProvider, Config::default());\n        engine\n            .create_session(repo, objective.to_owned())\n            .expect(\"session\")\n    }\n",
)
module.write_text(source)
