use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_protocol::EventPayload;
use medusa_provider::{ModelRequest, ProviderCapabilities, ProviderExecutionPhase};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::session::AgentSession;

const MANIFEST_SCHEMA_VERSION: u16 = 1;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ManifestRef {
    pub request_id: String,
    pub request_fingerprint: String,
    pub manifest_ref: String,
    pub attempt_ordinal: u32,
    pub parent_request_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct EffectiveModelRequestManifestV1 {
    schema_version: u16,
    manifest_fingerprint: String,
    request_id: String,
    request_fingerprint: String,
    request_content_fingerprint: String,
    request_content_ref: String,
    session_id: String,
    started_event_sequence: u64,
    preceding_event_sequence: u64,
    turn: u32,
    attempt_ordinal: u32,
    parent_request_id: Option<String>,
    parent_request_fingerprint: Option<String>,
    execution_phase: ProviderExecutionPhase,
    provider: String,
    model: String,
    provider_profile_fingerprint: String,
    capability_fingerprint: String,
    source_event_sequences: Vec<u64>,
    delivered_action_ids: Vec<String>,
    compaction_generation: Option<u32>,
    compaction_source_event_sequences: Vec<u64>,
    component_fingerprints: BTreeMap<String, String>,
    tool_schema_fingerprints: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct RequestFingerprintMaterial<'a> {
    schema_version: u16,
    execution_phase: ProviderExecutionPhase,
    provider: &'a str,
    model: &'a str,
    provider_profile_fingerprint: &'a str,
    capability_fingerprint: &'a str,
    request_content_fingerprint: &'a str,
    component_fingerprints: &'a BTreeMap<String, String>,
    tool_schema_fingerprints: &'a BTreeMap<String, String>,
}

#[derive(Serialize)]
struct ManifestFingerprintMaterial<'a> {
    schema_version: u16,
    request_id: &'a str,
    request_fingerprint: &'a str,
    session_id: &'a str,
    started_event_sequence: u64,
    preceding_event_sequence: u64,
    turn: u32,
    attempt_ordinal: u32,
    parent_request_id: &'a Option<String>,
    parent_request_fingerprint: &'a Option<String>,
    source_event_sequences: &'a [u64],
    delivered_action_ids: &'a [String],
    compaction_generation: Option<u32>,
    compaction_source_event_sequences: &'a [u64],
}

