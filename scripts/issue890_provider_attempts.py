from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    target = Path(path)
    text = target.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, found {count}: {old[:140]!r}")
    target.write_text(text.replace(old, new, 1))


# Provider-neutral, sanitized physical-attempt contract.
replace_once(
    "crates/medusa-provider/src/contracts.rs",
    '''pub enum ProviderExecutionPhase {\n    #[default]\n    Default,\n    Planning,\n    Implementation,\n    HighRiskReview,\n    Repair,\n    Summarization,\n    Formatting,\n}\n''',
    '''pub enum ProviderExecutionPhase {\n    #[default]\n    Default,\n    Planning,\n    Implementation,\n    HighRiskReview,\n    Repair,\n    Summarization,\n    Formatting,\n}\n\n/// Why one physical provider route is being attempted for an effective request.\n#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]\n#[serde(rename_all = "snake_case")]\npub enum ProviderAttemptKind {\n    Primary,\n    Retry,\n    Failover,\n    HedgePrimary,\n    HedgeSecondary,\n}\n\n/// Sanitized route identity persisted before a physical provider invocation.\n///\n/// Endpoint URLs and authentication sources are deliberately excluded so an audit record cannot\n/// become a credential or signed-URL side channel.\n#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]\n#[serde(deny_unknown_fields)]\npub struct ProviderAttemptDescriptor {\n    pub route_id: String,\n    pub provider: String,\n    pub model: String,\n    pub protocol: String,\n    pub tool_calling: bool,\n    pub streaming: bool,\n    pub max_retries: u8,\n    pub retry_base_delay_ms: u64,\n    pub retry_max_delay_ms: u64,\n    pub retry_jitter_ms: u64,\n    pub route_ordinal: usize,\n    pub retry_ordinal: u8,\n    pub kind: ProviderAttemptKind,\n    pub conditional: bool,\n    pub conditional_launch_after_ms: Option<u64>,\n}\n''',
)
replace_once(
    "crates/medusa-provider/src/contracts.rs",
    '''    fn complete_streaming_cancellable_for_phase(\n        &self,\n        request: &ModelRequest,\n        _phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_streaming_cancellable(request, cancel, sink)\n    }\n''',
    '''    fn complete_streaming_cancellable_for_phase(\n        &self,\n        request: &ModelRequest,\n        _phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_streaming_cancellable(request, cancel, sink)\n    }\n\n    /// Streams while allowing route-managing providers to durably certify each physical attempt\n    /// before it starts. Direct providers use the already-persisted logical request manifest and\n    /// therefore do not emit an additional nested attempt by default.\n    fn complete_streaming_cancellable_for_phase_with_attempts(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n        _before_attempt: &mut dyn FnMut(&ProviderAttemptDescriptor) -> MedusaResult<()>,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_streaming_cancellable_for_phase(request, phase, cancel, sink)\n    }\n''',
)
replace_once(
    "crates/medusa-provider/src/contracts.rs",
    '''    fn complete_cancellable_for_phase(\n        &self,\n        request: &ModelRequest,\n        _phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_cancellable(request, cancel)\n    }\n''',
    '''    fn complete_cancellable_for_phase(\n        &self,\n        request: &ModelRequest,\n        _phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_cancellable(request, cancel)\n    }\n\n    /// Completes while allowing route-managing providers to durably certify each physical attempt\n    /// before it starts. The callback must fail closed: returning an error prevents invocation.\n    fn complete_cancellable_for_phase_with_attempts(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n        _before_attempt: &mut dyn FnMut(&ProviderAttemptDescriptor) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_cancellable_for_phase(request, phase, cancel)\n    }\n''',
)

replace_once(
    "crates/medusa-provider/src/lib.rs",
    '''pub use contracts::{\n    ImageSource, Message, MessageBlock, ModelProvider, ModelRequest, ModelResponse,\n    ProviderCapabilities, ProviderExecutionPhase, ResponseBlock, Role, ToolDefinition, Usage,\n};\n''',
    '''pub use contracts::{\n    ImageSource, Message, MessageBlock, ModelProvider, ModelRequest, ModelResponse,\n    ProviderAttemptDescriptor, ProviderAttemptKind, ProviderCapabilities, ProviderExecutionPhase,\n    ResponseBlock, Role, ToolDefinition, Usage,\n};\n''',
)

