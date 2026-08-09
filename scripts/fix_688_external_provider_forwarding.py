from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, found {count}: {old[:140]!r}")
    file.write_text(text.replace(old, new, 1), encoding="utf-8")


path = "crates/medusa-external-provider/src/lib.rs"

replace_once(
    path,
    '''    ImageSource, Message, MessageBlock, MiniMaxProvider, ModelProvider, ModelRequest,\n    ModelResponse, OpenAiProvider, ProviderCapabilities, ProviderHealth, ProviderManager,\n    ProviderRouteProfile, ResponseBlock, Role, RouteRetryPolicy, ToolDefinition, Usage,\n''',
    '''    ImageSource, Message, MessageBlock, MiniMaxProvider, ModelProvider, ModelRequest,\n    ModelResponse, OpenAiProvider, ProviderCapabilities, ProviderExecutionPhase, ProviderHealth,\n    ProviderManager, ProviderRouteProfile, ProviderStreamEvent, ResponseBlock, Role,\n    RouteRetryPolicy, ToolDefinition, Usage,\n''',
)

replace_once(
    path,
    '''    fn legacy_capabilities(&self) -> ProviderCapabilities {\n        let capabilities = truthful_capabilities(&self.config);\n        ProviderCapabilities {\n            image_input: capabilities.image_input,\n            supported_image_media_types: capabilities.supported_image_media_types,\n            max_image_bytes: capabilities.max_image_bytes,\n            max_images_per_request: capabilities.max_images_per_request,\n            tool_calling: capabilities.tool_calling,\n            streaming: false,\n        }\n    }\n''',
    '''    fn legacy_capabilities(&self) -> ProviderCapabilities {\n        let capabilities = truthful_capabilities(&self.config);\n        ProviderCapabilities {\n            image_input: capabilities.image_input,\n            supported_image_media_types: capabilities.supported_image_media_types,\n            max_image_bytes: capabilities.max_image_bytes,\n            max_images_per_request: capabilities.max_images_per_request,\n            tool_calling: capabilities.tool_calling,\n            streaming: capabilities.streaming_text,\n        }\n    }\n''',
)

replace_once(
    path,
    '''            tool_calling: capabilities.tool_calling,\n            streaming: false,\n            retry: RouteRetryPolicy {\n''',
    '''            tool_calling: capabilities.tool_calling,\n            streaming: capabilities.streaming_text,\n            retry: RouteRetryPolicy {\n''',
)

replace_once(
    path,
    '''    fn complete_cancellable(\n        &self,\n        request: &ModelRequest,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {\n        self.state\n            .initialize()?\n            .complete_cancellable(request, cancel)\n    }\n\n    fn capabilities(&self) -> ProviderCapabilities {\n''',
    '''    fn complete_cancellable(\n        &self,\n        request: &ModelRequest,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {\n        self.state\n            .initialize()?\n            .complete_cancellable(request, cancel)\n    }\n\n    fn complete_streaming_cancellable(\n        &self,\n        request: &ModelRequest,\n        cancel: &AtomicBool,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        self.state\n            .initialize()?\n            .complete_streaming_cancellable(request, cancel, sink)\n    }\n\n    fn capabilities(&self) -> ProviderCapabilities {\n''',
)

replace_once(
    path,
    '''    fn complete_cancellable(\n        &self,\n        request: &ModelRequest,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {\n        self.manager.complete_cancellable(request, cancel)\n    }\n\n    fn capabilities(&self) -> ProviderCapabilities {\n''',
    '''    fn complete_cancellable(\n        &self,\n        request: &ModelRequest,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {\n        self.manager.complete_cancellable(request, cancel)\n    }\n\n    fn complete_cancellable_for_phase(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {\n        self.manager\n            .complete_cancellable_for_phase(request, phase, cancel)\n    }\n\n    fn complete_streaming_cancellable(\n        &self,\n        request: &ModelRequest,\n        cancel: &AtomicBool,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        self.manager\n            .complete_streaming_cancellable(request, cancel, sink)\n    }\n\n    fn complete_streaming_cancellable_for_phase(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        self.manager\n            .complete_streaming_cancellable_for_phase(request, phase, cancel, sink)\n    }\n\n    fn capabilities(&self) -> ProviderCapabilities {\n''',
)

replace_once(
    path,
    '''    ProviderCapabilitySet {\n        image_input,\n        tool_calling: config.model.tool_calling\n            && matches!(\n                protocol.as_str(),\n                "anthropic" | "openai" | "anthropic-compatible"\n            ),\n        streaming_text: false,\n        streaming_audio: false,\n        cancellation: false,\n''',
    '''    let streaming_text = config.model.streaming && protocol == "openai";\n    ProviderCapabilitySet {\n        image_input,\n        tool_calling: config.model.tool_calling\n            && matches!(\n                protocol.as_str(),\n                "anthropic" | "openai" | "anthropic-compatible"\n            ),\n        streaming_text,\n        streaming_audio: false,\n        cancellation: true,\n''',
)

replace_once(
    path,
    '''    #[test]\n    fn configuration_cannot_invent_streaming_or_cancellation() {\n        let mut config = Config::default();\n        config.model.streaming = true;\n        let manager = LazyConfiguredProviderManager::from_config_in_memory(\n            &config,\n            Some("session-key".to_owned()),\n        )\n        .unwrap();\n        let readiness = manager.route_readiness().unwrap();\n        assert!(!readiness[0].capabilities.streaming_text);\n        assert!(!readiness[0].capabilities.cancellation);\n        assert!(!manager.capabilities().streaming);\n    }\n''',
    '''    #[test]\n    fn supported_streaming_and_cancellation_reach_runtime_capabilities() {\n        let mut config = Config::default();\n        config.model.streaming = true;\n        let manager = LazyConfiguredProviderManager::from_config_in_memory(\n            &config,\n            Some("session-key".to_owned()),\n        )\n        .unwrap();\n        let readiness = manager.route_readiness().unwrap();\n        assert!(readiness[0].capabilities.streaming_text);\n        assert!(readiness[0].capabilities.cancellation);\n        assert!(manager.capabilities().streaming);\n    }\n\n    #[test]\n    fn anthropic_routes_do_not_claim_unsupported_streaming() {\n        let mut config = Config::default();\n        config.model.protocol = "anthropic".to_owned();\n        config.model.streaming = true;\n        let manager = LazyConfiguredProviderManager::from_config_in_memory(\n            &config,\n            Some("session-key".to_owned()),\n        )\n        .unwrap();\n        let readiness = manager.route_readiness().unwrap();\n        assert!(!readiness[0].capabilities.streaming_text);\n        assert!(!manager.capabilities().streaming);\n    }\n''',
)
