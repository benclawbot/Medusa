use std::{
    cell::RefCell,
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

#[derive(Clone, Debug)]
pub(super) struct AuditSessionContext {
    pub repo: PathBuf,
    pub session_id: String,
}

thread_local! {
    static ACTIVE_CONTEXT: RefCell<Option<AuditSessionContext>> = const { RefCell::new(None) };
}

pub(super) fn with_session_context<T>(session: &AgentSession, operation: impl FnOnce() -> T) -> T {
    let context = AuditSessionContext {
        repo: session.repo.clone(),
        session_id: session.id.to_string(),
    };
    ACTIVE_CONTEXT.with(|slot| {
        let previous = slot.replace(Some(context));
        let result = operation();
        slot.replace(previous);
        result
    })
}

pub(super) fn active_context() -> Option<AuditSessionContext> {
    ACTIVE_CONTEXT.with(|slot| slot.borrow().clone())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ManifestRef {
    pub request_id: String,
    pub request_fingerprint: String,
    pub artifact_ref: String,
    pub started_event_sequence: u64,
    pub attempt_ordinal: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct EffectiveModelRequestManifestV1 {
    schema_version: u16,
    request_id: String,
    session_id: String,
    started_event_sequence: u64,
    preceding_event_sequence: u64,
    turn: u32,
    attempt_ordinal: u32,
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
    canonical_request: Value,
    request_fingerprint: String,
}

#[derive(Serialize)]
struct FingerprintMaterial<'a> {
    schema_version: u16,
    session_id: &'a str,
    started_event_sequence: u64,
    preceding_event_sequence: u64,
    turn: u32,
    attempt_ordinal: u32,
    parent_request_fingerprint: &'a Option<String>,
    execution_phase: ProviderExecutionPhase,
    provider: &'a str,
    model: &'a str,
    provider_profile_fingerprint: &'a str,
    capability_fingerprint: &'a str,
    source_event_sequences: &'a [u64],
    delivered_action_ids: &'a [String],
    compaction_generation: Option<u32>,
    compaction_source_event_sequences: &'a [u64],
    component_fingerprints: &'a BTreeMap<String, String>,
    canonical_request: &'a Value,
}

pub(super) fn persist_before_provider_call(
    context: &AuditSessionContext,
    request: &ModelRequest,
    phase: ProviderExecutionPhase,
    capabilities: &ProviderCapabilities,
    previous: Option<&ManifestRef>,
) -> MedusaResult<ManifestRef> {
    let session = crate::session::load(&context.repo, &context.session_id)?;
    let (started_event_sequence, provider, model) = latest_started_request(&session)?;
    let preceding_event_sequence = started_event_sequence.saturating_sub(1);
    let same_started_boundary = previous
        .is_some_and(|manifest| manifest.started_event_sequence == started_event_sequence);
    let attempt_ordinal = previous
        .filter(|_| same_started_boundary)
        .map_or(0, |manifest| manifest.attempt_ordinal.saturating_add(1));
    let parent_request_fingerprint = previous
        .filter(|_| same_started_boundary)
        .map(|manifest| manifest.request_fingerprint.clone());

    let canonical_request = canonicalize_value(serde_json::to_value(request).map_err(json_error)?);
    let capability_value = canonicalize_value(serde_json::to_value(capabilities).map_err(json_error)?);
    let capability_fingerprint = fingerprint_value(&capability_value)?;
    let provider_profile_fingerprint = fingerprint_value(&json!({
        "provider": provider,
        "model": model,
        "capabilities": capability_value,
    }))?;
    let mut component_fingerprints = BTreeMap::new();
    component_fingerprints.insert("system".to_owned(), fingerprint_value(&json!(request.system))?);
    component_fingerprints.insert(
        "messages".to_owned(),
        fingerprint_value(&canonicalize_value(
            serde_json::to_value(&request.messages).map_err(json_error)?,
        ))?,
    );
    component_fingerprints.insert(
        "tools".to_owned(),
        fingerprint_value(&canonicalize_value(
            serde_json::to_value(&request.tools).map_err(json_error)?,
        ))?,
    );
    component_fingerprints.insert(
        "request_config".to_owned(),
        fingerprint_value(&json!({
            "max_tokens": request.max_tokens,
            "temperature_milli": request.temperature_milli,
        }))?,
    );

    let source_event_sequences = session
        .events
        .iter()
        .filter(|event| event.sequence <= preceding_event_sequence)
        .map(|event| event.sequence)
        .collect::<Vec<_>>();
    let delivered_action_ids = session
        .events
        .iter()
        .filter(|event| event.sequence <= preceding_event_sequence)
        .filter_map(|event| match &event.payload {
            EventPayload::SessionActionTranscriptLinked { action_id, .. } => Some(action_id.clone()),
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

    let material = FingerprintMaterial {
        schema_version: MANIFEST_SCHEMA_VERSION,
        session_id: &context.session_id,
        started_event_sequence,
        preceding_event_sequence,
        turn: session.turn,
        attempt_ordinal,
        parent_request_fingerprint: &parent_request_fingerprint,
        execution_phase: phase,
        provider: &provider,
        model: &model,
        provider_profile_fingerprint: &provider_profile_fingerprint,
        capability_fingerprint: &capability_fingerprint,
        source_event_sequences: &source_event_sequences,
        delivered_action_ids: &delivered_action_ids,
        compaction_generation,
        compaction_source_event_sequences: &compaction_source_event_sequences,
        component_fingerprints: &component_fingerprints,
        canonical_request: &canonical_request,
    };
    let request_fingerprint = sha256(&serde_json::to_vec(&material).map_err(json_error)?);
    let request_id = format!(
        "request-{}-{}-{}",
        started_event_sequence,
        attempt_ordinal,
        &request_fingerprint[..16]
    );
    let manifest = EffectiveModelRequestManifestV1 {
        schema_version: MANIFEST_SCHEMA_VERSION,
        request_id: request_id.clone(),
        session_id: context.session_id.clone(),
        started_event_sequence,
        preceding_event_sequence,
        turn: session.turn,
        attempt_ordinal,
        parent_request_fingerprint,
        execution_phase: phase,
        provider,
        model,
        provider_profile_fingerprint,
        capability_fingerprint,
        source_event_sequences,
        delivered_action_ids,
        compaction_generation,
        compaction_source_event_sequences,
        component_fingerprints,
        canonical_request,
        request_fingerprint: request_fingerprint.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(json_error)?;
    let path = persist_manifest(&context.repo, &context.session_id, &request_fingerprint, &bytes)?;
    Ok(ManifestRef {
        request_id,
        request_fingerprint,
        artifact_ref: path.display().to_string(),
        started_event_sequence,
        attempt_ordinal,
    })
}

pub(super) fn augment_execution_status(status: Option<Value>, manifest: &ManifestRef) -> Value {
    let mut status = status.unwrap_or_else(|| json!({}));
    if !status.is_object() {
        status = json!({"provider_status": status});
    }
    if let Some(object) = status.as_object_mut() {
        object.insert(
            "effective_model_request".to_owned(),
            serde_json::to_value(manifest).unwrap_or_else(|_| json!({
                "request_id": manifest.request_id,
                "request_fingerprint": manifest.request_fingerprint,
                "artifact_ref": manifest.artifact_ref,
            })),
        );
    }
    status
}

pub(super) fn inspect(
    repo: &Path,
    session_id: &str,
    request_fingerprint: &str,
) -> MedusaResult<Value> {
    let path = resolve_manifest_path(repo, session_id, request_fingerprint)?;
    let bytes = fs::read(&path)?;
    let manifest: EffectiveModelRequestManifestV1 =
        serde_json::from_slice(&bytes).map_err(json_error)?;
    let material = FingerprintMaterial {
        schema_version: manifest.schema_version,
        session_id: &manifest.session_id,
        started_event_sequence: manifest.started_event_sequence,
        preceding_event_sequence: manifest.preceding_event_sequence,
        turn: manifest.turn,
        attempt_ordinal: manifest.attempt_ordinal,
        parent_request_fingerprint: &manifest.parent_request_fingerprint,
        execution_phase: manifest.execution_phase,
        provider: &manifest.provider,
        model: &manifest.model,
        provider_profile_fingerprint: &manifest.provider_profile_fingerprint,
        capability_fingerprint: &manifest.capability_fingerprint,
        source_event_sequences: &manifest.source_event_sequences,
        delivered_action_ids: &manifest.delivered_action_ids,
        compaction_generation: manifest.compaction_generation,
        compaction_source_event_sequences: &manifest.compaction_source_event_sequences,
        component_fingerprints: &manifest.component_fingerprints,
        canonical_request: &manifest.canonical_request,
    };
    let reconstructed = sha256(&serde_json::to_vec(&material).map_err(json_error)?);
    if reconstructed != manifest.request_fingerprint || reconstructed != request_fingerprint {
        return Err(audit_error(format!(
            "effective model request manifest fingerprint mismatch: recorded={}, reconstructed={reconstructed}, requested={request_fingerprint}",
            manifest.request_fingerprint
        )));
    }
    Ok(json!({
        "healthy": true,
        "request_id": manifest.request_id,
        "request_fingerprint": manifest.request_fingerprint,
        "artifact_ref": path,
        "session_id": manifest.session_id,
        "started_event_sequence": manifest.started_event_sequence,
        "preceding_event_sequence": manifest.preceding_event_sequence,
        "turn": manifest.turn,
        "attempt_ordinal": manifest.attempt_ordinal,
        "parent_request_fingerprint": manifest.parent_request_fingerprint,
        "execution_phase": manifest.execution_phase,
        "provider": manifest.provider,
        "model": manifest.model,
        "component_fingerprints": manifest.component_fingerprints,
        "delivered_action_ids": manifest.delivered_action_ids,
        "compaction_generation": manifest.compaction_generation,
        "compaction_source_event_sequences": manifest.compaction_source_event_sequences,
        "canonical_request": manifest.canonical_request,
    }))
}

fn latest_started_request(session: &AgentSession) -> MedusaResult<(u64, String, String)> {
    session
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            EventPayload::ModelRequestStarted { provider, model } => {
                Some((event.sequence, provider.clone(), model.clone()))
            }
            _ => None,
        })
        .ok_or_else(|| audit_error("provider execution has no durable ModelRequestStarted boundary"))
}

fn persist_manifest(
    repo: &Path,
    session_id: &str,
    fingerprint: &str,
    bytes: &[u8],
) -> MedusaResult<PathBuf> {
    let primary = repo
        .join(".medusa/request-manifests")
        .join(session_id)
        .join(format!("{fingerprint}.json"));
    match persist_at(&primary, bytes) {
        Ok(()) => Ok(primary),
        Err(_) => {
            let fallback = crate::session::fallback_storage_root(repo, "request-manifests")
                .join(session_id)
                .join(format!("{fingerprint}.json"));
            persist_at(&fallback, bytes)?;
            Ok(fallback)
        }
    }
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
            "immutable effective request manifest already exists with different content: {}",
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

fn resolve_manifest_path(repo: &Path, session_id: &str, fingerprint: &str) -> MedusaResult<PathBuf> {
    let primary = repo
        .join(".medusa/request-manifests")
        .join(session_id)
        .join(format!("{fingerprint}.json"));
    if primary.is_file() {
        return Ok(primary);
    }
    let fallback = crate::session::fallback_storage_root(repo, "request-manifests")
        .join(session_id)
        .join(format!("{fingerprint}.json"));
    if fallback.is_file() {
        return Ok(fallback);
    }
    Err(audit_error(format!(
        "effective model request manifest is unavailable for fingerprint {fingerprint}"
    )))
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
    let canonical = canonicalize_value(value.clone());
    Ok(sha256(&serde_json::to_vec(&canonical).map_err(json_error)?))
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
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        format!("effective model request manifest is not serializable: {error}"),
    )
}

fn audit_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Persistence,
        message.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_ignores_object_insertion_order() {
        let left = json!({"z": 1, "a": {"y": 2, "b": 3}});
        let right = json!({"a": {"b": 3, "y": 2}, "z": 1});
        assert_eq!(
            fingerprint_value(&left).expect("left fingerprint"),
            fingerprint_value(&right).expect("right fingerprint")
        );
    }

    #[test]
    fn canonical_request_fingerprint_changes_with_model_visible_config() {
        let base = json!({
            "system": "system",
            "messages": [],
            "tools": [],
            "max_tokens": 1024,
            "temperature_milli": 200,
        });
        let changed = json!({
            "system": "system",
            "messages": [],
            "tools": [],
            "max_tokens": 2048,
            "temperature_milli": 200,
        });
        assert_ne!(
            fingerprint_value(&base).expect("base fingerprint"),
            fingerprint_value(&changed).expect("changed fingerprint")
        );
    }

    #[test]
    fn canonical_tool_schema_is_stable_across_map_order() {
        let left = json!({
            "name": "search_text",
            "input_schema": {"properties": {"query": {"type": "string"}}, "type": "object"}
        });
        let right = json!({
            "input_schema": {"type": "object", "properties": {"query": {"type": "string"}}},
            "name": "search_text"
        });
        assert_eq!(
            fingerprint_value(&left).expect("left fingerprint"),
            fingerprint_value(&right).expect("right fingerprint")
        );
    }
}
