from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, found {count}: {old[:120]!r}")
    file.write_text(text.replace(old, new, 1), encoding="utf-8")


path = "crates/medusa-agent/src/engine.rs"

replace_once(
    path,
    '''    Message, MessageBlock, ModelProvider, ModelRequest, ProviderStreamEvent,\n    ProviderStreamTranscript, ResponseBlock, Role,\n''',
    '''    Message, MessageBlock, ModelProvider, ModelRequest, ProviderExecutionPhase,\n    ProviderStreamEvent, ProviderStreamTranscript, ResponseBlock, Role,\n''',
)

replace_once(
    path,
    '''    pub fn run_to_completion(&self, session: &mut AgentSession) -> MedusaResult<()> {\n        while !session.completed && session.turn < self.config.agent.max_turns {\n            match self.step(session) {\n                Ok(StepOutcome::WaitingForUser) => {\n                    let error = MedusaError::new(\n                        ErrorCode::DependencyUnavailable,\n                        ErrorCategory::Execution,\n                        "agent is waiting for a user response",\n                    );\n                    let _ = runtime_failure::handle(session, &error)?;\n                    return Err(error);\n                }\n                Ok(StepOutcome::TurnComplete) => return Ok(()),\n                Ok(StepOutcome::Continue | StepOutcome::Completed) => {}\n                Err(error) => match runtime_failure::handle(session, &error)? {\n                    runtime_failure::RuntimeFailureAction::Retry\n                    | runtime_failure::RuntimeFailureAction::Replan => continue,\n                    runtime_failure::RuntimeFailureAction::Stop => return Err(error),\n                },\n            }\n        }\n''',
    '''    pub fn run_to_completion(&self, session: &mut AgentSession) -> MedusaResult<()> {\n        let default_phase = provider_execution_phase(self.config.agent.mode);\n        let mut phase = default_phase;\n        while !session.completed && session.turn < self.config.agent.max_turns {\n            match self.step_for_provider_phase(session, phase) {\n                Ok(StepOutcome::WaitingForUser) => {\n                    let error = MedusaError::new(\n                        ErrorCode::DependencyUnavailable,\n                        ErrorCategory::Execution,\n                        "agent is waiting for a user response",\n                    );\n                    let _ = runtime_failure::handle(session, &error)?;\n                    return Err(error);\n                }\n                Ok(StepOutcome::TurnComplete) => return Ok(()),\n                Ok(StepOutcome::Continue | StepOutcome::Completed) => {\n                    phase = default_phase;\n                }\n                Err(error) => match runtime_failure::handle(session, &error)? {\n                    runtime_failure::RuntimeFailureAction::Retry => continue,\n                    runtime_failure::RuntimeFailureAction::Replan => {\n                        phase = ProviderExecutionPhase::Repair;\n                        continue;\n                    }\n                    runtime_failure::RuntimeFailureAction::Stop => return Err(error),\n                },\n            }\n        }\n''',
)

replace_once(
    path,
    '''    pub fn step(&self, session: &mut AgentSession) -> MedusaResult<StepOutcome> {\n        self.step_with_observer(session, |_| {})\n    }\n\n''',
    '''    pub fn step(&self, session: &mut AgentSession) -> MedusaResult<StepOutcome> {\n        self.step_with_observer(session, |_| {})\n    }\n\n    fn step_for_provider_phase(\n        &self,\n        session: &mut AgentSession,\n        phase: ProviderExecutionPhase,\n    ) -> MedusaResult<StepOutcome> {\n        self.step_with_observer_and_context_and_turn_instruction_for_phase(\n            session,\n            None,\n            None,\n            phase,\n            |_| {},\n        )\n    }\n\n''',
)

replace_once(
    path,
    '''    pub fn step_with_observer_and_context_and_turn_instruction<F>(\n        &self,\n        session: &mut AgentSession,\n        additional_system_context: Option<&str>,\n        turn_instruction: Option<&str>,\n        mut observer: F,\n    ) -> MedusaResult<StepOutcome>\n    where\n        F: FnMut(&AgentUpdate),\n    {\n        if session.completed {\n''',
    '''    pub fn step_with_observer_and_context_and_turn_instruction<F>(\n        &self,\n        session: &mut AgentSession,\n        additional_system_context: Option<&str>,\n        turn_instruction: Option<&str>,\n        observer: F,\n    ) -> MedusaResult<StepOutcome>\n    where\n        F: FnMut(&AgentUpdate),\n    {\n        self.step_with_observer_and_context_and_turn_instruction_for_phase(\n            session,\n            additional_system_context,\n            turn_instruction,\n            provider_execution_phase(self.config.agent.mode),\n            observer,\n        )\n    }\n\n    fn step_with_observer_and_context_and_turn_instruction_for_phase<F>(\n        &self,\n        session: &mut AgentSession,\n        additional_system_context: Option<&str>,\n        turn_instruction: Option<&str>,\n        phase: ProviderExecutionPhase,\n        mut observer: F,\n    ) -> MedusaResult<StepOutcome>\n    where\n        F: FnMut(&AgentUpdate),\n    {\n        if session.completed {\n''',
)

replace_once(
    path,
    '''        let streaming_repo = session.repo.clone();\n        let phase = provider_execution_phase(self.config.agent.mode);\n        let mut complete_request = |request: &ModelRequest| {\n''',
    '''        let streaming_repo = session.repo.clone();\n        let mut complete_request = |request: &ModelRequest| {\n''',
)

replace_once(
    path,
    '''#[cfg(test)]\nmod terminal_stop_reason_tests {\n''',
    '''#[cfg(test)]\nmod repair_phase_routing_tests {\n    use std::sync::{Arc, Mutex};\n\n    use medusa_provider::{ModelResponse, Usage};\n\n    use super::*;\n\n    struct PhaseRecordingProvider {\n        phases: Arc<Mutex<Vec<ProviderExecutionPhase>>>,\n    }\n\n    impl ModelProvider for PhaseRecordingProvider {\n        fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {\n            unreachable!("phase-aware cancellable path must be used")\n        }\n\n        fn complete_cancellable_for_phase(\n            &self,\n            _request: &ModelRequest,\n            phase: ProviderExecutionPhase,\n            _cancel: &AtomicBool,\n        ) -> MedusaResult<ModelResponse> {\n            self.phases.lock().expect("phase lock").push(phase);\n            Ok(ModelResponse {\n                response_id: Some("repair-phase".to_owned()),\n                stop_reason: Some("stop".to_owned()),\n                blocks: Vec::new(),\n                usage: Usage::default(),\n            })\n        }\n    }\n\n    #[test]\n    fn explicit_repair_phase_reaches_provider_entrypoint() {\n        let directory = tempfile::tempdir().expect("temporary repository");\n        let phases = Arc::new(Mutex::new(Vec::new()));\n        let engine = AgentEngine::new(\n            PhaseRecordingProvider {\n                phases: Arc::clone(&phases),\n            },\n            Config::default(),\n        );\n        let mut session = engine\n            .create_session(directory.path(), "repair failed verification".to_owned())\n            .expect("create session");\n\n        engine\n            .step_for_provider_phase(&mut session, ProviderExecutionPhase::Repair)\n            .expect("repair step");\n\n        assert_eq!(\n            *phases.lock().expect("phase lock"),\n            vec![ProviderExecutionPhase::Repair]\n        );\n    }\n}\n\n#[cfg(test)]\nmod terminal_stop_reason_tests {\n''',
)
