from pathlib import Path

engine_path = Path("crates/medusa-agent/src/engine.rs")
engine = engine_path.read_text()
old = 'response.stop_reason.as_deref() == Some("end_turn")'
if engine.count(old) != 2:
    raise SystemExit("unexpected terminal stop-reason anchors")
engine = engine.replace(old, "stop_reason_completes_turn(response.stop_reason.as_deref())")

anchor = "fn approval_action_label(name: &str, input: &serde_json::Value) -> String {\n"
helper = '''fn stop_reason_completes_turn(stop_reason: Option<&str>) -> bool {
    matches!(stop_reason.map(str::trim), Some("end_turn" | "stop"))
}

'''
if engine.count(anchor) != 1:
    raise SystemExit("terminal stop helper anchor is missing")
engine = engine.replace(anchor, helper + anchor, 1)

tests = r'''

#[cfg(test)]
mod terminal_stop_reason_tests {
    use std::{collections::VecDeque, sync::Mutex};

    use medusa_provider::{ModelResponse, ResponseBlock, Usage};

    use super::*;

    struct ScriptedStopProvider {
        responses: Mutex<VecDeque<ModelResponse>>,
    }

    impl ScriptedStopProvider {
        fn new(stop_reason: &str) -> Self {
            Self {
                responses: Mutex::new(
                    [ModelResponse {
                        response_id: Some("stop-reason-fixture".to_owned()),
                        stop_reason: Some(stop_reason.to_owned()),
                        blocks: vec![ResponseBlock::Text {
                            text: "Evidence-backed delegated report complete.".to_owned(),
                        }],
                        usage: Usage::default(),
                    }]
                    .into(),
                ),
            }
        }
    }

    impl ModelProvider for ScriptedStopProvider {
        fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
            self.responses
                .lock()
                .expect("scripted stop provider lock")
                .pop_front()
                .ok_or_else(|| {
                    MedusaError::new(
                        ErrorCode::DependencyUnavailable,
                        ErrorCategory::Internal,
                        "scripted stop response exhausted",
                    )
                })
        }
    }

    fn read_only_step(stop_reason: &str) -> StepOutcome {
        let directory = tempfile::tempdir().expect("temporary repository");
        let mut config = Config::default();
        config.agent.mode = Mode::ReadOnly;
        let engine = AgentEngine::new(ScriptedStopProvider::new(stop_reason), config);
        let mut session = engine
            .create_session(directory.path(), "inspect the repository".to_owned())
            .expect("create delegated session");
        engine.step(&mut session).expect("run delegated step")
    }

    #[test]
    fn openai_stop_completes_a_read_only_turn() {
        assert_eq!(read_only_step("stop"), StepOutcome::TurnComplete);
    }

    #[test]
    fn anthropic_end_turn_still_completes_a_read_only_turn() {
        assert_eq!(read_only_step("end_turn"), StepOutcome::TurnComplete);
    }

    #[test]
    fn truncated_provider_output_does_not_complete_the_turn() {
        assert_eq!(read_only_step("length"), StepOutcome::Continue);
    }
}
'''
if "mod terminal_stop_reason_tests" in engine:
    raise SystemExit("terminal stop-reason tests already exist")
engine_path.write_text(engine.rstrip() + tests + "\n")