# ProviderManager reports each physical retry/failover/hedge intent before invocation.
replace_once(
    "crates/medusa-provider/src/manager.rs",
    '''    HedgePolicy, ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities,\n    ProviderContinuationCapabilities, ProviderExecutionPhase, ProviderHealthStore,\n    ProviderRouteLatencyStore, ProviderStreamEvent, RouteLatencyPolicy, RouteLatencyStats,\n''',
    '''    HedgePolicy, ModelProvider, ModelRequest, ModelResponse, ProviderAttemptDescriptor,\n    ProviderAttemptKind, ProviderCapabilities, ProviderContinuationCapabilities,\n    ProviderExecutionPhase, ProviderHealthStore, ProviderRouteLatencyStore, ProviderStreamEvent,\n    RouteLatencyPolicy, RouteLatencyStats,\n''',
)
replace_once(
    "crates/medusa-provider/src/manager.rs",
    '''    fn complete_streaming_cancellable_for_phase(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_with_cancel_and_sink(request, phase, Some(cancel), Some(sink))\n    }\n\n    fn capabilities(&self) -> ProviderCapabilities {\n''',
    '''    fn complete_streaming_cancellable_for_phase(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_with_cancel_and_sink(request, phase, Some(cancel), Some(sink))\n    }\n\n    fn complete_streaming_cancellable_for_phase_with_attempts(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n        before_attempt: &mut dyn FnMut(&ProviderAttemptDescriptor) -> MedusaResult<()>,\n        sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_with_cancel_and_sink_audited(\n            request,\n            phase,\n            Some(cancel),\n            Some(sink),\n            Some(before_attempt),\n        )\n    }\n\n    fn capabilities(&self) -> ProviderCapabilities {\n''',
)
replace_once(
    "crates/medusa-provider/src/manager.rs",
    '''    fn complete_cancellable_for_phase(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_with_cancel_and_sink(request, phase, Some(cancel), None)\n    }\n\n    fn complete_streaming_cancellable_for_phase(\n''',
    '''    fn complete_cancellable_for_phase(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_with_cancel_and_sink(request, phase, Some(cancel), None)\n    }\n\n    fn complete_cancellable_for_phase_with_attempts(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: &AtomicBool,\n        before_attempt: &mut dyn FnMut(&ProviderAttemptDescriptor) -> MedusaResult<()>,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_with_cancel_and_sink_audited(\n            request,\n            phase,\n            Some(cancel),\n            None,\n            Some(before_attempt),\n        )\n    }\n\n    fn complete_streaming_cancellable_for_phase(\n''',
)
replace_once(
    "crates/medusa-provider/src/manager.rs",
    '''    fn complete_with_cancel_and_sink(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: Option<&AtomicBool>,\n        mut sink: Option<&mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>>,\n    ) -> MedusaResult<ModelResponse> {\n''',
    '''    fn provider_attempt_descriptor(\n        &self,\n        index: usize,\n        route_ordinal: usize,\n        retry_ordinal: u8,\n        kind: ProviderAttemptKind,\n        conditional: bool,\n        conditional_launch_after_ms: Option<u64>,\n    ) -> MedusaResult<ProviderAttemptDescriptor> {\n        let profile = self.profiles.get(index).ok_or_else(|| {\n            cache_validation_error(format!("provider profile {index} is missing"))\n        })?;\n        Ok(ProviderAttemptDescriptor {\n            route_id: profile.id.clone(),\n            provider: profile.provider.clone(),\n            model: profile.model.clone(),\n            protocol: profile.protocol.clone(),\n            tool_calling: profile.tool_calling,\n            streaming: profile.streaming,\n            max_retries: profile.retry.max_retries,\n            retry_base_delay_ms: profile.retry.base_delay_ms,\n            retry_max_delay_ms: profile.retry.max_delay_ms,\n            retry_jitter_ms: profile.retry.jitter_ms,\n            route_ordinal,\n            retry_ordinal,\n            kind,\n            conditional,\n            conditional_launch_after_ms,\n        })\n    }\n\n    fn complete_with_cancel_and_sink(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: Option<&AtomicBool>,\n        sink: Option<&mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>>,\n    ) -> MedusaResult<ModelResponse> {\n        self.complete_with_cancel_and_sink_audited(request, phase, cancel, sink, None)\n    }\n\n    fn complete_with_cancel_and_sink_audited(\n        &self,\n        request: &ModelRequest,\n        phase: ProviderExecutionPhase,\n        cancel: Option<&AtomicBool>,\n        mut sink: Option<&mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>>,\n        mut before_attempt: Option<\n            &mut dyn FnMut(&ProviderAttemptDescriptor) -> MedusaResult<()>,\n        >,\n    ) -> MedusaResult<ModelResponse> {\n''',
)
replace_once(
    "crates/medusa-provider/src/manager.rs",
    '''        if let Some(decision) = hedge {\n            self.record_attempt(decision.primary_index)?;\n            let mut race_sink = sink.take();\n''',
    '''        if let Some(decision) = hedge {\n            let primary_ordinal = route_order\n                .iter()\n                .position(|index| *index == decision.primary_index)\n                .unwrap_or_default();\n            let secondary_ordinal = route_order\n                .iter()\n                .position(|index| *index == decision.secondary_index)\n                .unwrap_or(1);\n            if let Some(audit) = before_attempt.as_deref_mut() {\n                audit(&self.provider_attempt_descriptor(\n                    decision.primary_index,\n                    primary_ordinal,\n                    0,\n                    ProviderAttemptKind::HedgePrimary,\n                    false,\n                    None,\n                )?)?;\n                // The secondary is an explicit conditional intent. It is persisted before the\n                // race starts, and the race outcome records whether the delayed candidate launched.\n                audit(&self.provider_attempt_descriptor(\n                    decision.secondary_index,\n                    secondary_ordinal,\n                    0,\n                    ProviderAttemptKind::HedgeSecondary,\n                    true,\n                    Some(decision.launch_after_ms),\n                )?)?;\n            }\n            self.record_attempt(decision.primary_index)?;\n            let mut race_sink = sink.take();\n''',
)
replace_once(
    "crates/medusa-provider/src/manager.rs",
    '''            for attempt in 0..=policy.max_retries {\n                self.record_attempt(index)?;\n                let started = Instant::now();\n''',
    '''            for attempt in 0..=policy.max_retries {\n                let kind = if attempt > 0 {\n                    ProviderAttemptKind::Retry\n                } else if position > 0 {\n                    ProviderAttemptKind::Failover\n                } else {\n                    ProviderAttemptKind::Primary\n                };\n                if let Some(audit) = before_attempt.as_deref_mut() {\n                    audit(&self.provider_attempt_descriptor(\n                        index,\n                        position,\n                        attempt,\n                        kind,\n                        false,\n                        None,\n                    )?)?;\n                }\n                self.record_attempt(index)?;\n                let started = Instant::now();\n''',
)
# Upgrade the existing retry/failover test to prove callbacks run before every uncached call.
replace_once(
    "crates/medusa-provider/src/manager.rs",
    '''        let manager = ProviderManager::new(vec![primary, fallback]).without_sleep();\n\n        manager.complete(&request()).expect("fallback response");\n        let uncached = manager.execution_status().expect("uncached status");\n''',
    '''        let manager = ProviderManager::new(vec![primary, fallback]).without_sleep();\n        let mut attempts = Vec::new();\n        let mut before_attempt = |attempt: &ProviderAttemptDescriptor| {\n            attempts.push(attempt.clone());\n            Ok(())\n        };\n\n        manager\n            .complete_cancellable_for_phase_with_attempts(\n                &request(),\n                ProviderExecutionPhase::Default,\n                &AtomicBool::new(false),\n                &mut before_attempt,\n            )\n            .expect("fallback response");\n        let uncached = manager.execution_status().expect("uncached status");\n''',
)
replace_once(
    "crates/medusa-provider/src/manager.rs",
    '''        assert_eq!(primary_calls.load(Ordering::SeqCst), 2);\n        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);\n''',
    '''        assert_eq!(primary_calls.load(Ordering::SeqCst), 2);\n        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);\n        assert_eq!(\n            attempts\n                .iter()\n                .map(|attempt| attempt.kind)\n                .collect::<Vec<_>>(),\n            vec![\n                ProviderAttemptKind::Primary,\n                ProviderAttemptKind::Retry,\n                ProviderAttemptKind::Failover,\n            ]\n        );\n        assert_eq!(\n            attempts\n                .iter()\n                .map(|attempt| attempt.retry_ordinal)\n                .collect::<Vec<_>>(),\n            vec![0, 1, 0]\n        );\n        assert!(attempts.iter().all(|attempt| !attempt.conditional));\n''',
)

