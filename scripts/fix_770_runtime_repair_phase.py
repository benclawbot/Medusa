from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, found {count}: {old[:120]!r}")
    file.write_text(text.replace(old, new, 1), encoding="utf-8")


replace_once(
    "crates/medusa-agent/src/engine.rs",
    "    fn step_with_observer_and_context_and_turn_instruction_for_phase<F>(\n",
    "    pub fn step_with_observer_and_context_and_turn_instruction_for_phase<F>(\n",
)

replace_once(
    "crates/medusa-runtime/src/lib.rs",
    "use medusa_provider::{ConfiguredProvider, Message, MessageBlock, ModelProvider, Role};\n",
    "use medusa_provider::{\n    ConfiguredProvider, Message, MessageBlock, ModelProvider, ProviderExecutionPhase, Role,\n};\n",
)

replace_once(
    "crates/medusa-runtime/src/lib.rs",
    '''            let outcome = loop {\n                let attempt_signature = format!("{provider_signature}:attempt:{next_attempt}");\n''',
    '''            let mut provider_phase = match state.config.agent.mode {\n                Mode::ReadOnly => ProviderExecutionPhase::Planning,\n                Mode::Review => ProviderExecutionPhase::HighRiskReview,\n                Mode::Yolo => ProviderExecutionPhase::Implementation,\n            };\n            let outcome = loop {\n                let attempt_signature = format!("{provider_signature}:attempt:{next_attempt}");\n''',
)

replace_once(
    "crates/medusa-runtime/src/lib.rs",
    '''                match engine.step_with_observer_and_context_and_turn_instruction(\n                    &mut session,\n                    Some(skill_context.as_str()),\n                    None,\n                    |update| {\n                        forward_update(update, events, &mut updates);\n                    },\n                ) {\n''',
    '''                match engine.step_with_observer_and_context_and_turn_instruction_for_phase(\n                    &mut session,\n                    Some(skill_context.as_str()),\n                    None,\n                    provider_phase,\n                    |update| {\n                        forward_update(update, events, &mut updates);\n                    },\n                ) {\n''',
)

replace_once(
    "crates/medusa-runtime/src/lib.rs",
    '''                        if decision.action == medusa_tool_control::RetryAction::Retry {\n                            continue;\n                        }\n''',
    '''                        if decision.action == medusa_tool_control::RetryAction::Retry {\n                            provider_phase = ProviderExecutionPhase::Repair;\n                            continue;\n                        }\n''',
)
