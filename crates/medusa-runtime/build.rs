use std::{env, error::Error, fs, io, path::PathBuf};

fn replace_once(source: &mut String, needle: &str, replacement: &str) -> io::Result<()> {
    let position = source.find(needle).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("runtime generation anchor not found: {needle}"),
        )
    })?;
    source.replace_range(position..position + needle.len(), replacement);
    Ok(())
}

fn bind_module(
    source: &mut String,
    manifest: &str,
    declaration: &str,
    file: &str,
) -> io::Result<()> {
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
    )
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=src/runtime_impl.rs");
    println!("cargo:rerun-if-changed=src/production_orchestrator.rs");
    println!("cargo:rerun-if-changed=src/recovery_tui.inc");

    let manifest = env::var("CARGO_MANIFEST_DIR")?;
    let mut source = fs::read_to_string("src/runtime_impl.rs")?.replace("\r\n", "\n");

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
        bind_module(&mut source, &manifest, declaration, file)?;
    }

    let recovery_source = fs::read_to_string("src/recovery_tui.inc")?.replace("\r\n", "\n");
    source.push_str("\nmod recovery_tui {\n");
    source.push_str(&recovery_source);
    source.push_str("\n}\n");

    replace_once(
        &mut source,
        "    ConfigureModel(ModelConfiguration),\n    Shutdown,",
        "    ConfigureModel(ModelConfiguration),\n    Recovery {\n        view: Box<medusa_recovery_coordinator::RecoveryView>,\n        request: medusa_recovery_coordinator::RecoveryActionRequest,\n        preflight: medusa_recovery_coordinator::RecoveryPreflightEvidence,\n    },\n    Shutdown,",
    )?;

    replace_once(
        &mut source,
        "pub enum RuntimeEvent {\n    Started,",
        "pub enum RuntimeEvent {\n    RecoveryAvailable(medusa_recovery_coordinator::RecoveryView),\n    RecoveryCompleted(medusa_recovery_coordinator::RecoveryExecutionReceipt),\n    Started,",
    )?;

    replace_once(
        &mut source,
        "    pub fn cancel(&self) -> bool {",
        "    pub fn execute_recovery(\n        &self,\n        view: medusa_recovery_coordinator::RecoveryView,\n        request: medusa_recovery_coordinator::RecoveryActionRequest,\n        preflight: medusa_recovery_coordinator::RecoveryPreflightEvidence,\n    ) -> Result<(), RuntimeError> {\n        if lock_submission(&self.submission).busy {\n            return Err(RuntimeError::Busy);\n        }\n        self.commands\n            .send(RuntimeCommand::Recovery { view: Box::new(view), request, preflight })\n            .map_err(|_| RuntimeError::WorkerStopped)\n    }\n\n    pub fn cancel(&self) -> bool {",
    )?;

    replace_once(
        &mut source,
        "            RuntimeCommand::Shutdown => break,",
        "            RuntimeCommand::Recovery { view, request, preflight } => {\n                match recovery_tui::execute_view_action(&state.repo, &view, &request, preflight) {\n                    Ok(receipt) => {\n                        let _ = events.send(RuntimeEvent::RecoveryCompleted(receipt));\n                    }\n                    Err(error) => {\n                        let _ = events.send(RuntimeEvent::Notice {\n                            title: \"Recovery action failed closed\".to_owned(),\n                            details: vec![error],\n                        });\n                    }\n                }\n            }\n            RuntimeCommand::Shutdown => break,",
    )?;

    replace_once(
        &mut source,
        "    let _ = events.send(state.settings_event());\n    let capability_repo =",
        "    let _ = events.send(state.settings_event());\n    for recovery_event in recovery::startup_events(&state.repo) {\n        let _ = events.send(recovery_event);\n    }\n    let capability_repo =",
    )?;

    replace_once(
        &mut source,
        "    let engine = AgentEngine::new(provider, config);\n    let selected_skill = state.pending_skill.clone();\n    let skill_context = selected_skill.as_ref().map(SelectedSkill::prompt_context);\n    let content = message_blocks(&draft)?;\n",
        "    let engine = AgentEngine::new(provider, config);\n    let selected_skill = state.pending_skill.clone();\n    let execution_plan = crate::production_orchestrator::plan(&draft)\n        .map_err(RuntimeError::agent)?;\n    for event in crate::production_orchestrator::events(&execution_plan) {\n        let _ = events.send(event);\n    }\n    let orchestration_context = crate::production_orchestrator::runtime_context(&execution_plan);\n    let skill_context = selected_skill\n        .as_ref()\n        .map(SelectedSkill::prompt_context)\n        .map(|skill| format!(\"{skill}\\n\\n{orchestration_context}\"))\n        .unwrap_or(orchestration_context);\n    let content = message_blocks(&draft)?;\n",
    )?;

    replace_once(
        &mut source,
        "        SlashCommand::Skill { selector, task } => {",
        "        SlashCommand::Skill { selector, task } if selector.eq_ignore_ascii_case(\"recovery\") => {\n            match recovery_tui::execute_command(&state.repo, task.as_deref()) {\n                Ok(Some(receipt)) => { let _ = events.send(RuntimeEvent::RecoveryCompleted(receipt)); }\n                Ok(None) => {\n                    for event in recovery::startup_events(&state.repo) { let _ = events.send(event); }\n                    let _ = events.send(RuntimeEvent::Notice {\n                        title: \"Recovery commands\".to_owned(),\n                        details: vec![\"/recovery inspect|resume|verify|abandon or /recovery restore <checkpoint> [--confirm]\".to_owned()],\n                    });\n                }\n                Err(error) => { let _ = events.send(RuntimeEvent::Notice { title: \"Recovery action failed closed\".to_owned(), details: vec![error] }); }\n            }\n        }\n        SlashCommand::Skill { selector, task } => {",
    )?;

    replace_once(
        &mut source,
        "step_with_observer_and_context(&mut session, skill_context.as_deref(), |update| {",
        "step_with_observer_and_context(&mut session, Some(skill_context.as_str()), |update| {",
    )?;

    replace_once(
        &mut source,
        "            let outcome = engine\n                .step_with_observer_and_context(&mut session, Some(skill_context.as_str()), |update| {\n                    forward_update(update, events, &mut updates);\n                })\n                .map_err(RuntimeError::agent)?;",
        "            let provider_activity_id = format!(\"provider-request-{}\", session.turn.saturating_add(1));\n            let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {\n                id: Some(provider_activity_id.clone()),\n                kind: RuntimeActivityKind::Progress,\n                title: format!(\n                    \"Waiting for {} / {} response\",\n                    state.config.model.provider, state.config.model.name\n                ),\n                details: vec![\n                    \"The provider request has a 120-second per-attempt timeout.\".to_owned(),\n                    \"A bounded route retry or failover may follow; press Esc to cancel.\".to_owned(),\n                ],\n            }));\n            let provider_started_at = std::time::Instant::now();\n            let outcome = engine\n                .step_with_observer_and_context(&mut session, Some(skill_context.as_str()), |update| {\n                    forward_update(update, events, &mut updates);\n                })\n                .map_err(RuntimeError::agent)?;\n            let provider_duration_ms = u64::try_from(provider_started_at.elapsed().as_millis())\n                .unwrap_or(u64::MAX);\n            let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {\n                id: Some(provider_activity_id),\n                kind: RuntimeActivityKind::Done,\n                title: \"Model response received\".to_owned(),\n                details: vec![format!(\"completed in {provider_duration_ms} ms\")],\n            }));",
    )?;

    replace_once(
        &mut source,
        "    state.session = Some(session);\n    result\n}\n\nfn append_followups",
        "    let verified = matches!(&result, Ok(RuntimeEvent::Completed { .. }));\n    let failed = result.is_err();\n    if let Err(error) = crate::production_orchestrator::persist_outcome(\n        &state.repo,\n        &draft,\n        &execution_plan,\n        verified,\n        failed,\n    ) {\n        let _ = events.send(RuntimeEvent::Notice {\n            title: \"Runtime learning record unavailable\".to_owned(),\n            details: vec![error.to_string()],\n        });\n    }\n    state.session = Some(session);\n    result\n}\n\nfn append_followups",
    )?;

    let output = PathBuf::from(env::var("OUT_DIR")?).join("runtime_generated.rs");
    fs::write(output, source)?;
    Ok(())
}
