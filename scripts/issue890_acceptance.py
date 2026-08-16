from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, found {count}: {old[:120]!r}")
    p.write_text(text.replace(old, new, 1))


# Fix the known clippy callback type-complexity regardless of whether the earlier runner landed.
p = Path("crates/medusa-provider/src/manager.rs")
text = p.read_text()
if "type ProviderAttemptHook<'a>" not in text:
    anchor = "enum RetryDisposition {\n    Retry,\n    Failover,\n    Permanent,\n}\n\n"
    alias = anchor + "type ProviderAttemptHook<'a> =\n    &'a mut dyn FnMut(&ProviderAttemptDescriptor) -> MedusaResult<()>;\n\n"
    if text.count(anchor) != 1:
        raise SystemExit("manager: retry disposition anchor mismatch")
    text = text.replace(anchor, alias, 1)
old_callback = "        mut before_attempt: Option<&mut dyn FnMut(&ProviderAttemptDescriptor) -> MedusaResult<()>>,\n"
if old_callback in text:
    text = text.replace(old_callback, "        mut before_attempt: Option<ProviderAttemptHook<'_>>,\n", 1)
p.write_text(text)

# Deterministic execution-policy projection; path order is canonicalized.
replace_once(
    "crates/medusa-agent/src/team.rs",
    '''    #[must_use]\n    pub fn allows(&self, tool: &str) -> bool {\n''',
    '''    pub(crate) fn audit_projection(&self) -> Value {\n        let mut allowed_write_paths = self.allowed_write_paths.clone();\n        if let Some(paths) = &mut allowed_write_paths {\n            paths.sort();\n            paths.dedup();\n        }\n        json!({\n            "allowed_tools": self.allowed_tools.as_ref().map(|tools| tools.iter().cloned().collect::<Vec<_>>()),\n            "allow_user_questions": self.allow_user_questions,\n            "allowed_write_paths": allowed_write_paths,\n        })\n    }\n\n    #[must_use]\n    pub fn allows(&self, tool: &str) -> bool {\n''',
)

