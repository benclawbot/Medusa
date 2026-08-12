//! Durable trigger-to-session-action dispatch.
//!
//! Trigger producers (timer, heartbeat, file, process, external signal) retain authority over
//! when an occurrence exists. This module owns only the crash-safe translation from that
//! occurrence into the canonical `SessionAction` plane. It intentionally has no prompt queue or
//! transcript authority of its own.

use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use medusa_protocol::{
    SessionActionDeliveryPolicy, SessionActionKind, SessionActionLifecycle, SessionActionWakePolicy,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::{
    RuntimeController, RuntimeError,
    frontend::{SessionActionAdmission, SessionActionRequest, session_action_snapshot},
};

const DISPATCH_FORMAT_VERSION: u16 = 1;
const MIN_RECURRENCE_SECONDS: u64 = 60;
const MAX_CATCH_UP_OCCURRENCES: usize = 8;

/// Trigger family. Trigger implementations remain outside the session action plane.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerSourceKind {
    Timer,
    Heartbeat,
    File,
    Process,
    ExternalSignal,
}

/// Explicit busy-session delivery semantics stored with every occurrence.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerDeliveryMode {
    Steer,
    FollowUp,
}

/// Bounded treatment of intervals that elapsed while the runtime was unavailable.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MissedRunPolicy {
    Skip,
    Coalesce,
    CatchUp,
}

/// Durable state of trigger-to-action translation. This is provenance, not a delivery queue.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerDispatchStatus {
    Claimed,
    ActionAccepted,
    Skipped,
}

/// One trigger occurrence eligible for dispatch into a durable session action.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerDispatchRequest {
    pub schedule_id: String,
    pub source_kind: TriggerSourceKind,
    pub occurrence_id: String,
    pub occurrence_sequence: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub scheduled_for: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    pub target_session_id: String,
    pub delivery_mode: TriggerDeliveryMode,
    pub wake_policy: SessionActionWakePolicy,
    pub prompt: String,
    /// Registration-time ownership/trust-domain decision. Dispatch fails closed when false.
    pub authorized: bool,
    /// A disabled schedule may not create new actions. Existing accepted actions are untouched.
    pub enabled: bool,
    /// Required for schedules intended to wake an otherwise completed/dormant session repeatedly.
    pub persistent_goal: bool,
    pub recurrence_seconds: Option<u64>,
    pub missed_run_policy: MissedRunPolicy,
}

/// Crash-safe trigger provenance linked to the resulting canonical action.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerDispatchRecord {
    pub format_version: u16,
    pub schedule_id: String,
    pub source_kind: TriggerSourceKind,
    pub occurrence_id: String,
    pub occurrence_sequence: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub scheduled_for: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    pub target_session_id: String,
    pub idempotency_key: String,
    pub action_id: String,
    pub delivery_mode: TriggerDeliveryMode,
    pub delivery_policy: SessionActionDeliveryPolicy,
    pub wake_policy: SessionActionWakePolicy,
    pub prompt: String,
    pub authorized: bool,
    pub enabled: bool,
    pub persistent_goal: bool,
    pub recurrence_seconds: Option<u64>,
    pub missed_run_policy: MissedRunPolicy,
    pub status: TriggerDispatchStatus,
    pub recovered: bool,
    pub skip_reason: Option<String>,
    pub action_lifecycle: Option<SessionActionLifecycle>,
    pub action_accepted_sequence: Option<u64>,
    pub content_hash: String,
}

impl TriggerDispatchRecord {
    fn verify(&self) -> Result<(), RuntimeError> {
        if self.format_version != DISPATCH_FORMAT_VERSION {
            return Err(RuntimeError::InvalidCommand(
                "unsupported trigger dispatch record version".to_owned(),
            ));
        }
        if self.content_hash != record_hash(self)? {
            return Err(RuntimeError::InvalidCommand(
                "trigger dispatch record failed content verification".to_owned(),
            ));
        }
        Ok(())
    }
}

