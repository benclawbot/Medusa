from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, found {count}: {old[:80]!r}")
    file.write_text(text.replace(old, new, 1), encoding="utf-8")


# Provider execution phase contract and context-aware trait entrypoint.
replace_once(
    "crates/medusa-provider/src/contracts.rs",
    "/// One model request.\n",
    '''/// Execution phase used by routing policy without contaminating provider request payloads.\n#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]\n#[serde(rename_all = "snake_case")]\npub enum ProviderExecutionPhase {\n    #[default]\n    Default,\n    Planning,\n    Implementation,\n    HighRiskReview,\n    Repair,\n    Summarization,\n    Formatting,\n}\n\n/// One model request.\n''',
)
replace_once(
    "crates/medusa-provider/src/contracts.rs",
    '''    fn complete_cancellable(\n        &self,\n        request: &ModelRequest,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {\n''',
    '''    /// Streams with an explicit execution phase for phase-aware route selection.\n    /// Providers that do not route internally can ignore the phase and preserve existing behavior.\n    fn complete_streaming_cancellable_for_phase(\n        &self,\n        request: &ModelRequest,\n        _phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_streaming_cancellable(request, cancel, sink)\n    }\n\n    fn complete_cancellable(\n        &self,\n        request: &ModelRequest,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {\n''',
)

# Export the execution phase.
replace_once(
    "crates/medusa-provider/src/lib.rs",
    '''    ImageSource, Message, MessageBlock, ModelProvider, ModelRequest, ModelResponse,\n    ProviderCapabilities, ResponseBlock, Role, ToolDefinition, Usage,\n''',
    '''    ImageSource, Message, MessageBlock, ModelProvider, ModelRequest, ModelResponse,\n    ProviderCapabilities, ProviderExecutionPhase, ResponseBlock, Role, ToolDefinition, Usage,\n''',
)

# Phase-specific latency policy derived deterministically from the configured base policy.
replace_once(
    "crates/medusa-provider/src/route_latency.rs",
    "use crate::ProviderRouteProfile;\n",
    "use crate::{ProviderExecutionPhase, ProviderRouteProfile};\n",
)
replace_once(
    "crates/medusa-provider/src/route_latency.rs",
    '''    pub const fn production_default() -> Self {\n        Self {\n            cold_start_duration_ms: 30_000,\n            failure_penalty_ms_per_mille: 10,\n            max_cache_credit_ms: 2_000,\n        }\n    }\n''',
    '''    pub const fn production_default() -> Self {\n        Self {\n            cold_start_duration_ms: 30_000,\n            failure_penalty_ms_per_mille: 10,\n            max_cache_credit_ms: 2_000,\n        }\n    }\n\n    /// Derives deterministic route-scoring policy for the current execution phase.\n    #[must_use]\n    pub fn for_phase(self, phase: ProviderExecutionPhase) -> Self {\n        match phase {\n            ProviderExecutionPhase::Default | ProviderExecutionPhase::Implementation => self,\n            ProviderExecutionPhase::Planning => Self {\n                cold_start_duration_ms: self.cold_start_duration_ms.saturating_mul(3) / 4,\n                failure_penalty_ms_per_mille: self.failure_penalty_ms_per_mille,\n                max_cache_credit_ms: self.max_cache_credit_ms.saturating_mul(3) / 2,\n            },\n            ProviderExecutionPhase::HighRiskReview => Self {\n                cold_start_duration_ms: self.cold_start_duration_ms,\n                failure_penalty_ms_per_mille: self\n                    .failure_penalty_ms_per_mille\n                    .saturating_mul(2),\n                max_cache_credit_ms: self.max_cache_credit_ms / 2,\n            },\n            ProviderExecutionPhase::Repair => Self {\n                cold_start_duration_ms: self.cold_start_duration_ms,\n                failure_penalty_ms_per_mille: self\n                    .failure_penalty_ms_per_mille\n                    .saturating_mul(3)\n                    / 2,\n                max_cache_credit_ms: self.max_cache_credit_ms / 2,\n            },\n            ProviderExecutionPhase::Summarization | ProviderExecutionPhase::Formatting => Self {\n                cold_start_duration_ms: self.cold_start_duration_ms / 2,\n                failure_penalty_ms_per_mille: self.failure_penalty_ms_per_mille,\n                max_cache_credit_ms: self.max_cache_credit_ms.saturating_mul(2),\n            },\n        }\n    }\n''',
)
replace_once(
    "crates/medusa-provider/src/route_latency.rs",
    '''    #[test]\n    fn capability_incompatible_routes_are_excluded() {\n''',
    '''    #[test]\n    fn execution_phase_can_change_route_order_without_changing_measurements() {\n        let profiles = vec![\n            profile("fast-less-verified", true, true),\n            profile("slower-verified", true, true),\n        ];\n        let stats = vec![\n            RouteLatencyStats {\n                samples: 10,\n                successes: 10,\n                total_duration_ms: 1_000,\n                verified_successes: 5,\n                verified_failures: 5,\n                ..RouteLatencyStats::default()\n            },\n            RouteLatencyStats {\n                samples: 10,\n                successes: 10,\n                total_duration_ms: 60_000,\n                verified_successes: 10,\n                ..RouteLatencyStats::default()\n            },\n        ];\n        let base = RouteLatencyPolicy::default();\n\n        assert_eq!(\n            latency_aware_route_order(\n                &profiles,\n                &stats,\n                true,\n                true,\n                base.for_phase(ProviderExecutionPhase::Planning),\n            ),\n            vec![0, 1]\n        );\n        assert_eq!(\n            latency_aware_route_order(\n                &profiles,\n                &stats,\n                true,\n                true,\n                base.for_phase(ProviderExecutionPhase::HighRiskReview),\n            ),\n            vec![1, 0]\n        );\n    }\n\n    #[test]\n    fn capability_incompatible_routes_are_excluded() {\n''',
)

