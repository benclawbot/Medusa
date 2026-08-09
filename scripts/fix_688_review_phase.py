from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, found {count}: {old[:100]!r}")
    file.write_text(text.replace(old, new, 1), encoding="utf-8")


replace_once(
    "crates/medusa-agent/src/engine_support.rs",
    "use medusa_provider::{ImageSource, Message, MessageBlock, ProviderCapabilities, Role};\n",
    "use medusa_provider::{\n    ImageSource, Message, MessageBlock, ProviderCapabilities, ProviderExecutionPhase, Role,\n};\n",
)
replace_once(
    "crates/medusa-agent/src/engine_support.rs",
    '''pub(crate) fn system_prompt_with_context(\n    mode: Mode,\n''',
    '''pub(crate) const fn provider_execution_phase(mode: Mode) -> ProviderExecutionPhase {\n    match mode {\n        Mode::ReadOnly => ProviderExecutionPhase::Planning,\n        Mode::Review => ProviderExecutionPhase::HighRiskReview,\n        Mode::Yolo => ProviderExecutionPhase::Implementation,\n    }\n}\n\npub(crate) fn system_prompt_with_context(\n    mode: Mode,\n''',
)
replace_once(
    "crates/medusa-agent/src/engine_support.rs",
    '''    #[test]\n    fn web_tools_are_available_in_standard_and_planning_modes() {\n''',
    '''    #[test]\n    fn provider_phase_tracks_production_agent_mode() {\n        assert_eq!(\n            provider_execution_phase(Mode::ReadOnly),\n            ProviderExecutionPhase::Planning\n        );\n        assert_eq!(\n            provider_execution_phase(Mode::Review),\n            ProviderExecutionPhase::HighRiskReview\n        );\n        assert_eq!(\n            provider_execution_phase(Mode::Yolo),\n            ProviderExecutionPhase::Implementation\n        );\n    }\n\n    #[test]\n    fn web_tools_are_available_in_standard_and_planning_modes() {\n''',
)
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''        let phase = if self.config.agent.mode == Mode::ReadOnly {\n            ProviderExecutionPhase::Planning\n        } else {\n            ProviderExecutionPhase::Implementation\n        };\n''',
    '''        let phase = provider_execution_phase(self.config.agent.mode);\n''',
)
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''    Message, MessageBlock, ModelProvider, ModelRequest, ProviderExecutionPhase,\n    ProviderStreamEvent, ProviderStreamTranscript, ResponseBlock, Role,\n''',
    '''    Message, MessageBlock, ModelProvider, ModelRequest, ProviderStreamEvent,\n    ProviderStreamTranscript, ResponseBlock, Role,\n''',
)