impl RuntimeController {
    /// Durably claims and dispatches one trigger occurrence through the canonical action plane.
    /// Replaying the same occurrence is idempotent, including after a crash between action append
    /// and dispatch-receipt completion.
    pub fn dispatch_trigger_action(
        &self,
        request: TriggerDispatchRequest,
    ) -> Result<TriggerDispatchRecord, RuntimeError> {
        validate_request(&request)?;
        let path = dispatch_path(&self.repo, &request)?;
        if path.exists() {
            let existing = load_record(&path)?;
            existing.verify()?;
            ensure_same_occurrence(&existing, &request)?;
            if existing.status == TriggerDispatchStatus::ActionAccepted
                || existing.status == TriggerDispatchStatus::Skipped
            {
                return Ok(existing);
            }
            return self.finish_claimed_trigger(request, existing, path, true);
        }

        let mut record = claimed_record(&request)?;
        if !request.enabled {
            record.status = TriggerDispatchStatus::Skipped;
            record.skip_reason = Some("schedule_disabled".to_owned());
            seal_record(&mut record)?;
            persist_record(&path, &record)?;
            return Ok(record);
        }
        if !request.authorized {
            record.status = TriggerDispatchStatus::Skipped;
            record.skip_reason = Some("registration_not_authorized".to_owned());
            seal_record(&mut record)?;
            persist_record(&path, &record)?;
            return Ok(record);
        }

        persist_record(&path, &record)?;
        self.finish_claimed_trigger(request, record, path, false)
    }

    /// Recovers incomplete trigger claims for the active session. Deterministic occurrence
    /// identities make replay coalesce with an already-appended action rather than enqueue twice.
    pub fn recover_trigger_dispatches(&self) -> Result<Vec<TriggerDispatchRecord>, RuntimeError> {
        let Some(session_id) = self.active_session_id() else {
            return Ok(Vec::new());
        };
        let directory = dispatch_directory(&self.repo, &session_id);
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut recovered = Vec::new();
        for entry in fs::read_dir(directory).map_err(RuntimeError::agent)? {
            let entry = entry.map_err(RuntimeError::agent)?;
            if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let record = load_record(&entry.path())?;
            record.verify()?;
            if record.status != TriggerDispatchStatus::Claimed {
                continue;
            }
            let request = request_from_record(&record)?;
            recovered.push(self.finish_claimed_trigger(request, record, entry.path(), true)?);
        }
        Ok(recovered)
    }

    fn finish_claimed_trigger(
        &self,
        request: TriggerDispatchRequest,
        mut record: TriggerDispatchRecord,
        path: PathBuf,
        recovered: bool,
    ) -> Result<TriggerDispatchRecord, RuntimeError> {
        let snapshot = session_action_snapshot(&self.repo, &request.target_session_id)?;
        let action_request = SessionActionRequest {
            idempotency_key: record.idempotency_key.clone(),
            source: format!(
                "trigger:{:?}:{}:{}",
                request.source_kind, request.schedule_id, request.occurrence_sequence
            )
            .to_ascii_lowercase(),
            target_session_id: request.target_session_id.clone(),
            expected_session_revision: snapshot.revision,
            kind: match request.delivery_mode {
                TriggerDeliveryMode::Steer => SessionActionKind::Steer,
                TriggerDeliveryMode::FollowUp => SessionActionKind::FollowUp,
            },
            delivery_policy: record.delivery_policy,
            wake_policy: request.wake_policy,
            payload: json!({
                "text": request.prompt,
                "trigger_provenance": {
                    "schedule_id": request.schedule_id,
                    "source_kind": request.source_kind,
                    "occurrence_id": request.occurrence_id,
                    "occurrence_sequence": request.occurrence_sequence,
                    "scheduled_for": request.scheduled_for,
                    "observed_at": request.observed_at,
                    "missed_run_policy": request.missed_run_policy,
                }
            }),
        };
        let admission: SessionActionAdmission = self.submit_session_action(action_request)?;
        record.action_id = admission.action.action.action_id.clone();
        record.status = TriggerDispatchStatus::ActionAccepted;
        record.recovered |= recovered;
        record.action_lifecycle = Some(admission.action.lifecycle);
        record.action_accepted_sequence = Some(admission.action.accepted_sequence);
        record.skip_reason = None;
        seal_record(&mut record)?;
        persist_record(&path, &record)?;
        Ok(record)
    }
}

/// Applies the configured missed-run policy without allowing an unbounded replay backlog.
#[must_use]
pub fn select_missed_occurrences<T: Clone>(policy: MissedRunPolicy, missed: &[T]) -> Vec<T> {
    match policy {
        MissedRunPolicy::Skip => Vec::new(),
        MissedRunPolicy::Coalesce => missed.last().cloned().into_iter().collect(),
        MissedRunPolicy::CatchUp => missed
            .iter()
            .rev()
            .take(MAX_CATCH_UP_OCCURRENCES)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
    }
}

