use std::{env, fs, path::PathBuf};

fn replace_once(source: &mut String, needle: &str, replacement: &str) {
    let position = source
        .find(needle)
        .unwrap_or_else(|| panic!("runtime generation anchor not found: {needle}"));
    source.replace_range(position..position + needle.len(), replacement);
}

fn bind_module(source: &mut String, manifest: &str, declaration: &str, file: &str) {
    let path = PathBuf::from(manifest)
        .join("src")
        .join(file)
        .display()
        .to_string()
        .replace('\\', "/");
    replace_once(
        source,
        declaration,
        &format!("#[path = \"{path}\"]\n{declaration}"),
    );
}

fn main() {
    println!("cargo:rerun-if-changed=src/runtime_impl.rs");
    println!("cargo:rerun-if-changed=src/production_orchestrator.rs");

    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let mut source =
        fs::read_to_string("src/runtime_impl.rs").expect("read canonical runtime implementation");

    for (declaration, file) in [
        ("pub mod commands;", "commands.rs"),
        ("mod error;", "error.rs"),
        ("pub mod lifecycle;", "lifecycle.rs"),
        ("pub mod prompt;", "prompt.rs"),
        ("pub mod skill_dependencies;", "skill_dependencies.rs"),
        (
            "pub mod skill_dependency_locks;",
            "skill_dependency_locks.rs",
        ),
        ("mod support;", "support.rs"),
        ("mod tests;", "tests.rs"),
    ] {
        bind_module(&mut source, &manifest, declaration, file);
    }
    source = source.replace(", StepOutcome, TurnUsage,", ", StepOutcome,");

    replace_once(
        &mut source,
        "    let engine = AgentEngine::new(provider, config);\n    let selected_skill = state.pending_skill.clone();\n    let skill_context = selected_skill.as_ref().map(SelectedSkill::prompt_context);\n    let content = message_blocks(&draft)?;\n",
        "    let engine = AgentEngine::new(provider, config);\n    let selected_skill = state.pending_skill.clone();\n    let execution_plan = crate::production_orchestrator::plan(&draft)\n        .map_err(RuntimeError::agent)?;\n    for event in crate::production_orchestrator::events(&execution_plan) {\n        let _ = events.send(event);\n    }\n    let orchestration_context = crate::production_orchestrator::runtime_context(&execution_plan);\n    let skill_context = selected_skill\n        .as_ref()\n        .map(SelectedSkill::prompt_context)\n        .map(|skill| format!(\"{skill}\\n\\n{orchestration_context}\"))\n        .unwrap_or(orchestration_context);\n    let content = message_blocks(&draft)?;\n",
    );

    replace_once(
        &mut source,
        "step_with_observer_and_context(&mut session, skill_context.as_deref(), |update| {",
        "step_with_observer_and_context(&mut session, Some(skill_context.as_str()), |update| {",
    );

    replace_once(
        &mut source,
        "    state.session = Some(session);\n    result\n}\n\nfn append_followups",
        "    let verified = matches!(&result, Ok(RuntimeEvent::Completed { .. }));\n    let failed = result.is_err();\n    if let Err(error) = crate::production_orchestrator::persist_outcome(\n        &state.repo,\n        &draft,\n        &execution_plan,\n        verified,\n        failed,\n    ) {\n        let _ = events.send(RuntimeEvent::Notice {\n            title: \"Runtime learning record unavailable\".to_owned(),\n            details: vec![error.to_string()],\n        });\n    }\n    state.session = Some(session);\n    result\n}\n\nfn append_followups",
    );

    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("runtime_generated.rs");
    fs::write(output, source).expect("write generated production runtime");
}
