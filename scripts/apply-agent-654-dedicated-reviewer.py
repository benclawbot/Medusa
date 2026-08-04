#!/usr/bin/env python3
from pathlib import Path


def replace_once(path: str, old: str, new: str, label: str) -> None:
    target = Path(path)
    text = target.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected exactly one match in {path}, found {count}")
    target.write_text(text.replace(old, new, 1), encoding="utf-8")


LIB = "crates/medusa-runtime/src/lib.rs"
REVIEWER = "crates/medusa-runtime/src/parent_reviewer.rs"
LEGACY = "docs/architecture/LEGACY-DELETION.md"

replace_once(
    LIB,
    "mod mutating_worker_coordinator;\nmod mutation_transaction;\npub mod openai_realtime;",
    "mod mutating_worker_coordinator;\nmod mutation_transaction;\nmod parent_reviewer;\npub mod openai_realtime;",
    "declare dedicated reviewer module",
)

replace_once(
    LIB,
    '''    if implementation_evidence.is_none() {
    if let Some(evidence) = coordinator_evidence.as_ref() {
        task_context.push(evidence.parent_context());
    }
}
if let Some(evidence) = implementation_evidence.as_ref() {
    task_context.push(evidence.parent_context());
}
''',
    '''    if implementation_evidence.is_none() {
        if let Some(evidence) = coordinator_evidence.as_ref() {
            task_context.push(evidence.parent_context());
        }
    } else {
        task_context.push(
            "An isolated implementer has prepared an immutable mutation transaction. A separate dedicated no-tools reviewer is the sole authority for that patch. This conversational turn must not inspect, accept, reject, or claim integration of the prepared mutation; provide only a concise user-facing status and leave authorization to the durable reviewer transport."
                .to_owned(),
        );
    }
''',
    "remove mutation packet from generic session",
)

replace_once(
    LIB,
    '''    if coordinated {
        if let Some(ledger) = execution_ledger.as_mut() {
            crate::production_orchestrator::begin_kinds(
                ledger,
                &execution_plan,
                &[medusa_multi_agent_scheduler::TaskKind::Review],
                "parent-review",
            )
            .map_err(RuntimeError::agent)?;
            let _ = events.send(RuntimeEvent::Plan(
                crate::production_orchestrator::projection(ledger),
            ));
        }
    }
''',
    '''    if coordinated && implementation_evidence.is_none() {
        if let Some(ledger) = execution_ledger.as_mut() {
            crate::production_orchestrator::begin_kinds(
                ledger,
                &execution_plan,
                &[medusa_multi_agent_scheduler::TaskKind::Review],
                "parent-review",
            )
            .map_err(RuntimeError::agent)?;
            let _ = events.send(RuntimeEvent::Plan(
                crate::production_orchestrator::projection(ledger),
            ));
        }
    }
''',
    "defer mutating review ledger to dedicated transport",
)

replace_once(
    LIB,
    '''                let provider_started_at = std::time::Instant::now();
                let turn_instruction = implementation_evidence
                    .as_ref()
                    .map(|_| crate::mutation_transaction::PARENT_REVIEW_TURN_INSTRUCTION);
                match engine.step_with_observer_and_context_and_turn_instruction(
                    &mut session,
                    Some(skill_context.as_str()),
                    turn_instruction,
''',
    '''                let provider_started_at = std::time::Instant::now();
                match engine.step_with_observer_and_context_and_turn_instruction(
                    &mut session,
                    Some(skill_context.as_str()),
                    None,
''',
    "remove generic parent-review turn instruction",
)

replace_once(
    LIB,
    '''                Ok(RuntimeEvent::Completed { .. } | RuntimeEvent::TurnFinished) => {
                    match crate::mutation_transaction::complete_after_parent_review(
                        &evidence.transaction_path,
                        &state.repo,
                        &session,
                        events,
                    ) {
''',
    '''                Ok(RuntimeEvent::Completed { .. } | RuntimeEvent::TurnFinished) => {
                    if let Some(ledger) = execution_ledger.as_mut() {
                        crate::production_orchestrator::begin_kinds(
                            ledger,
                            &execution_plan,
                            &[medusa_multi_agent_scheduler::TaskKind::Review],
                            "dedicated-parent-review",
                        )
                        .map_err(RuntimeError::agent)?;
                        let _ = events.send(RuntimeEvent::Plan(
                            crate::production_orchestrator::projection(ledger),
                        ));
                    }
                    let review_result = (|| {
                        let review_provider = ConfiguredProvider::manager_from_config(
                            &config,
                            state.session_api_key.clone(),
                        )
                        .map_err(|error| error.to_string())?;
                        crate::parent_reviewer::complete(
                            &evidence.transaction_path,
                            &state.repo,
                            &review_provider,
                            &config,
                            cancel.as_ref(),
                            events,
                        )
                    })();
                    match review_result {
''',
    "route authorization through dedicated reviewer",
)

