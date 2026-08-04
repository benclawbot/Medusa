#!/usr/bin/env python3
from pathlib import Path


def replace_once(path: str, old: str, new: str, label: str) -> None:
    target = Path(path)
    text = target.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one match in {path}, found {count}")
    target.write_text(text.replace(old, new, 1), encoding="utf-8")


RUNTIME = "crates/medusa-runtime/src/lib.rs"
GUARD = "scripts/check-mutation-lifecycle.py"

replace_once(
    RUNTIME,
    "use medusa_provider::{ConfiguredProvider, ModelProvider};\n",
    "use medusa_provider::{ConfiguredProvider, Message, MessageBlock, ModelProvider, Role};\n",
    "import deterministic completion message types",
)

replace_once(
    RUNTIME,
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
    '''    if implementation_evidence.is_none()
        && let Some(evidence) = coordinator_evidence.as_ref()
    {
        task_context.push(evidence.parent_context());
    }
''',
    "remove redundant conversational mutation status prompt",
)

replace_once(
    RUNTIME,
    '''    let result = (|| {
        loop {
''',
    '''    let result = if implementation_evidence.is_some() {
        Ok(RuntimeEvent::TurnFinished)
    } else {
        (|| {
            loop {
''',
    "bypass generic model turn for prepared mutations",
)

replace_once(
    RUNTIME,
    '''            }
        }
    })();
    let waiting_for_user = matches!(&result, Ok(RuntimeEvent::Question(_)));
''',
    '''            }
        })()
    };
    let waiting_for_user = matches!(&result, Ok(RuntimeEvent::Question(_)));
''',
    "close deterministic mutation branch",
)

replace_once(
    RUNTIME,
    '''                            medusa_agent::record_session_event(
                                &mut session,
                                Actor::Coordinator,
                                EventPayload::IntegrationReceiptRecorded {
                                    receipt: serde_json::to_value(&receipt)
                                        .map_err(RuntimeError::agent)?,
                                },
                            )
                            .map_err(RuntimeError::agent)?;
''',
    '''                            medusa_agent::record_session_event(
                                &mut session,
                                Actor::Coordinator,
                                EventPayload::IntegrationReceiptRecorded {
                                    receipt: serde_json::to_value(&receipt)
                                        .map_err(RuntimeError::agent)?,
                                },
                            )
                            .map_err(RuntimeError::agent)?;
                            let completion_text = mutation_completion_text(
                                &evidence.summary,
                                &receipt.commit,
                                &receipt.changed_paths,
                            );
                            let message = Message {
                                role: Role::Assistant,
                                content: vec![MessageBlock::Text {
                                    text: completion_text.clone(),
                                }],
                            };
                            session.messages.push(message.clone());
                            medusa_agent::record_session_event(
                                &mut session,
                                Actor::Coordinator,
                                EventPayload::AssistantMessageRecorded {
                                    message: serde_json::to_value(&message)
                                        .map_err(RuntimeError::agent)?,
                                },
                            )
                            .map_err(RuntimeError::agent)?;
                            session.completed = true;
                            let _ = events.send(RuntimeEvent::AssistantText(completion_text));
                            result = Ok(RuntimeEvent::Completed {
                                session_id: session.id.to_string(),
                            });
''',
    "record deterministic post-review completion",
)

replace_once(
    RUNTIME,
    '''fn append_followups<P: ModelProvider>(
''',
    '''fn mutation_completion_text(summary: &str, commit: &str, changed_paths: &[String]) -> String {
    let visible_summary = summary
        .rsplit_once("</think>")
        .map_or(summary, |(_, visible)| visible)
        .trim();
    let status = format!(
        "Verified and integrated commit `{commit}`. Changed paths: {}.",
        changed_paths.join(", ")
    );
    if visible_summary.is_empty() {
        status
    } else {
        format!("{visible_summary}\\n\\n{status}")
    }
}

#[cfg(test)]
mod mutation_completion_tests {
    use super::mutation_completion_text;

    #[test]
    fn hides_reasoning_and_preserves_visible_implementer_result() {
        let text = mutation_completion_text(
            "<think>private implementation reasoning</think>\\n\\nMEDUSA_TUI_MINIMAX_OK",
            "abc123",
            &["src/lib.rs".to_owned()],
        );
        assert!(!text.contains("private implementation reasoning"));
        assert!(text.starts_with("MEDUSA_TUI_MINIMAX_OK"));
        assert!(text.contains("Verified and integrated commit `abc123`"));
        assert!(text.contains("src/lib.rs"));
    }

    #[test]
    fn falls_back_to_verified_status_when_summary_has_no_visible_text() {
        let text = mutation_completion_text(
            "<think>private implementation reasoning</think>",
            "abc123",
            &["src/lib.rs".to_owned()],
        );
        assert_eq!(
            text,
            "Verified and integrated commit `abc123`. Changed paths: src/lib.rs."
        );
    }
}

fn append_followups<P: ModelProvider>(
''',
    "add deterministic completion formatter",
)

replace_once(
    GUARD,
    '''status_turn = runtime.find("engine.step_with_observer_and_context_and_turn_instruction")
provider = runtime.find("ConfiguredProvider::manager_from_config", status_turn)
completion = runtime.find("complete_after_parent_review", provider)
if status_turn < 0 or provider < status_turn or completion < provider:
    errors.append("dedicated transaction review is not ordered after the conversational status turn")
''',
    '''provider = runtime.find("ConfiguredProvider::manager_from_config")
completion = runtime.find("complete_after_parent_review", provider)
if provider < 0 or completion < provider:
    errors.append("dedicated transaction review is not connected to runtime completion")
if "let result = if implementation_evidence.is_some()" not in runtime:
    errors.append("prepared mutations still enter the generic conversational model loop")
if "mutation_completion_text(" not in runtime or "EventPayload::AssistantMessageRecorded" not in runtime:
    errors.append("accepted mutations lack a deterministic durable completion response")
''',
    "update lifecycle guard for deterministic completion",
)

print("agent #654 deterministic completion patch applied")