# Durable attempt records are children of the already-persisted logical request authority.
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''use medusa_provider::{ModelRequest, ProviderCapabilities, ProviderExecutionPhase};\n''',
    '''use medusa_provider::{\n    ModelRequest, ProviderAttemptDescriptor, ProviderCapabilities, ProviderExecutionPhase,\n};\n''',
)
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''struct EffectiveModelRequestManifestV1 {\n''',
    '''struct EffectiveModelRequestManifestV1 {\n''',
)
# Insert the provider-attempt manifest immediately after the effective-request manifest.
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''    tool_schema_fingerprints: BTreeMap<String, String>,\n}\n\n#[derive(Serialize)]\nstruct RequestFingerprintMaterial<'a> {\n''',
    '''    tool_schema_fingerprints: BTreeMap<String, String>,\n}\n\n#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]\n#[serde(deny_unknown_fields)]\nstruct ProviderAttemptManifestV1 {\n    schema_version: u16,\n    attempt_fingerprint: String,\n    request_id: String,\n    request_fingerprint: String,\n    descriptor: ProviderAttemptDescriptor,\n}\n\n#[derive(Serialize)]\nstruct RequestFingerprintMaterial<'a> {\n''',
)
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''pub(crate) fn failure_event(manifest: &ManifestRef, error: &MedusaError) -> EventPayload {\n    EventPayload::ModelRequestFailed {\n        request_id: manifest.request_id.clone(),\n        request_fingerprint: manifest.request_fingerprint.clone(),\n        error: error.to_string(),\n    }\n}\n\npub(crate) fn augment_execution_status(status: Option<Value>, manifest: &ManifestRef) -> Value {\n''',
    '''pub(crate) fn failure_event(manifest: &ManifestRef, error: &MedusaError) -> EventPayload {\n    EventPayload::ModelRequestFailed {\n        request_id: manifest.request_id.clone(),\n        request_fingerprint: manifest.request_fingerprint.clone(),\n        error: error.to_string(),\n    }\n}\n\npub(crate) fn persist_provider_attempt(\n    repo: &Path,\n    session_id: &str,\n    manifest: &ManifestRef,\n    descriptor: &ProviderAttemptDescriptor,\n) -> MedusaResult<String> {\n    let material = canonicalize_value(json!({\n        "schema_version": MANIFEST_SCHEMA_VERSION,\n        "request_id": manifest.request_id,\n        "request_fingerprint": manifest.request_fingerprint,\n        "descriptor": descriptor,\n    }));\n    let attempt_fingerprint = fingerprint_value(&material)?;\n    let attempt_ref = logical_ref("provider-attempt", &attempt_fingerprint);\n    let attempt = ProviderAttemptManifestV1 {\n        schema_version: MANIFEST_SCHEMA_VERSION,\n        attempt_fingerprint: attempt_fingerprint.clone(),\n        request_id: manifest.request_id.clone(),\n        request_fingerprint: manifest.request_fingerprint.clone(),\n        descriptor: descriptor.clone(),\n    };\n    persist_immutable(\n        repo,\n        "request-attempts",\n        session_id,\n        &attempt_fingerprint,\n        &serde_json::to_vec_pretty(&attempt).map_err(json_error)?,\n    )?;\n    Ok(attempt_ref)\n}\n\npub(crate) fn augment_execution_status(status: Option<Value>, manifest: &ManifestRef) -> Value {\n''',
)
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''    Ok(json!({\n        "healthy": true,\n''',
    '''    let provider_attempts = load_provider_attempts(repo, session_id, &manifest.request_id)?;\n    let provider_execution = provider_execution_status(repo, session_id, &manifest.request_id);\n    Ok(json!({\n        "healthy": true,\n''',
)
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''        "compaction_source_event_sequences": manifest.compaction_source_event_sequences,\n        "canonical_request": canonical_request,\n    }))\n}\n''',
    '''        "compaction_source_event_sequences": manifest.compaction_source_event_sequences,\n        "provider_attempts": provider_attempts,\n        "provider_execution": provider_execution,\n        "canonical_request": canonical_request,\n    }))\n}\n''',
)
# Fail closed on immutable primary conflicts instead of silently masking them via fallback.
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''    let primary = repo\n        .join(".medusa")\n        .join(category)\n        .join(session_id)\n        .join(format!("{key}.json"));\n    match persist_at(&primary, bytes) {\n        Ok(()) => Ok(primary),\n        Err(_) => {\n            let fallback = crate::session::fallback_storage_root(repo, category)\n                .join(session_id)\n                .join(format!("{key}.json"));\n            persist_at(&fallback, bytes)?;\n            Ok(fallback)\n        }\n    }\n}\n\nfn read_immutable(\n''',
    '''    let primary = repo\n        .join(".medusa")\n        .join(category)\n        .join(session_id)\n        .join(format!("{key}.json"));\n    if primary.is_file() {\n        persist_at(&primary, bytes)?;\n        return Ok(primary);\n    }\n    match persist_at(&primary, bytes) {\n        Ok(()) => Ok(primary),\n        Err(primary_error) => {\n            // A concurrent immutable write must never be bypassed by fallback storage.\n            if primary.is_file() {\n                return Err(primary_error);\n            }\n            let fallback = crate::session::fallback_storage_root(repo, category)\n                .join(session_id)\n                .join(format!("{key}.json"));\n            persist_at(&fallback, bytes)?;\n            Ok(fallback)\n        }\n    }\n}\n\nfn read_immutable(\n''',
)
# Add verified attempt loading and final provider status lookup before persist_at.
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''fn persist_at(path: &Path, bytes: &[u8]) -> MedusaResult<()> {\n''',
    '''fn load_provider_attempts(\n    repo: &Path,\n    session_id: &str,\n    request_id: &str,\n) -> MedusaResult<Vec<Value>> {\n    let roots = [\n        repo.join(".medusa").join("request-attempts").join(session_id),\n        crate::session::fallback_storage_root(repo, "request-attempts").join(session_id),\n    ];\n    let mut attempts = BTreeMap::<String, Value>::new();\n    for root in roots {\n        let Ok(entries) = fs::read_dir(root) else {\n            continue;\n        };\n        for entry in entries {\n            let entry = entry?;\n            if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {\n                continue;\n            }\n            let bytes = fs::read(entry.path())?;\n            let attempt: ProviderAttemptManifestV1 =\n                serde_json::from_slice(&bytes).map_err(json_error)?;\n            if attempt.request_id != request_id {\n                continue;\n            }\n            let material = canonicalize_value(json!({\n                "schema_version": attempt.schema_version,\n                "request_id": attempt.request_id,\n                "request_fingerprint": attempt.request_fingerprint,\n                "descriptor": attempt.descriptor,\n            }));\n            let reconstructed = fingerprint_value(&material)?;\n            if reconstructed != attempt.attempt_fingerprint {\n                return Err(audit_error(format!(\n                    "provider attempt fingerprint mismatch: recorded={}, reconstructed={reconstructed}",\n                    attempt.attempt_fingerprint\n                )));\n            }\n            attempts.insert(\n                attempt.attempt_fingerprint.clone(),\n                serde_json::to_value(attempt).map_err(json_error)?,\n            );\n        }\n    }\n    Ok(attempts.into_values().collect())\n}\n\nfn provider_execution_status(repo: &Path, session_id: &str, request_id: &str) -> Option<Value> {\n    let session = crate::session::load(repo, session_id).ok()?;\n    session.events.iter().rev().find_map(|event| match &event.payload {\n        EventPayload::ProviderExecutionRecorded { status }\n            if status\n                .get("effective_model_request")\n                .and_then(|request| request.get("request_id"))\n                .and_then(Value::as_str)\n                == Some(request_id) =>\n        {\n            Some(status.clone())\n        }\n        _ => None,\n    })\n}\n\nfn persist_at(path: &Path, bytes: &[u8]) -> MedusaResult<()> {\n''',
)

# Engine supplies the durable attempt callback to both normal and compaction requests.
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''        let request_started = std::time::Instant::now();\n        let semantic = match self.provider.complete_cancellable_for_phase(\n            &summary_request,\n            ProviderExecutionPhase::Summarization,\n            &self.cancellation,\n        ) {\n''',
    '''        let request_started = std::time::Instant::now();\n        let audit_repo = session.repo.clone();\n        let audit_session_id = session.id.to_string();\n        let mut before_provider_attempt = |attempt| {\n            effective_request::persist_provider_attempt(\n                &audit_repo,\n                &audit_session_id,\n                &manifest,\n                attempt,\n            )\n            .map(|_| ())\n        };\n        let semantic = match self\n            .provider\n            .complete_cancellable_for_phase_with_attempts(\n                &summary_request,\n                ProviderExecutionPhase::Summarization,\n                &self.cancellation,\n                &mut before_provider_attempt,\n            )\n        {\n''',
)
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''        let mut early_tool_executions = BTreeMap::<String, EarlyToolExecution>::new();\n        let streaming_repo = session.repo.clone();\n        let mut complete_request = |request: &ModelRequest| {\n            if !streaming {\n                return self.provider.complete_cancellable_for_phase(\n                    request,\n                    phase,\n                    &self.cancellation,\n                );\n            }\n            let mut sink = |event: ProviderStreamEvent| {\n''',
    '''        let mut early_tool_executions = BTreeMap::<String, EarlyToolExecution>::new();\n        let streaming_repo = session.repo.clone();\n        let audit_repo = session.repo.clone();\n        let audit_session_id = session.id.to_string();\n        let mut complete_request = |request: &ModelRequest, manifest: &effective_request::ManifestRef| {\n            let mut before_provider_attempt = |attempt| {\n                effective_request::persist_provider_attempt(\n                    &audit_repo,\n                    &audit_session_id,\n                    manifest,\n                    attempt,\n                )\n                .map(|_| ())\n            };\n            if !streaming {\n                return self\n                    .provider\n                    .complete_cancellable_for_phase_with_attempts(\n                        request,\n                        phase,\n                        &self.cancellation,\n                        &mut before_provider_attempt,\n                    );\n            }\n            let mut sink = |event: ProviderStreamEvent| {\n''',
)
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''            self.provider.complete_streaming_cancellable_for_phase(\n                request,\n                phase,\n                &self.cancellation,\n                &mut sink,\n            )\n        };\n        let response = match complete_request(&request) {\n''',
    '''            self.provider\n                .complete_streaming_cancellable_for_phase_with_attempts(\n                    request,\n                    phase,\n                    &self.cancellation,\n                    &mut before_provider_attempt,\n                    &mut sink,\n                )\n        };\n        let response = match complete_request(&request, &active_manifest) {\n''',
)
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''                active_manifest = retry_manifest;\n                match complete_request(&request) {\n''',
    '''                active_manifest = retry_manifest;\n                match complete_request(&request, &active_manifest) {\n''',
)
