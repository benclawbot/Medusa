from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, found {count}: {old[:120]!r}")
    p.write_text(text.replace(old, new, 1))


replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''pub(crate) fn persist_before_provider_call(\n    session: &AgentSession,\n    request: &ModelRequest,\n    phase: ProviderExecutionPhase,\n    provider: &str,\n    model: &str,\n    capabilities: &ProviderCapabilities,\n    execution_policy: &Value,\n    assembly_provenance: BTreeMap<String, String>,\n    previous: Option<&ManifestRef>,\n) -> MedusaResult<ManifestRef> {\n''',
    '''pub(crate) struct RequestManifestInput<'a> {\n    pub phase: ProviderExecutionPhase,\n    pub provider: &'a str,\n    pub model: &'a str,\n    pub capabilities: &'a ProviderCapabilities,\n    pub execution_policy: &'a Value,\n    pub assembly_provenance: BTreeMap<String, String>,\n    pub previous: Option<&'a ManifestRef>,\n}\n\npub(crate) fn persist_before_provider_call(\n    session: &AgentSession,\n    request: &ModelRequest,\n    input: RequestManifestInput<'_>,\n) -> MedusaResult<ManifestRef> {\n    let RequestManifestInput {\n        phase,\n        provider,\n        model,\n        capabilities,\n        execution_policy,\n        assembly_provenance,\n        previous,\n    } = input;\n''',
)

# compaction initial call
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''        let manifest = effective_request::persist_before_provider_call(\n            session,\n            &summary_request,\n            ProviderExecutionPhase::Summarization,\n            &self.config.model.provider,\n            &self.config.model.name,\n            &self.provider.capabilities(),\n            &execution_policy,\n            summary_provenance,\n            None,\n        )?;\n''',
    '''        let capabilities = self.provider.capabilities();\n        let manifest = effective_request::persist_before_provider_call(\n            session,\n            &summary_request,\n            effective_request::RequestManifestInput {\n                phase: ProviderExecutionPhase::Summarization,\n                provider: &self.config.model.provider,\n                model: &self.config.model.name,\n                capabilities: &capabilities,\n                execution_policy: &execution_policy,\n                assembly_provenance: summary_provenance,\n                previous: None,\n            },\n        )?;\n''',
)

# normal initial call
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''        let mut active_manifest = effective_request::persist_before_provider_call(\n            session,\n            &request,\n            phase,\n            &self.config.model.provider,\n            &self.config.model.name,\n            &self.provider.capabilities(),\n            &execution_policy,\n            assembly_provenance.clone(),\n            None,\n        )?;\n''',
    '''        let capabilities = self.provider.capabilities();\n        let mut active_manifest = effective_request::persist_before_provider_call(\n            session,\n            &request,\n            effective_request::RequestManifestInput {\n                phase,\n                provider: &self.config.model.provider,\n                model: &self.config.model.name,\n                capabilities: &capabilities,\n                execution_policy: &execution_policy,\n                assembly_provenance: assembly_provenance.clone(),\n                previous: None,\n            },\n        )?;\n''',
)

# context-limit linked retry
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''                let retry_manifest = effective_request::persist_before_provider_call(\n                    session,\n                    &request,\n                    phase,\n                    &self.config.model.provider,\n                    &self.config.model.name,\n                    &self.provider.capabilities(),\n                    &execution_policy,\n                    assembly_provenance.clone(),\n                    Some(&active_manifest),\n                )?;\n''',
    '''                let retry_capabilities = self.provider.capabilities();\n                let retry_manifest = effective_request::persist_before_provider_call(\n                    session,\n                    &request,\n                    effective_request::RequestManifestInput {\n                        phase,\n                        provider: &self.config.model.provider,\n                        model: &self.config.model.name,\n                        capabilities: &retry_capabilities,\n                        execution_policy: &execution_policy,\n                        assembly_provenance: assembly_provenance.clone(),\n                        previous: Some(&active_manifest),\n                    },\n                )?;\n''',
)
