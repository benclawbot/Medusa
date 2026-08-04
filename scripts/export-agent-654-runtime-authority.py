#!/usr/bin/env python3
from pathlib import Path


def replace_once(path: str, old: str, new: str, label: str) -> None:
    target = Path(path)
    text = target.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one match in {path}, found {count}")
    target.write_text(text.replace(old, new, 1), encoding="utf-8")


LIB = "crates/medusa-runtime/src/lib.rs"
FACADE = "crates/medusa-runtime/src/mutation_transaction.rs"
LEGACY_DOC = "docs/architecture/LEGACY-DELETION.md"

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
    "remove immutable mutation packet from generic session",
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
    "defer mutation review ledger to dedicated transport",
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
                    let review_provider = ConfiguredProvider::manager_from_config(
                        &state.config,
                        state.session_api_key.clone(),
                    )
                    .map_err(RuntimeError::agent)?;
                    match crate::mutation_transaction::complete_after_parent_review(
                        &evidence.transaction_path,
                        &state.repo,
                        &review_provider,
                        &state.config,
                        cancel.as_ref(),
                        events,
                    ) {
''',
    "pass active runtime provider authority",
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

Path(FACADE).write_text(
    '''//! Dedicated parent-review transaction facade.

use std::{
    path::Path,
    sync::{atomic::AtomicBool, mpsc::Sender},
};

use medusa_config::Config;
use medusa_provider::ModelProvider;

use crate::RuntimeEvent;

#[allow(dead_code)]
#[path = "mutation_transaction_legacy.rs"]
mod legacy;

pub use legacy::*;

pub fn complete_after_parent_review<P: ModelProvider>(
    path: &Path,
    repo: &Path,
    provider: &P,
    config: &Config,
    cancel: &AtomicBool,
    events: &Sender<RuntimeEvent>,
) -> Result<TransactionCompletion, String> {
    crate::parent_reviewer::complete(path, repo, provider, config, cancel, events)
}
''',
    encoding="utf-8",
)

legacy = Path(LEGACY_DOC).read_text(encoding="utf-8")
needle = "- Remaining #632 work: replace the generic `AgentEngine` review transport with a dedicated no-tools reviewer while preserving durable session evidence.\n"
replacement = """- Production mutation authorization now uses a dedicated direct `ModelProvider` request that advertises zero tools and receives only the immutable review packet.\n- The reviewer inherits the active runtime model configuration, session API key, fallback routes, and cancellation signal rather than reconstructing defaults.\n- The review request, provider outcome, typed decision, rationale, usage, and response fingerprint are persisted in a versioned `parent-review-session.json` journal and resumed idempotently after interruption.\n- Tool-use responses, malformed envelopes, journal substitution, and corrupt or incomplete terminal evidence fail closed before verification or integration.\n- The generic conversational `AgentEngine` no longer receives the mutation patch and cannot authorize or reject integration.\n- Remaining #632 deletion target: remove the quarantined compatibility parser after recovery fixtures migrate to the dedicated journal path.\n"""
if needle not in legacy:
    raise SystemExit("legacy deletion receipt marker missing")
Path(LEGACY_DOC).write_text(legacy.replace(needle, replacement, 1), encoding="utf-8")

print("exported #654 runtime authority patch")