fn validate_request(request: &TriggerDispatchRequest) -> Result<(), RuntimeError> {
    if request.schedule_id.trim().is_empty()
        || request.occurrence_id.trim().is_empty()
        || request.target_session_id.trim().is_empty()
        || request.prompt.trim().is_empty()
    {
        return Err(RuntimeError::InvalidCommand(
            "trigger dispatch identity, target, and prompt must be non-empty".to_owned(),
        ));
    }
    if request.recurrence_seconds.is_some_and(|seconds| seconds < MIN_RECURRENCE_SECONDS) {
        return Err(RuntimeError::InvalidCommand(format!(
            "trigger recurrence must be at least {MIN_RECURRENCE_SECONDS} seconds"
        )));
    }
    if request.wake_policy == SessionActionWakePolicy::Immediate && !request.persistent_goal {
        return Err(RuntimeError::InvalidCommand(
            "immediate scheduled wake requires an explicit persistent-goal contract".to_owned(),
        ));
    }
    Ok(())
}

fn claimed_record(request: &TriggerDispatchRequest) -> Result<TriggerDispatchRecord, RuntimeError> {
    let delivery_policy = match request.delivery_mode {
        TriggerDeliveryMode::Steer => SessionActionDeliveryPolicy::NextSafeTurnBoundary,
        TriggerDeliveryMode::FollowUp => SessionActionDeliveryPolicy::WhenIdle,
    };
    let idempotency_key = occurrence_key(request);
    let action_id = deterministic_action_id(&request.target_session_id, &idempotency_key);
    let mut record = TriggerDispatchRecord {
        format_version: DISPATCH_FORMAT_VERSION,
        schedule_id: request.schedule_id.clone(),
        source_kind: request.source_kind,
        occurrence_id: request.occurrence_id.clone(),
        occurrence_sequence: request.occurrence_sequence,
        scheduled_for: request.scheduled_for,
        observed_at: request.observed_at,
        target_session_id: request.target_session_id.clone(),
        idempotency_key,
        action_id,
        delivery_mode: request.delivery_mode,
        delivery_policy,
        wake_policy: request.wake_policy,
        prompt: request.prompt.clone(),
        authorized: request.authorized,
        enabled: request.enabled,
        persistent_goal: request.persistent_goal,
        recurrence_seconds: request.recurrence_seconds,
        missed_run_policy: request.missed_run_policy,
        status: TriggerDispatchStatus::Claimed,
        recovered: false,
        skip_reason: None,
        action_lifecycle: None,
        action_accepted_sequence: None,
        content_hash: String::new(),
    };
    seal_record(&mut record)?;
    Ok(record)
}

fn request_from_record(record: &TriggerDispatchRecord) -> Result<TriggerDispatchRequest, RuntimeError> {
    Ok(TriggerDispatchRequest {
        schedule_id: record.schedule_id.clone(),
        source_kind: record.source_kind,
        occurrence_id: record.occurrence_id.clone(),
        occurrence_sequence: record.occurrence_sequence,
        scheduled_for: record.scheduled_for,
        observed_at: record.observed_at,
        target_session_id: record.target_session_id.clone(),
        delivery_mode: record.delivery_mode,
        wake_policy: record.wake_policy,
        prompt: record.prompt.clone(),
        authorized: record.authorized,
        enabled: record.enabled,
        persistent_goal: record.persistent_goal,
        recurrence_seconds: record.recurrence_seconds,
        missed_run_policy: record.missed_run_policy,
    })
}

fn ensure_same_occurrence(
    record: &TriggerDispatchRecord,
    request: &TriggerDispatchRequest,
) -> Result<(), RuntimeError> {
    if record.schedule_id == request.schedule_id
        && record.source_kind == request.source_kind
        && record.occurrence_id == request.occurrence_id
        && record.occurrence_sequence == request.occurrence_sequence
        && record.target_session_id == request.target_session_id
        && record.delivery_mode == request.delivery_mode
        && record.wake_policy == request.wake_policy
    {
        return Ok(());
    }
    Err(RuntimeError::InvalidCommand(
        "trigger occurrence identity was reused with different dispatch semantics".to_owned(),
    ))
}

fn occurrence_key(request: &TriggerDispatchRequest) -> String {
    format!(
        "trigger-v1:{}:{}:{}",
        request.schedule_id, request.occurrence_sequence, request.occurrence_id
    )
}

fn deterministic_action_id(session_id: &str, idempotency_key: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(session_id.as_bytes());
    digest.update([0]);
    digest.update(idempotency_key.as_bytes());
    format!("action-{}", hex::encode(digest.finalize()))
}

fn dispatch_directory(repo: &Path, session_id: &str) -> PathBuf {
    repo.join(".medusa")
        .join("trigger-dispatch-v1")
        .join(safe_component(session_id))
}