pub(crate) fn persist_before_provider_call(
    session: &AgentSession,
    request: &ModelRequest,
    phase: ProviderExecutionPhase,
    provider: &str,
    model: &str,
    capabilities: &ProviderCapabilities,
    previous: Option<&ManifestRef>,
) -> MedusaResult<ManifestRef> {
    let preceding_event_sequence = session.events.last().map_or(0, |event| event.sequence);
    let started_event_sequence = preceding_event_sequence.saturating_add(1);
    let attempt_ordinal = previous.map_or(0, |manifest| manifest.attempt_ordinal.saturating_add(1));
    let parent_request_id = previous.map(|manifest| manifest.request_id.clone());
    let parent_request_fingerprint = previous.map(|manifest| manifest.request_fingerprint.clone());

    let canonical_request = canonicalize_value(serde_json::to_value(request).map_err(json_error)?);
    let request_content_fingerprint = fingerprint_value(&canonical_request)?;
    let request_content_ref = logical_ref("request-content", &request_content_fingerprint);
    persist_immutable(
        &session.repo,
        "request-artifacts",
        session.id.as_str(),
        &request_content_fingerprint,
        &serde_json::to_vec_pretty(&canonical_request).map_err(json_error)?,
    )?;

    let capability_value =
        canonicalize_value(serde_json::to_value(capabilities).map_err(json_error)?);
    let capability_fingerprint = fingerprint_value(&capability_value)?;
    let provider_profile_fingerprint = fingerprint_value(&canonicalize_value(json!({
        "provider": provider,
        "model": model,
        "capabilities": capability_value,
    })))?;
    let component_fingerprints = request_component_fingerprints(request)?;
    let tool_schema_fingerprints = request
        .tools
        .iter()
        .map(|tool| {
            Ok((
                tool.name.clone(),
                fingerprint_value(&canonicalize_value(tool.input_schema.clone()))?,
            ))
        })
        .collect::<MedusaResult<BTreeMap<_, _>>>()?;

    let previous_started_sequence = session
        .events
        .iter()
        .rev()
        .find_map(|event| {
            matches!(event.payload, EventPayload::ModelRequestStarted { .. })
                .then_some(event.sequence)
        })
        .unwrap_or(0);
    let source_event_sequences = session
        .events
        .iter()
        .filter(|event| event.sequence <= preceding_event_sequence)
        .map(|event| event.sequence)
        .collect::<Vec<_>>();
    let delivered_action_ids = session
        .events
        .iter()
        .filter(|event| {
            event.sequence > previous_started_sequence && event.sequence <= preceding_event_sequence
        })
        .filter_map(|event| match &event.payload {
            EventPayload::SessionActionTranscriptLinked { action_id, .. } => {
                Some(action_id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let (compaction_generation, compaction_source_event_sequences) = session
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            EventPayload::ConversationCompacted {
                generation,
                source_event_sequences,
                ..
            } if event.sequence <= preceding_event_sequence => {
                Some((Some(*generation), source_event_sequences.clone()))
            }
            _ => None,
        })
        .unwrap_or_default();

    let request_material = RequestFingerprintMaterial {
        schema_version: MANIFEST_SCHEMA_VERSION,
        execution_phase: phase,
        provider,
        model,
        provider_profile_fingerprint: &provider_profile_fingerprint,
        capability_fingerprint: &capability_fingerprint,
        request_content_fingerprint: &request_content_fingerprint,
        component_fingerprints: &component_fingerprints,
        tool_schema_fingerprints: &tool_schema_fingerprints,
    };
    let request_fingerprint = sha256(&serde_json::to_vec(&request_material).map_err(json_error)?);
    let request_id = format!(
        "{}:{}:{}",
        session.id, started_event_sequence, attempt_ordinal,
    );
    let manifest_material = ManifestFingerprintMaterial {
        schema_version: MANIFEST_SCHEMA_VERSION,
        request_id: &request_id,
        request_fingerprint: &request_fingerprint,
        session_id: session.id.as_str(),
        started_event_sequence,
        preceding_event_sequence,
        turn: session.turn,
        attempt_ordinal,
        parent_request_id: &parent_request_id,
        parent_request_fingerprint: &parent_request_fingerprint,
        source_event_sequences: &source_event_sequences,
        delivered_action_ids: &delivered_action_ids,
        compaction_generation,
        compaction_source_event_sequences: &compaction_source_event_sequences,
    };
    let manifest_fingerprint = sha256(&serde_json::to_vec(&manifest_material).map_err(json_error)?);
    let manifest_ref = logical_ref("request-manifest", &manifest_fingerprint);
    let manifest = EffectiveModelRequestManifestV1 {
        schema_version: MANIFEST_SCHEMA_VERSION,
        manifest_fingerprint: manifest_fingerprint.clone(),
        request_id: request_id.clone(),
        request_fingerprint: request_fingerprint.clone(),
        request_content_fingerprint,
        request_content_ref,
        session_id: session.id.to_string(),
        started_event_sequence,
        preceding_event_sequence,
        turn: session.turn,
        attempt_ordinal,
        parent_request_id: parent_request_id.clone(),
        parent_request_fingerprint,
        execution_phase: phase,
        provider: provider.to_owned(),
        model: model.to_owned(),
        provider_profile_fingerprint,
        capability_fingerprint,
        source_event_sequences,
        delivered_action_ids,
        compaction_generation,
        compaction_source_event_sequences,
        component_fingerprints,
        tool_schema_fingerprints,
    };
    persist_immutable(
        &session.repo,
        "request-manifests",
        session.id.as_str(),
        &manifest_fingerprint,
        &serde_json::to_vec_pretty(&manifest).map_err(json_error)?,
    )?;
    Ok(ManifestRef {
        request_id,
        request_fingerprint,
        manifest_ref,
        attempt_ordinal,
        parent_request_id,
    })
}

pub(crate) fn started_event(manifest: &ManifestRef, provider: &str, model: &str) -> EventPayload {
    EventPayload::ModelRequestStarted {
        provider: provider.to_owned(),
        model: model.to_owned(),
        request_id: Some(manifest.request_id.clone()),
        request_fingerprint: Some(manifest.request_fingerprint.clone()),
        manifest_ref: Some(manifest.manifest_ref.clone()),
        attempt_ordinal: manifest.attempt_ordinal,
        parent_request_id: manifest.parent_request_id.clone(),
    }
}

pub(crate) fn response_event(
    manifest: &ManifestRef,
    response_id: Option<String>,
    usage: Value,
) -> EventPayload {
    EventPayload::ModelResponseReceived {
        response_id,
        usage,
        request_id: Some(manifest.request_id.clone()),
        request_fingerprint: Some(manifest.request_fingerprint.clone()),
    }
}

pub(crate) fn failure_event(manifest: &ManifestRef, error: &MedusaError) -> EventPayload {
    EventPayload::ModelRequestFailed {
        request_id: manifest.request_id.clone(),
        request_fingerprint: manifest.request_fingerprint.clone(),
        error: error.to_string(),
    }
}

pub(crate) fn augment_execution_status(status: Option<Value>, manifest: &ManifestRef) -> Value {
    let mut status = status.unwrap_or_else(|| json!({}));
    if !status.is_object() {
        status = json!({"provider_status": status});
    }
    if let Some(object) = status.as_object_mut() {
        object.insert(
            "effective_model_request".to_owned(),
            serde_json::to_value(manifest).unwrap_or_else(|_| {
                json!({
                    "request_id": manifest.request_id,
                    "request_fingerprint": manifest.request_fingerprint,
                    "manifest_ref": manifest.manifest_ref,
                })
            }),
        );
    }
    status
}

pub(crate) fn inspect(repo: &Path, session_id: &str, manifest_ref: &str) -> MedusaResult<Value> {
    let manifest_fingerprint = parse_logical_ref("request-manifest", manifest_ref)?;
    let manifest_bytes =
        read_immutable(repo, "request-manifests", session_id, manifest_fingerprint)?;
    let manifest: EffectiveModelRequestManifestV1 =
        serde_json::from_slice(&manifest_bytes).map_err(json_error)?;
    if manifest.manifest_fingerprint != manifest_fingerprint {
        return Err(audit_error(format!(
            "request manifest identity mismatch: requested={manifest_fingerprint}, recorded={}",
            manifest.manifest_fingerprint
        )));
    }
    let content_fingerprint = parse_logical_ref("request-content", &manifest.request_content_ref)?;
    let content_bytes = read_immutable(repo, "request-artifacts", session_id, content_fingerprint)
        .map_err(|_| {
            audit_error(format!(
                "protected request artifact is unavailable or redacted: {}",
                manifest.request_content_ref
            ))
        })?;
    let canonical_request: Value = serde_json::from_slice(&content_bytes).map_err(json_error)?;
    let reconstructed_content_fingerprint = fingerprint_value(&canonical_request)?;
    let reconstructed_components = component_fingerprints_from_value(&canonical_request)?;
    let mismatches =
        component_mismatches(&manifest.component_fingerprints, &reconstructed_components);
    if reconstructed_content_fingerprint != manifest.request_content_fingerprint
        || !mismatches.is_empty()
    {
        let mut error =
            audit_error("effective model request content does not match its durable manifest");
        error
            .context
            .insert("mismatched_components".to_owned(), json!(mismatches));
        error.context.insert(
            "recorded_content_fingerprint".to_owned(),
            json!(manifest.request_content_fingerprint),
        );
        error.context.insert(
            "reconstructed_content_fingerprint".to_owned(),
            json!(reconstructed_content_fingerprint),
        );
        return Err(error);
    }
    let request_material = RequestFingerprintMaterial {
        schema_version: manifest.schema_version,
        execution_phase: manifest.execution_phase,
        provider: &manifest.provider,
        model: &manifest.model,
        provider_profile_fingerprint: &manifest.provider_profile_fingerprint,
        capability_fingerprint: &manifest.capability_fingerprint,
        request_content_fingerprint: &manifest.request_content_fingerprint,
        component_fingerprints: &manifest.component_fingerprints,
        tool_schema_fingerprints: &manifest.tool_schema_fingerprints,
    };
    let reconstructed_request_fingerprint =
        sha256(&serde_json::to_vec(&request_material).map_err(json_error)?);
    let manifest_material = ManifestFingerprintMaterial {
        schema_version: manifest.schema_version,
        request_id: &manifest.request_id,
        request_fingerprint: &manifest.request_fingerprint,
        session_id: &manifest.session_id,
        started_event_sequence: manifest.started_event_sequence,
        preceding_event_sequence: manifest.preceding_event_sequence,
        turn: manifest.turn,
        attempt_ordinal: manifest.attempt_ordinal,
        parent_request_id: &manifest.parent_request_id,
        parent_request_fingerprint: &manifest.parent_request_fingerprint,
        source_event_sequences: &manifest.source_event_sequences,
        delivered_action_ids: &manifest.delivered_action_ids,
        compaction_generation: manifest.compaction_generation,
        compaction_source_event_sequences: &manifest.compaction_source_event_sequences,
    };
    let reconstructed_manifest_fingerprint =
        sha256(&serde_json::to_vec(&manifest_material).map_err(json_error)?);
    if reconstructed_request_fingerprint != manifest.request_fingerprint
        || reconstructed_manifest_fingerprint != manifest.manifest_fingerprint
    {
        let mut error = audit_error("effective model request manifest fingerprint mismatch");
        error.context.insert(
            "recorded_request_fingerprint".to_owned(),
            json!(manifest.request_fingerprint),
        );
        error.context.insert(
            "reconstructed_request_fingerprint".to_owned(),
            json!(reconstructed_request_fingerprint),
        );
        error.context.insert(
            "recorded_manifest_fingerprint".to_owned(),
            json!(manifest.manifest_fingerprint),
        );
        error.context.insert(
            "reconstructed_manifest_fingerprint".to_owned(),
            json!(reconstructed_manifest_fingerprint),
        );
        return Err(error);
    }
    Ok(json!({
        "healthy": true,
        "schema_version": manifest.schema_version,
        "manifest_ref": logical_ref("request-manifest", &manifest.manifest_fingerprint),
        "request_id": manifest.request_id,
        "request_fingerprint": manifest.request_fingerprint,
        "request_content_ref": manifest.request_content_ref,
        "session_id": manifest.session_id,
        "preceding_event_sequence": manifest.preceding_event_sequence,
        "started_event_sequence": manifest.started_event_sequence,
        "turn": manifest.turn,
        "attempt_ordinal": manifest.attempt_ordinal,
        "parent_request_id": manifest.parent_request_id,
        "parent_request_fingerprint": manifest.parent_request_fingerprint,
        "execution_phase": manifest.execution_phase,
        "provider": manifest.provider,
        "model": manifest.model,
        "component_fingerprints": manifest.component_fingerprints,
        "tool_schema_fingerprints": manifest.tool_schema_fingerprints,
        "delivered_action_ids": manifest.delivered_action_ids,
        "compaction_generation": manifest.compaction_generation,
        "compaction_source_event_sequences": manifest.compaction_source_event_sequences,
        "canonical_request": canonical_request,
    }))
}

fn request_component_fingerprints(
    request: &ModelRequest,
) -> MedusaResult<BTreeMap<String, String>> {
    Ok(BTreeMap::from([
        (
            "system".to_owned(),
            fingerprint_value(&json!(request.system))?,
        ),
        (
            "messages".to_owned(),
            fingerprint_value(&canonicalize_value(
                serde_json::to_value(&request.messages).map_err(json_error)?,
            ))?,
        ),
        (
            "tools".to_owned(),
            fingerprint_value(&canonicalize_value(
                serde_json::to_value(&request.tools).map_err(json_error)?,
            ))?,
        ),
        (
            "request_config".to_owned(),
            fingerprint_value(&canonicalize_value(json!({
                "max_tokens": request.max_tokens,
                "temperature_milli": request.temperature_milli,
            })))?,
        ),
    ]))
}

fn component_fingerprints_from_value(request: &Value) -> MedusaResult<BTreeMap<String, String>> {
    let object = request
        .as_object()
        .ok_or_else(|| audit_error("protected request artifact must be a JSON object"))?;
    let config = canonicalize_value(json!({
        "max_tokens": object.get("max_tokens").cloned().unwrap_or(Value::Null),
        "temperature_milli": object.get("temperature_milli").cloned().unwrap_or(Value::Null),
    }));
    Ok(BTreeMap::from([
        (
            "system".to_owned(),
            fingerprint_value(object.get("system").unwrap_or(&Value::Null))?,
        ),
        (
            "messages".to_owned(),
            fingerprint_value(object.get("messages").unwrap_or(&Value::Null))?,
        ),
        (
            "tools".to_owned(),
            fingerprint_value(object.get("tools").unwrap_or(&Value::Null))?,
        ),
        ("request_config".to_owned(), fingerprint_value(&config)?),
    ]))
}

fn component_mismatches(
    recorded: &BTreeMap<String, String>,
    reconstructed: &BTreeMap<String, String>,
) -> Vec<String> {
    recorded
        .iter()
        .filter_map(|(name, fingerprint)| {
            (reconstructed.get(name) != Some(fingerprint)).then_some(name.clone())
        })
        .collect()
}

fn persist_immutable(
    repo: &Path,
    category: &str,
    session_id: &str,
    key: &str,
    bytes: &[u8],
) -> MedusaResult<PathBuf> {
    let primary = repo
        .join(".medusa")
        .join(category)
        .join(session_id)
        .join(format!("{key}.json"));
    match persist_at(&primary, bytes) {
        Ok(()) => Ok(primary),
        Err(_) => {
            let fallback = crate::session::fallback_storage_root(repo, category)
                .join(session_id)
                .join(format!("{key}.json"));
            persist_at(&fallback, bytes)?;
            Ok(fallback)
        }
    }
}

fn read_immutable(
    repo: &Path,
    category: &str,
    session_id: &str,
    key: &str,
) -> MedusaResult<Vec<u8>> {
    let primary = repo
        .join(".medusa")
        .join(category)
        .join(session_id)
        .join(format!("{key}.json"));
    if primary.is_file() {
        return fs::read(primary).map_err(Into::into);
    }
    let fallback = crate::session::fallback_storage_root(repo, category)
        .join(session_id)
        .join(format!("{key}.json"));
    if fallback.is_file() {
        return fs::read(fallback).map_err(Into::into);
    }
    Err(audit_error(format!(
        "immutable {category} artifact {key} is unavailable"
    )))
}

fn persist_at(path: &Path, bytes: &[u8]) -> MedusaResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.is_file() {
        let existing = fs::read(path)?;
        if existing == bytes {
            return Ok(());
        }
        return Err(audit_error(format!(
            "immutable request artifact already exists with different content: {}",
            path.display()
        )));
    }
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_extension(format!("json.tmp.{}.{}", process::id(), sequence));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        sync_parent(path);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn canonicalize_value(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(canonicalize_value).collect()),
        Value::Object(object) => {
            let sorted = object
                .into_iter()
                .map(|(key, value)| (key, canonicalize_value(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(Map::from_iter(sorted))
        }
        scalar => scalar,
    }
}

fn fingerprint_value(value: &Value) -> MedusaResult<String> {
    Ok(sha256(
        &serde_json::to_vec(&canonicalize_value(value.clone())).map_err(json_error)?,
    ))
}

fn logical_ref(kind: &str, fingerprint: &str) -> String {
    format!("{kind}:sha256:{fingerprint}")
}

fn parse_logical_ref<'a>(kind: &str, reference: &'a str) -> MedusaResult<&'a str> {
    reference
        .strip_prefix(&format!("{kind}:sha256:"))
        .filter(|fingerprint| {
            fingerprint.len() == 64 && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .ok_or_else(|| audit_error(format!("invalid {kind} reference: {reference}")))
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent()
        && let Ok(directory) = fs::File::open(parent)
    {
        let _ = directory.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn json_error(error: serde_json::Error) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidEvent,
        ErrorCategory::Persistence,
        format!("could not serialize effective model request authority: {error}"),
    )
}

fn audit_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidEvent,
        ErrorCategory::Persistence,
        message.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_provider::{Message, MessageBlock, Role, ToolDefinition};

    fn request() -> ModelRequest {
        ModelRequest {
            system: "system".to_owned(),
            messages: vec![Message {
                role: Role::User,
                content: vec![MessageBlock::Text {
                    text: "hello".to_owned(),
                }],
            }],
            tools: vec![ToolDefinition {
                name: "read".to_owned(),
                description: "read a file".to_owned(),
                input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
            }],
            max_tokens: 1024,
            temperature_milli: 200,
        }
    }

    #[test]
    fn canonical_json_is_deterministic_for_object_key_order() {
        let left = canonicalize_value(json!({"b": 2, "a": {"d": 4, "c": 3}}));
        let right = canonicalize_value(json!({"a": {"c": 3, "d": 4}, "b": 2}));
        assert_eq!(
            fingerprint_value(&left).unwrap(),
            fingerprint_value(&right).unwrap()
        );
    }

    #[test]
    fn component_fingerprints_attribute_visible_changes() {
        let first = request_component_fingerprints(&request()).unwrap();
        let mut changed = request();
        changed.system.push_str(" changed");
        let second = request_component_fingerprints(&changed).unwrap();
        assert_ne!(first["system"], second["system"]);
        assert_eq!(first["messages"], second["messages"]);
        assert_eq!(first["tools"], second["tools"]);
        assert_eq!(first["request_config"], second["request_config"]);
    }

    #[test]
    fn logical_refs_reject_non_hash_paths() {
        assert!(parse_logical_ref("request-manifest", "../../secret").is_err());
        let hash = "a".repeat(64);
        assert_eq!(
            parse_logical_ref("request-manifest", &logical_ref("request-manifest", &hash)).unwrap(),
            hash,
        );
    }
}