# ProviderManager consumes the phase for route and hedge scoring while preserving old entrypoints.
replace_once(
    "crates/medusa-provider/src/manager.rs",
    '''    HedgePolicy, ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities,\n    ProviderHealthStore, ProviderRouteLatencyStore, ProviderStreamEvent, RouteLatencyPolicy,\n''',
    '''    HedgePolicy, ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities,\n    ProviderExecutionPhase, ProviderHealthStore, ProviderRouteLatencyStore, ProviderStreamEvent, RouteLatencyPolicy,\n''',
)
replace_once(
    "crates/medusa-provider/src/manager.rs",
    "        self.complete_with_cancel_and_sink(request, None, None)\n",
    "        self.complete_with_cancel_and_sink(request, ProviderExecutionPhase::Default, None, None)\n",
)
replace_once(
    "crates/medusa-provider/src/manager.rs",
    "        self.complete_with_cancel_and_sink(request, None, Some(sink))\n",
    "        self.complete_with_cancel_and_sink(request, ProviderExecutionPhase::Default, None, Some(sink))\n",
)
replace_once(
    "crates/medusa-provider/src/manager.rs",
    "        self.complete_with_cancel_and_sink(request, Some(cancel), Some(sink))\n",
    "        self.complete_with_cancel_and_sink(request, ProviderExecutionPhase::Default, Some(cancel), Some(sink))\n",
)
replace_once(
    "crates/medusa-provider/src/manager.rs",
    "        self.complete_with_cancel_and_sink(request, Some(cancel), None)\n",
    "        self.complete_with_cancel_and_sink(request, ProviderExecutionPhase::Default, Some(cancel), None)\n",
)
replace_once(
    "crates/medusa-provider/src/manager.rs",
    '''    fn capabilities(&self) -> ProviderCapabilities {\n''',
    '''    fn complete_streaming_cancellable_for_phase(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_with_cancel_and_sink(request, phase, Some(cancel), Some(sink))\n    }\n\n    fn capabilities(&self) -> ProviderCapabilities {\n''',
)
replace_once(
    "crates/medusa-provider/src/manager.rs",
    '''    fn complete_with_cancel_and_sink(\n        &self,\n        request: &ModelRequest,\n        cancel: Option<&AtomicBool>,\n        mut sink: Option<&mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>>,\n    ) -> MedusaResult<ModelResponse> {\n''',
    '''    fn complete_with_cancel_and_sink(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: Option<&AtomicBool>,\n        mut sink: Option<&mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>>,\n    ) -> MedusaResult<ModelResponse> {\n''',
)
replace_once(
    "crates/medusa-provider/src/manager.rs",
    '''        let stats = self.latency.stats()?;\n        let route_order = latency_aware_route_order(\n''',
    '''        let stats = self.latency.stats()?;\n        let phase_latency_policy = self.latency_policy.for_phase(phase);\n        let route_order = latency_aware_route_order(\n''',
)
replace_once(
    "crates/medusa-provider/src/manager.rs",
    '''            false,\n            self.latency_policy,\n        );\n        if let Some(decision) = hedge_decision(\n''',
    '''            false,\n            phase_latency_policy,\n        );\n        if let Some(decision) = hedge_decision(\n''',
)
replace_once(
    "crates/medusa-provider/src/manager.rs",
    '''            request.max_tokens,\n            self.hedge_policy,\n            self.latency_policy,\n        ) {\n''',
    '''            request.max_tokens,\n            self.hedge_policy,\n            phase_latency_policy,\n        ) {\n''',
)

# Production agent entrypoint distinguishes planning and implementation turns.
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''    Message, MessageBlock, ModelProvider, ModelRequest, ProviderStreamEvent,\n    ProviderStreamTranscript, ResponseBlock, Role,\n''',
    '''    Message, MessageBlock, ModelProvider, ModelRequest, ProviderExecutionPhase, ProviderStreamEvent,\n    ProviderStreamTranscript, ResponseBlock, Role,\n''',
)
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''            self.provider\n                .complete_streaming_cancellable(request, &self.cancellation, &mut sink)\n''',
    '''            let phase = if self.config.agent.mode == Mode::Plan {\n                ProviderExecutionPhase::Planning\n            } else {\n                ProviderExecutionPhase::Implementation\n            };\n            self.provider.complete_streaming_cancellable_for_phase(\n                request,\n                phase,\n                &self.cancellation,\n                &mut sink,\n            )\n''',
)