fn dispatch_path(repo: &Path, request: &TriggerDispatchRequest) -> Result<PathBuf, RuntimeError> {
    let key = occurrence_key(request);
    let name = hex::encode(Sha256::digest(key.as_bytes()));
    Ok(dispatch_directory(repo, &request.target_session_id).join(format!("{name}.json")))
}

fn safe_component(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn persist_record(path: &Path, record: &TriggerDispatchRecord) -> Result<(), RuntimeError> {
    let parent = path
        .parent()
        .ok_or_else(|| RuntimeError::agent("trigger dispatch path has no parent"))?;
    fs::create_dir_all(parent).map_err(RuntimeError::agent)?;
    let bytes = serde_json::to_vec_pretty(record).map_err(RuntimeError::agent)?;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = File::create(&tmp).map_err(RuntimeError::agent)?;
    file.write_all(&bytes).map_err(RuntimeError::agent)?;
    file.sync_all().map_err(RuntimeError::agent)?;
    fs::rename(&tmp, path).map_err(RuntimeError::agent)?;
    sync_directory(parent)?;
    Ok(())
}

fn load_record(path: &Path) -> Result<TriggerDispatchRecord, RuntimeError> {
    let bytes = fs::read(path).map_err(RuntimeError::agent)?;
    serde_json::from_slice(&bytes).map_err(RuntimeError::agent)
}

fn sync_directory(path: &Path) -> Result<(), RuntimeError> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(RuntimeError::agent)?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn seal_record(record: &mut TriggerDispatchRecord) -> Result<(), RuntimeError> {
    record.content_hash.clear();
    record.content_hash = record_hash(record)?;
    Ok(())
}

fn record_hash(record: &TriggerDispatchRecord) -> Result<String, RuntimeError> {
    let mut material = record.clone();
    material.content_hash.clear();
    let bytes = serde_json::to_vec(&material).map_err(RuntimeError::agent)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> TriggerDispatchRequest {
        TriggerDispatchRequest {
            schedule_id: "heartbeat-main".to_owned(),
            source_kind: TriggerSourceKind::Heartbeat,
            occurrence_id: "occ-7".to_owned(),
            occurrence_sequence: 7,
            scheduled_for: OffsetDateTime::UNIX_EPOCH,
            observed_at: OffsetDateTime::UNIX_EPOCH,
            target_session_id: "session-1".to_owned(),
            delivery_mode: TriggerDeliveryMode::Steer,
            wake_policy: SessionActionWakePolicy::OnBoundary,
            prompt: "re-check the external state".to_owned(),
            authorized: true,
            enabled: true,
            persistent_goal: false,
            recurrence_seconds: Some(60),
            missed_run_policy: MissedRunPolicy::Coalesce,
        }
    }

    #[test]
    fn trigger_identity_is_deterministic_and_delivery_mode_is_explicit() {
        let request = request();
        let first = claimed_record(&request).expect("record");
        let second = claimed_record(&request).expect("record");
        assert_eq!(first.idempotency_key, second.idempotency_key);
        assert_eq!(first.action_id, second.action_id);
        assert_eq!(first.delivery_policy, SessionActionDeliveryPolicy::NextSafeTurnBoundary);

        let mut follow_up = request;
        follow_up.delivery_mode = TriggerDeliveryMode::FollowUp;
        let record = claimed_record(&follow_up).expect("follow-up record");
        assert_eq!(record.delivery_policy, SessionActionDeliveryPolicy::WhenIdle);
    }

    #[test]
    fn missed_run_policy_is_bounded_and_deterministic() {
        let missed: Vec<u8> = (0..20).collect();
        assert!(select_missed_occurrences(MissedRunPolicy::Skip, &missed).is_empty());
        assert_eq!(select_missed_occurrences(MissedRunPolicy::Coalesce, &missed), vec![19]);
        assert_eq!(
            select_missed_occurrences(MissedRunPolicy::CatchUp, &missed),
            (12..20).collect::<Vec<_>>()
        );
    }

    #[test]
    fn safeguards_fail_closed() {
        let mut too_fast = request();
        too_fast.recurrence_seconds = Some(1);
        assert!(validate_request(&too_fast).is_err());

        let mut perpetual = request();
        perpetual.wake_policy = SessionActionWakePolicy::Immediate;
        assert!(validate_request(&perpetual).is_err());
    }

    #[test]
    fn content_hash_rejects_tampering() {
        let mut record = claimed_record(&request()).expect("record");
        record.occurrence_sequence = 99;
        assert!(record.verify().is_err());
    }
}
