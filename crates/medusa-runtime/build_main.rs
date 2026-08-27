fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=src/runtime_impl.rs");
    println!("cargo:rerun-if-changed=src/production_orchestrator.rs");
    println!("cargo:rerun-if-changed=src/recovery_tui.inc");
    println!("cargo:rerun-if-changed=src/commands.rs");
    println!("cargo:rerun-if-changed=src/review.inc");
    println!("cargo:rerun-if-changed=src/review_tests.inc");
    println!("cargo:rerun-if-changed=src/attachment.rs");
    println!("cargo:rerun-if-changed=src/learning_retrieval.rs");
    println!("cargo:rerun-if-changed=src/openai_realtime.rs");
    println!("cargo:rerun-if-changed=src/voice.rs");
    println!("cargo:rerun-if-changed=src/voice_agent_bridge.rs");

    let manifest = env::var("CARGO_MANIFEST_DIR")?;
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let commands = write_generated_commands(&out_dir)?;
    let review_tests = write_generated_review_tests(&out_dir)?;
    let review = write_generated_review(&out_dir, &review_tests)?;
    let mut source = fs::read_to_string("src/runtime_impl.rs")?.replace("\r\n", "\n");

    replace_once(
        &mut source,
        "    commands::{Effort, ModelCommand, ModelConfiguration, SlashCommand},",
        "    commands::{Effort, ModelCommand, ModelConfiguration, ReviewCommand, SlashCommand},",
    )?;
    replace_once(
        &mut source,
        "pub mod commands;",
        &format!(
            "#[path = \"{}\"]\npub mod commands;",
            commands.display().to_string().replace('\\', "/")
        ),
    )?;
    replace_once(
        &mut source,
        "pub mod prompt;\npub mod skill_dependencies;",
        &format!(
            "pub mod attachment;\nmod learning_retrieval;\npub mod openai_realtime;\npub mod prompt;\n#[path = \"{}\"]\npub mod review;\npub mod voice;\npub mod voice_agent_bridge;\npub mod skill_dependencies;",
            review.display().to_string().replace('\\', "/")
        ),
    )?;

    for (declaration, file) in [
        ("mod error;", "error.rs"),
        ("pub mod attachment;", "attachment.rs"),
        ("mod learning_retrieval;", "learning_retrieval.rs"),
        ("pub mod lifecycle;", "lifecycle.rs"),
        ("pub mod openai_realtime;", "openai_realtime.rs"),
        ("pub mod prompt;", "prompt.rs"),
        ("pub mod voice;", "voice.rs"),
        ("pub mod voice_agent_bridge;", "voice_agent_bridge.rs"),
        ("pub mod skill_dependencies;", "skill_dependencies.rs"),
        ("pub mod skill_dependency_locks;", "skill_dependency_locks.rs"),
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
        "            RuntimeCommand::Recovery { view, request, preflight } => {\n                match recovery_tui::execute_view_action(&state.repo, &view, &request, preflight) {\n                    Ok(receipt) => { let _ = events.send(RuntimeEvent::RecoveryCompleted(receipt)); }\n                    Err(error) => { let _ = events.send(RuntimeEvent::Notice { title: \"Recovery action failed closed\".to_owned(), details: vec![error] }); }\n                }\n            }\n            RuntimeCommand::Shutdown => break,",
    )?;
    replace_once(
        &mut source,
        "    let _ = events.send(state.settings_event());\n    let capability_repo =",
        "    let _ = events.send(state.settings_event());\n    for recovery_event in recovery::startup_events(&state.repo) { let _ = events.send(recovery_event); }\n    let capability_repo =",
    )?;

    let review_arm = runtime_review_arm();
    replace_once(
        &mut source,
        "        SlashCommand::Help => {",
        &format!("{review_arm}        SlashCommand::Help => {{"),
    )?;
    replace_once(
        &mut source,
        "        SlashCommand::Skill { selector, task } => {",
        "        SlashCommand::Skill { selector, task } if selector.eq_ignore_ascii_case(\"recovery\") => {\n            match recovery_tui::execute_command(&state.repo, task.as_deref()) {\n                Ok(Some(receipt)) => { let _ = events.send(RuntimeEvent::RecoveryCompleted(receipt)); }\n                Ok(None) => {\n                    for event in recovery::startup_events(&state.repo) { let _ = events.send(event); }\n                    let _ = events.send(RuntimeEvent::Notice {\n                        title: \"Recovery commands\".to_owned(),\n                        details: vec![\"/recovery inspect|resume|verify|abandon or /recovery restore <checkpoint> [--confirm]\".to_owned()],\n                    });\n                }\n                Err(error) => { let _ = events.send(RuntimeEvent::Notice { title: \"Recovery action failed closed\".to_owned(), details: vec![error] }); }\n            }\n        }\n        SlashCommand::Skill { selector, task } => {",
    )?;

    replace_once(
        &mut source,
        "    let engine = AgentEngine::new_with_cancellation(provider, config, Arc::clone(cancel));\n    let selected_skill = state.pending_skill.clone();\n    let skill_context = selected_skill.as_ref().map(SelectedSkill::prompt_context);\n    let content = message_blocks(&draft)?;\n",
        "    let resuming_pending_question = state\n        .session\n        .as_ref()\n        .is_some_and(|session| session.pending_question.is_some());\n    if !resuming_pending_question {\n        crate::review::capture_review_baseline(&state.repo)\n            .map_err(|error| RuntimeError::agent(error.to_string()))?;\n    }\n    let engine = AgentEngine::new_with_cancellation(provider, config, Arc::clone(cancel));\n    let selected_skill = state.pending_skill.clone();\n    let execution_plan = crate::production_orchestrator::plan(&draft).map_err(RuntimeError::agent)?;\n    for event in crate::production_orchestrator::events(&execution_plan) { let _ = events.send(event); }\n    let orchestration_context = crate::production_orchestrator::runtime_context(&execution_plan);\n    let learning_context = crate::learning_retrieval::select(\n        &state.repo,\n        &draft,\n        state.session.as_ref().map(|session| session.id.as_str()),\n        events,\n    );\n    let mut task_context = vec![orchestration_context];\n    if let Some(learning) = learning_context.prompt_context { task_context.push(learning); }\n    if let Some(skill) = selected_skill.as_ref().map(SelectedSkill::prompt_context) { task_context.push(skill); }\n    let skill_context = task_context.join(\"\\n\\n\");\n    let content = message_blocks(&draft)?;\n",
    )?;
    replace_once(
        &mut source,
        "                skill_context.as_deref(),\n                |update| {",
        "                Some(skill_context.as_str()),\n                |update| {",
    )?;
    replace_once(
        &mut source,
        "            let outcome = match engine.step_with_observer_and_context(\n                &mut session,\n                Some(skill_context.as_str()),\n                |update| {\n                    forward_update(update, events, &mut updates);\n                },\n            ) {\n                Ok(outcome) => outcome,\n                Err(_) if cancel_requested(cancel, submission) => {\n                    return Ok(RuntimeEvent::Cancelled);\n                }\n                Err(error) => return Err(RuntimeError::agent(error)),\n            };",
        "            let provider_activity_id = format!(\"provider-request-{}\", session.turn.saturating_add(1));\n            let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {\n                id: Some(provider_activity_id.clone()),\n                kind: RuntimeActivityKind::Progress,\n                title: format!(\n                    \"Waiting for {} / {} response\",\n                    state.config.model.provider, state.config.model.name\n                ),\n                details: vec![\n                    \"The provider request has a 120-second per-attempt timeout.\".to_owned(),\n                    \"A bounded route retry or failover may follow; press Esc to cancel.\".to_owned(),\n                ],\n            }));\n            let provider_started_at = std::time::Instant::now();\n            let outcome = engine\n                .step_with_observer_and_context(&mut session, Some(skill_context.as_str()), |update| {\n                    forward_update(update, events, &mut updates);\n                })\n                .map_err(RuntimeError::agent)?;\n            let provider_duration_ms = u64::try_from(provider_started_at.elapsed().as_millis())\n                .unwrap_or(u64::MAX);\n            let _ = events.send(RuntimeEvent::Activity(RuntimeActivity {\n                id: Some(provider_activity_id),\n                kind: RuntimeActivityKind::Done,\n                title: \"Model response received\".to_owned(),\n                details: vec![format!(\"completed in {provider_duration_ms} ms\")],\n            }));",
    )?;
    replace_once(
        &mut source,
        "    state.session = Some(session);\n    result\n}\n\nfn append_followups",
        "    let verified = matches!(&result, Ok(RuntimeEvent::Completed { .. }));\n    let failed = result.is_err();\n    if let Err(error) = crate::production_orchestrator::persist_outcome(&state.repo, &draft, &execution_plan, verified, failed) {\n        let _ = events.send(RuntimeEvent::Notice { title: \"Runtime learning record unavailable\".to_owned(), details: vec![error.to_string()] });\n    }\n    state.session = Some(session);\n    result\n}\n\nfn append_followups",
    )?;

    source = source.replace("cancel: &AtomicBool", "cancel: &Arc<AtomicBool>");
    fs::write(out_dir.join("runtime_generated.rs"), source)?;
    Ok(())
}