# Manifest owns execution-policy and request-assembly provenance.
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''    capability_fingerprint: String,\n    source_event_sequences: Vec<u64>,\n''',
    '''    capability_fingerprint: String,\n    execution_policy_fingerprint: String,\n    execution_policy: Value,\n    assembly_provenance: BTreeMap<String, String>,\n    source_event_sequences: Vec<u64>,\n''',
)
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''    capability_fingerprint: &'a str,\n    request_content_fingerprint: &'a str,\n''',
    '''    capability_fingerprint: &'a str,\n    execution_policy_fingerprint: &'a str,\n    assembly_provenance: &'a BTreeMap<String, String>,\n    request_content_fingerprint: &'a str,\n''',
)
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''    capabilities: &ProviderCapabilities,\n    previous: Option<&ManifestRef>,\n) -> MedusaResult<ManifestRef> {\n''',
    '''    capabilities: &ProviderCapabilities,\n    execution_policy: &Value,\n    assembly_provenance: BTreeMap<String, String>,\n    previous: Option<&ManifestRef>,\n) -> MedusaResult<ManifestRef> {\n''',
)
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''    let capability_fingerprint = fingerprint_value(&capability_value)?;\n    let provider_profile_fingerprint = fingerprint_value(&canonicalize_value(json!({\n''',
    '''    let capability_fingerprint = fingerprint_value(&capability_value)?;\n    let execution_policy = canonicalize_value(execution_policy.clone());\n    let execution_policy_fingerprint = fingerprint_value(&execution_policy)?;\n    let provider_profile_fingerprint = fingerprint_value(&canonicalize_value(json!({\n''',
)
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''        capability_fingerprint: &capability_fingerprint,\n        request_content_fingerprint: &request_content_fingerprint,\n''',
    '''        capability_fingerprint: &capability_fingerprint,\n        execution_policy_fingerprint: &execution_policy_fingerprint,\n        assembly_provenance: &assembly_provenance,\n        request_content_fingerprint: &request_content_fingerprint,\n''',
)
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''        capability_fingerprint,\n        source_event_sequences,\n''',
    '''        capability_fingerprint,\n        execution_policy_fingerprint,\n        execution_policy,\n        assembly_provenance,\n        source_event_sequences,\n''',
)
# reconstruct request fingerprint with policy/assembly and diagnose individual tool-schema drift.
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''    let reconstructed_components = component_fingerprints_from_value(&canonical_request)?;\n    let mismatches =\n        component_mismatches(&manifest.component_fingerprints, &reconstructed_components);\n    if reconstructed_content_fingerprint != manifest.request_content_fingerprint\n        || !mismatches.is_empty()\n    {\n''',
    '''    let reconstructed_components = component_fingerprints_from_value(&canonical_request)?;\n    let mismatches =\n        component_mismatches(&manifest.component_fingerprints, &reconstructed_components);\n    let reconstructed_tool_schemas = tool_schema_fingerprints_from_value(&canonical_request)?;\n    let mismatched_tool_schemas = component_mismatches(\n        &manifest.tool_schema_fingerprints,\n        &reconstructed_tool_schemas,\n    );\n    if reconstructed_content_fingerprint != manifest.request_content_fingerprint\n        || !mismatches.is_empty()\n        || !mismatched_tool_schemas.is_empty()\n    {\n''',
)
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''        error\n            .context\n            .insert("mismatched_components".to_owned(), json!(mismatches));\n''',
    '''        error\n            .context\n            .insert("mismatched_components".to_owned(), json!(mismatches));\n        error.context.insert(\n            "mismatched_tool_schemas".to_owned(),\n            json!(mismatched_tool_schemas),\n        );\n''',
)
# The request-material literal occurs once in inspect after the construction literal was already patched.
needle = '''        capability_fingerprint: &manifest.capability_fingerprint,\n        request_content_fingerprint: &manifest.request_content_fingerprint,\n'''
replacement = '''        capability_fingerprint: &manifest.capability_fingerprint,\n        execution_policy_fingerprint: &manifest.execution_policy_fingerprint,\n        assembly_provenance: &manifest.assembly_provenance,\n        request_content_fingerprint: &manifest.request_content_fingerprint,\n'''
replace_once("crates/medusa-agent/src/engine/effective_request.rs", needle, replacement)
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''        "component_fingerprints": manifest.component_fingerprints,\n        "tool_schema_fingerprints": manifest.tool_schema_fingerprints,\n''',
    '''        "component_fingerprints": manifest.component_fingerprints,\n        "tool_schema_fingerprints": manifest.tool_schema_fingerprints,\n        "execution_policy_fingerprint": manifest.execution_policy_fingerprint,\n        "execution_policy": manifest.execution_policy,\n        "assembly_provenance": manifest.assembly_provenance,\n''',
)
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''fn component_mismatches(\n''',
    '''fn tool_schema_fingerprints_from_value(request: &Value) -> MedusaResult<BTreeMap<String, String>> {\n    request\n        .get("tools")\n        .and_then(Value::as_array)\n        .ok_or_else(|| audit_error("protected request tools must be an array"))?\n        .iter()\n        .map(|tool| {\n            let name = tool\n                .get("name")\n                .and_then(Value::as_str)\n                .ok_or_else(|| audit_error("protected tool definition has no name"))?;\n            let schema = tool\n                .get("input_schema")\n                .ok_or_else(|| audit_error("protected tool definition has no input_schema"))?;\n            Ok((name.to_owned(), fingerprint_value(schema)?))\n        })\n        .collect()\n}\n\npub(crate) fn fragment_fingerprint(text: &str) -> String {\n    sha256(text.as_bytes())\n}\n\nfn component_mismatches(\n''',
)
# Provider child attempts must match both parent ID and fingerprint, and return in semantic order.
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''    let provider_attempts = load_provider_attempts(repo, session_id, &manifest.request_id)?;\n''',
    '''    let provider_attempts = load_provider_attempts(repo, session_id, &manifest)?;\n''',
)
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''fn load_provider_attempts(\n    repo: &Path,\n    session_id: &str,\n    request_id: &str,\n) -> MedusaResult<Vec<Value>> {\n''',
    '''fn load_provider_attempts(\n    repo: &Path,\n    session_id: &str,\n    request: &EffectiveModelRequestManifestV1,\n) -> MedusaResult<Vec<Value>> {\n''',
)
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''            if attempt.request_id != request_id {\n                continue;\n            }\n''',
    '''            if attempt.request_id != request.request_id {\n                continue;\n            }\n            if attempt.request_fingerprint != request.request_fingerprint {\n                return Err(audit_error(format!(\n                    "provider attempt is linked to the wrong request fingerprint: {}",\n                    attempt.attempt_fingerprint\n                )));\n            }\n''',
)
replace_once(
    "crates/medusa-agent/src/engine/effective_request.rs",
    '''    Ok(attempts.into_values().collect())\n}\n''',
    '''    let mut attempts = attempts.into_values().collect::<Vec<_>>();\n    attempts.sort_by_key(|attempt| {\n        (\n            attempt["descriptor"]["route_ordinal"].as_u64().unwrap_or(u64::MAX),\n            attempt["descriptor"]["retry_ordinal"].as_u64().unwrap_or(u64::MAX),\n            attempt["descriptor"]["conditional"].as_bool().unwrap_or(false),\n        )\n    });\n    Ok(attempts)\n}\n''',
)

# Engine records named assembly fragments and execution-policy authority.
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''        let summary_request = crate::compaction_v2::semantic_summary_request(session, focus);\n        let manifest = effective_request::persist_before_provider_call(\n''',
    '''        let summary_request = crate::compaction_v2::semantic_summary_request(session, focus);\n        let summary_provenance = BTreeMap::from([\n            (\n                "compaction_v2_system".to_owned(),\n                effective_request::fragment_fingerprint(&summary_request.system),\n            ),\n            (\n                "compaction_v2_focus".to_owned(),\n                effective_request::fragment_fingerprint(focus.unwrap_or_default()),\n            ),\n        ]);\n        let execution_policy = self.execution_policy.audit_projection();\n        let manifest = effective_request::persist_before_provider_call(\n''',
)
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''            &self.provider.capabilities(),\n            None,\n        )?;\n        append_event(\n''',
    '''            &self.provider.capabilities(),\n            &execution_policy,\n            summary_provenance,\n            None,\n        )?;\n        append_event(\n''',
)
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''        let mut system = coding_policy::apply(\n            system_prompt_with_context(\n                self.config.agent.mode,\n                &session.repo,\n                additional_system_context,\n            ),\n            self.config.agent.mode,\n        );\n        if let Some(team) = &self.team_context {\n            system.push_str("\\n\\n");\n            system.push_str(&team.prompt_context()?);\n        }\n        if let Some(branch_context) = crate::branch_summary::advisory_context(session) {\n            system.push_str("\\n\\n");\n            system.push_str(&branch_context);\n        }\n''',
    '''        let mut assembly_provenance = BTreeMap::new();\n        if let Some(context) = additional_system_context.filter(|text| !text.trim().is_empty()) {\n            assembly_provenance.insert(\n                "additional_system_context".to_owned(),\n                effective_request::fragment_fingerprint(context),\n            );\n        }\n        if let Some(instruction) = turn_instruction.filter(|text| !text.trim().is_empty()) {\n            assembly_provenance.insert(\n                "turn_instruction".to_owned(),\n                effective_request::fragment_fingerprint(instruction),\n            );\n        }\n        let mut system = coding_policy::apply(\n            system_prompt_with_context(\n                self.config.agent.mode,\n                &session.repo,\n                additional_system_context,\n            ),\n            self.config.agent.mode,\n        );\n        assembly_provenance.insert(\n            "base_system_projection".to_owned(),\n            effective_request::fragment_fingerprint(&system),\n        );\n        if let Some(team) = &self.team_context {\n            let team_context = team.prompt_context()?;\n            assembly_provenance.insert(\n                "team_context".to_owned(),\n                effective_request::fragment_fingerprint(&team_context),\n            );\n            system.push_str("\\n\\n");\n            system.push_str(&team_context);\n        }\n        if let Some(branch_context) = crate::branch_summary::advisory_context(session) {\n            assembly_provenance.insert(\n                "branch_summary".to_owned(),\n                effective_request::fragment_fingerprint(&branch_context),\n            );\n            system.push_str("\\n\\n");\n            system.push_str(&branch_context);\n        }\n''',
)
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''        )? {\n            system.push_str("\\n\\n");\n            system.push_str(&retrieval.system_fragment);\n            observer(&AgentUpdate::ToolOutput {\n''',
    '''        )? {\n            assembly_provenance.insert(\n                "repository_context".to_owned(),\n                effective_request::fragment_fingerprint(&retrieval.system_fragment),\n            );\n            system.push_str("\\n\\n");\n            system.push_str(&retrieval.system_fragment);\n            observer(&AgentUpdate::ToolOutput {\n''',
)
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''        let mut active_manifest = effective_request::persist_before_provider_call(\n            session,\n            &request,\n            phase,\n            &self.config.model.provider,\n            &self.config.model.name,\n            &self.provider.capabilities(),\n            None,\n        )?;\n''',
    '''        let execution_policy = self.execution_policy.audit_projection();\n        let mut active_manifest = effective_request::persist_before_provider_call(\n            session,\n            &request,\n            phase,\n            &self.config.model.provider,\n            &self.config.model.name,\n            &self.provider.capabilities(),\n            &execution_policy,\n            assembly_provenance.clone(),\n            None,\n        )?;\n''',
)
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''                    &self.provider.capabilities(),\n                    Some(&active_manifest),\n                )?;\n''',
    '''                    &self.provider.capabilities(),\n                    &execution_policy,\n                    assembly_provenance.clone(),\n                    Some(&active_manifest),\n                )?;\n''',
)
# Update stale method doc: instruction is durable in the protected request artifact, not session history.
replace_once(
    "crates/medusa-agent/src/engine.rs",
    '''    /// Executes one model step with ephemeral system context and an optional latest-turn\n    /// instruction. The instruction is sent only in the provider request and is never persisted in\n    /// the durable session history.\n''',
    '''    /// Executes one model step with ephemeral system context and an optional latest-turn\n    /// instruction. It is not appended to conversational session history, but the exact effective\n    /// request is retained through the protected request-manifest authority for audit/replay.\n''',
)

# CLI/support inspection path.
replace_once(
    "crates/medusa-cli/src/main.rs",
    '''use medusa_agent::{bootstrap, session_browser::list_sessions};\n''',
    '''use medusa_agent::{bootstrap, inspect_effective_model_request, session_browser::list_sessions};\n''',
)
replace_once(
    "crates/medusa-cli/src/main.rs",
    '''    Checkpoint {\n        message: String,\n    },\n    Run {\n''',
    '''    Checkpoint {\n        message: String,\n    },\n    /// Verify and inspect one durable effective-model-request manifest.\n    RequestAudit {\n        session: String,\n        manifest: String,\n    },\n    Run {\n''',
)
replace_once(
    "crates/medusa-cli/src/main.rs",
    '''        CommandKind::Checkpoint { message } => checkpoint(&repo, &message),\n        CommandKind::Run {\n''',
    '''        CommandKind::Checkpoint { message } => checkpoint(&repo, &message),\n        CommandKind::RequestAudit { session, manifest } => {\n            let audit = inspect_effective_model_request(&repo, &session, &manifest)?;\n            println!(\n                "{}",\n                serde_json::to_string_pretty(&audit).map_err(|error| MedusaError::new(\n                    ErrorCode::InvalidEvent,\n                    ErrorCategory::Internal,\n                    format!("could not render request audit: {error}"),\n                ))?\n            );\n            Ok(())\n        }\n        CommandKind::Run {\n''',
)

# Expand integration proof: same visible tools + different write scopes produce different request fingerprints,
# and the manifest survives ambient config drift.
p = Path("crates/medusa-agent/tests/effective_request_manifest.rs")
text = p.read_text()
text = text.replace(
    "use medusa_agent::{AgentEngine, StepOutcome, inspect_effective_model_request};",
    "use medusa_agent::{AgentEngine, AgentExecutionPolicy, StepOutcome, inspect_effective_model_request};",
)
if "fn execution_policy_and_ambient_config_are_bound_without_replay_drift()" not in text:
    text += r'''

#[test]
fn execution_policy_and_ambient_config_are_bound_without_replay_drift() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let mut config = Config::default();
    config.agent.mode = Mode::ReadOnly;

    let first_calls = Arc::new(AtomicUsize::new(0));
    let first = AgentEngine::new(
        CountingProvider { calls: Arc::clone(&first_calls) },
        config.clone(),
    )
    .with_execution_policy(
        AgentExecutionPolicy::unrestricted().with_allowed_write_paths(["src".to_owned()]),
    );
    let mut first_session = first
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("first session");
    first.step(&mut first_session).expect("first step");
    let (first_fp, first_manifest) = first_session.events.iter().find_map(|event| match &event.payload {
        EventPayload::ModelRequestStarted {
            request_fingerprint: Some(fp),
            manifest_ref: Some(reference),
            ..
        } => Some((fp.clone(), reference.clone())),
        _ => None,
    }).expect("first manifest");

    let second_calls = Arc::new(AtomicUsize::new(0));
    let second = AgentEngine::new(
        CountingProvider { calls: Arc::clone(&second_calls) },
        config,
    )
    .with_execution_policy(
        AgentExecutionPolicy::unrestricted().with_allowed_write_paths(["tests".to_owned()]),
    );
    let mut second_session = second
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("second session");
    second.step(&mut second_session).expect("second step");
    let second_fp = second_session.events.iter().find_map(|event| match &event.payload {
        EventPayload::ModelRequestStarted { request_fingerprint: Some(fp), .. } => Some(fp.clone()),
        _ => None,
    }).expect("second fingerprint");
    assert_ne!(first_fp, second_fp, "execution policy must affect the stable request fingerprint");

    let audit = inspect_effective_model_request(directory.path(), first_session.id.as_str(), &first_manifest)
        .expect("historical request remains reconstructable");
    assert_eq!(audit["healthy"], serde_json::json!(true));
    assert_eq!(
        audit["execution_policy"]["allowed_write_paths"],
        serde_json::json!(["src"]),
    );
}
'''
p.write_text(text)