replace_once(
    LIB,
    '''/// The shipped coordinated path is `RuntimeController -> run_prompt ->
/// multi_agent_coordinator::run_preflight -> read-only AgentEngine teammates -> parent AgentEngine`.
''',
    '''/// The shipped coordinated path is `RuntimeController -> run_prompt ->
/// multi_agent_coordinator::run_preflight -> isolated implementer -> dedicated no-tools parent reviewer`.
''',
    "update production path documentation",
)

replace_once(
    REVIEWER,
    "    use medusa_provider::{ModelResponse, ToolDefinition};\n",
    "    use medusa_provider::ModelResponse;\n",
    "remove unused test import",
)

replace_once(
    REVIEWER,
    "    journal.fingerprint = hash(journal);\n",
    "    journal.fingerprint = hash(&*journal);\n",
    "hash immutable journal view",
)

replace_once(
    REVIEWER,
    '''                || journal.rationale.as_deref().is_none_or(str::is_empty)
                || journal.response_fingerprint.as_deref().is_none_or(str::is_empty)
''',
    '''                || is_blank(journal.rationale.as_deref())
                || is_blank(journal.response_fingerprint.as_deref())
''',
    "validate completed journal text",
)

replace_once(
    REVIEWER,
    '''            if journal.error.as_deref().is_none_or(str::is_empty) || journal.decision.is_some() {
''',
    '''            if is_blank(journal.error.as_deref()) || journal.decision.is_some() {
''',
    "validate failed journal error",
)

replace_once(
    REVIEWER,
    '''    fs::write(&temporary, encoded).map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

fn validate_journal''',
    '''    fs::write(&temporary, encoded).map_err(|error| error.to_string())?;
    if path.exists() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

fn validate_journal''',
    "make journal replacement cross-platform",
)

replace_once(
    REVIEWER,
    '''fn hash(value: &impl Serialize) -> String {
''',
    '''fn is_blank(value: Option<&str>) -> bool {
    value.is_none_or(|value| value.trim().is_empty())
}

fn hash(value: &impl Serialize) -> String {
''',
    "add blank text helper",
)

replace_once(
    REVIEWER,
    '''
    #[test]
    fn nonempty_tool_schema_is_never_needed_by_the_reviewer() {
        let definition = ToolDefinition {
            name: "unused".to_owned(),
            description: "unused".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        assert!(!definition.name.is_empty());
        let request = ModelRequest {
            system: REVIEW_SYSTEM_PROMPT.to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_tokens: MAX_REVIEW_OUTPUT_TOKENS,
            temperature_milli: 0,
        };
        assert!(request.tools.is_empty());
    }
''',
    "\n",
    "remove redundant schema test",
)

replace_once(
    LEGACY,
    '''- Remaining #632 work: replace the generic `AgentEngine` review transport with a dedicated no-tools reviewer while preserving durable session evidence.
''',
    '''- Production mutation authorization now uses a dedicated direct `ModelProvider` request that advertises zero tools and receives only the immutable review packet.
- The review request, provider outcome, typed decision, rationale, usage, and response fingerprint are persisted in a versioned `parent-review-session.json` journal and resumed idempotently after interruption.
- Tool-use responses, malformed envelopes, journal substitution, and corrupt or incomplete terminal evidence fail closed before verification or integration.
- The generic conversational `AgentEngine` no longer receives the mutation patch and cannot authorize or reject integration.
- Remaining #632 deletion target: remove the now-unreferenced compatibility parser that reads parent-review decisions from an `AgentSession` after repository-wide callers and recovery fixtures confirm the dedicated journal path.
''',
    "record dedicated reviewer deletion receipt",
)

print("agent #654 dedicated parent-review patch applied")
