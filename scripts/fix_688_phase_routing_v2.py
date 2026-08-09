from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, found {count}: {old[:100]!r}")
    file.write_text(text.replace(old, new, 1), encoding="utf-8")


# Read-only is Medusa's existing planning mode authority.
replace_once(
    "crates/medusa-agent/src/engine.rs",
    "self.config.agent.mode == Mode::Plan",
    "self.config.agent.mode == Mode::ReadOnly",
)

# Preserve phase-aware routing for non-streaming provider routes too.
replace_once(
    "crates/medusa-provider/src/contracts.rs",
    '''    fn capabilities(&self) -> ProviderCapabilities {\n        ProviderCapabilities::default()\n    }\n''',
    '''    /// Completes with an explicit execution phase for phase-aware route selection.\n    /// Providers that do not route internally can ignore the phase and preserve existing behavior.\n    fn complete_cancellable_for_phase(\n        &self,\n        request: &ModelRequest,\n        _phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_cancellable(request, cancel)\n    }\n\n    fn capabilities(&self) -> ProviderCapabilities {\n        ProviderCapabilities::default()\n    }\n''',
)
replace_once(
    "crates/medusa-provider/src/manager.rs",
    '''    fn complete_streaming_cancellable_for_phase(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_with_cancel_and_sink(request, phase, Some(cancel), Some(sink))\n    }\n\n    fn capabilities(&self) -> ProviderCapabilities {\n''',
    '''    fn complete_cancellable_for_phase(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_with_cancel_and_sink(request, phase, Some(cancel), None)\n    }\n\n    fn complete_streaming_cancellable_for_phase(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_with_cancel_and_sink(request, phase, Some(cancel), Some(sink))\n    }\n\n    fn capabilities(&self) -> ProviderCapabilities {\n''',
)
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''        let mut complete_request = |request: &ModelRequest| {\n            if !streaming {\n                return self\n                    .provider\n                    .complete_cancellable(request, &self.cancellation);\n            }\n''',
    '''        let phase = if self.config.agent.mode == Mode::ReadOnly {\n            ProviderExecutionPhase::Planning\n        } else {\n            ProviderExecutionPhase::Implementation\n        };\n        let mut complete_request = |request: &ModelRequest| {\n            if !streaming {\n                return self.provider.complete_cancellable_for_phase(\n                    request,\n                    phase,\n                    &self.cancellation,\n                );\n            }\n''',
)
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''            let phase = if self.config.agent.mode == Mode::ReadOnly {\n                ProviderExecutionPhase::Planning\n            } else {\n                ProviderExecutionPhase::Implementation\n            };\n            self.provider.complete_streaming_cancellable_for_phase(\n''',
    '''            self.provider.complete_streaming_cancellable_for_phase(\n''',
)
