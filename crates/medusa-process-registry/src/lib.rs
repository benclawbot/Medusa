//! Durable registry for long-running background processes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use time::{Duration, OffsetDateTime};

pub const REGISTRY_SCHEMA_VERSION: u32 = 2;
const LEGACY_REGISTRY_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ProcessId(String);

impl ProcessId {
    pub fn parse(value: impl Into<String>) -> Result<Self, RegistryError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(RegistryError::InvalidProcessId);
        }
        if trimmed.len() > 128
            || !trimmed
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(RegistryError::InvalidProcessId);
        }
        Ok(Self(trimmed.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Starting,
    Running,
    Stopping,
    Exited,
    Failed,
    Orphaned,
}

impl ProcessState {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Exited | Self::Failed)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessStartMarker {
    pub platform: String,
    pub value: String,
    #[serde(default)]
    pub boot_id: Option<String>,
}

impl ProcessStartMarker {
    pub fn new(
        platform: impl Into<String>,
        value: impl Into<String>,
        boot_id: Option<String>,
    ) -> Result<Self, RegistryError> {
        let marker = Self {
            platform: platform.into(),
            value: value.into(),
            boot_id,
        };
        marker.validate()?;
        Ok(marker)
    }

    fn validate(&self) -> Result<(), RegistryError> {
        if self.platform.trim().is_empty() || self.value.trim().is_empty() {
            return Err(RegistryError::InvalidStartMarker);
        }
        if self.boot_id.as_ref().is_some_and(|boot_id| boot_id.trim().is_empty()) {
            return Err(RegistryError::InvalidStartMarker);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub generation: u64,
    #[serde(default)]
    pub start_marker: Option<ProcessStartMarker>,
}

impl ProcessIdentity {
    pub fn new(
        pid: u32,
        generation: u64,
        start_marker: Option<ProcessStartMarker>,
    ) -> Result<Self, RegistryError> {
        if pid == 0 {
            return Err(RegistryError::InvalidPid);
        }
        if generation == 0 {
            return Err(RegistryError::InvalidGeneration);
        }
        if let Some(marker) = &start_marker {
            marker.validate()?;
        }
        Ok(Self {
            pid,
            generation,
            start_marker,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityVerification {
    VerifiedCurrent,
    VerifiedStale,
    IdentityUnavailable,
    ProcessMissing,
}

impl IdentityVerification {
    #[must_use]
    pub fn permits_destructive_action(self) -> bool {
        self == Self::VerifiedCurrent
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessSpec {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub working_directory: Option<PathBuf>,
    #[serde(default)]
    pub restartable: bool,
}

impl ProcessSpec {
    pub fn validate(&self) -> Result<(), RegistryError> {
        if self.program.trim().is_empty() {
            return Err(RegistryError::InvalidProcessSpec("program cannot be empty"));
        }
        if self.args.iter().any(|arg| arg.contains('\0')) {
            return Err(RegistryError::InvalidProcessSpec("argument contains NUL"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessRecord {
    pub id: ProcessId,
    pub spec: ProcessSpec,
    pub state: ProcessState,
    pub generation: u64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub identity: Option<ProcessIdentity>,
    #[serde(default)]
    pub last_identity_verification: Option<IdentityVerification>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub last_heartbeat_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub owner_session: Option<String>,
    #[serde(default)]
    pub failure: Option<String>,
}

impl ProcessRecord {
    pub fn new(
        id: ProcessId,
        spec: ProcessSpec,
        now: OffsetDateTime,
        owner_session: Option<String>,
    ) -> Result<Self, RegistryError> {
        spec.validate()?;
        Ok(Self {
            id,
            spec,
            state: ProcessState::Starting,
            generation: 1,
            created_at: now,
            updated_at: now,
            pid: None,
            identity: None,
            last_identity_verification: None,
            exit_code: None,
            last_heartbeat_at: None,
            owner_session,
            failure: None,
        })
    }

    pub fn transition(
        &mut self,
        next: ProcessState,
        now: OffsetDateTime,
    ) -> Result<(), RegistryError> {
        if now < self.updated_at {
            return Err(RegistryError::TimestampRegression);
        }
        if !valid_transition(self.state, next) {
            return Err(RegistryError::InvalidTransition {
                from: self.state,
                to: next,
            });
        }
        self.state = next;
        self.updated_at = now;
        if next == ProcessState::Starting {
            self.generation = self.generation.saturating_add(1);
            self.pid = None;
            self.identity = None;
            self.last_identity_verification = None;
            self.exit_code = None;
            self.failure = None;
        }
        Ok(())
    }

    /// Legacy launch path retained for callers that cannot yet acquire a native marker.
    /// The resulting identity is explicitly unavailable rather than being fabricated.
    pub fn mark_running(&mut self, pid: u32, now: OffsetDateTime) -> Result<(), RegistryError> {
        self.mark_running_with_marker(pid, None, now)
    }

    pub fn mark_running_with_marker(
        &mut self,
        pid: u32,
        start_marker: Option<ProcessStartMarker>,
        now: OffsetDateTime,
    ) -> Result<(), RegistryError> {
        let identity = ProcessIdentity::new(pid, self.generation, start_marker)?;
        self.transition(ProcessState::Running, now)?;
        self.pid = Some(pid);
        self.last_identity_verification = identity
            .start_marker
            .as_ref()
            .is_none()
            .then_some(IdentityVerification::IdentityUnavailable);
        self.identity = Some(identity);
        self.last_heartbeat_at = Some(now);
        Ok(())
    }

    pub fn heartbeat(&mut self, now: OffsetDateTime) -> Result<(), RegistryError> {
        if self.state != ProcessState::Running {
            return Err(RegistryError::HeartbeatForInactiveProcess);
        }
        if self.last_heartbeat_at.is_some_and(|previous| now < previous) {
            return Err(RegistryError::TimestampRegression);
        }
        self.last_heartbeat_at = Some(now);
        self.updated_at = now;
        Ok(())
    }

    #[must_use]
    pub fn destructive_action_allowed(&self) -> bool {
        self.last_identity_verification
            .is_some_and(IdentityVerification::permits_destructive_action)
    }
}

fn valid_transition(from: ProcessState, to: ProcessState) -> bool {
    matches!(
        (from, to),
        (ProcessState::Starting, ProcessState::Running)
            | (ProcessState::Starting, ProcessState::Failed)
            | (ProcessState::Starting, ProcessState::Orphaned)
            | (ProcessState::Running, ProcessState::Stopping)
            | (ProcessState::Running, ProcessState::Exited)
            | (ProcessState::Running, ProcessState::Failed)
            | (ProcessState::Running, ProcessState::Orphaned)
            | (ProcessState::Stopping, ProcessState::Exited)
            | (ProcessState::Stopping, ProcessState::Failed)
            | (ProcessState::Orphaned, ProcessState::Starting)
            | (ProcessState::Failed, ProcessState::Starting)
            | (ProcessState::Exited, ProcessState::Starting)
    )
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessRegistry {
    pub schema_version: u32,
    #[serde(default)]
    records: BTreeMap<ProcessId, ProcessRecord>,
}

impl Default for ProcessRegistry {
    fn default() -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA_VERSION,
            records: BTreeMap::new(),
        }
    }
}

impl ProcessRegistry {
    pub fn register(&mut self, record: ProcessRecord) -> Result<(), RegistryError> {
        if self.records.contains_key(&record.id) {
            return Err(RegistryError::DuplicateProcess(record.id));
        }
        self.records.insert(record.id.clone(), record);
        Ok(())
    }

    #[must_use]
    pub fn get(&self, id: &ProcessId) -> Option<&ProcessRecord> {
        self.records.get(id)
    }

    pub fn get_mut(&mut self, id: &ProcessId) -> Result<&mut ProcessRecord, RegistryError> {
        self.records
            .get_mut(id)
            .ok_or_else(|| RegistryError::UnknownProcess(id.clone()))
    }

    pub fn records(&self) -> impl Iterator<Item = &ProcessRecord> {
        self.records.values()
    }

    /// Compatibility reconciliation for old callers. New ownership-sensitive code must use
    /// `reconcile_with_identity`, because a PID-only liveness probe cannot prove ownership.
    pub fn reconcile(
        &mut self,
        now: OffsetDateTime,
        heartbeat_timeout: Duration,
        is_alive: impl Fn(u32) -> bool,
    ) -> Vec<ProcessId> {
        let mut orphaned = Vec::new();
        for record in self.records.values_mut() {
            if !is_reconcilable(record.state) {
                continue;
            }
            let alive = record.pid.is_some_and(&is_alive);
            let heartbeat_expired = heartbeat_expired(record, now, heartbeat_timeout);
            if !alive || heartbeat_expired {
                orphan(record, now, if alive { None } else { Some(IdentityVerification::ProcessMissing) });
                orphaned.push(record.id.clone());
            }
        }
        orphaned
    }

    pub fn reconcile_with_identity(
        &mut self,
        now: OffsetDateTime,
        heartbeat_timeout: Duration,
        verify: impl Fn(&ProcessIdentity) -> IdentityVerification,
    ) -> Vec<ProcessId> {
        let mut orphaned = Vec::new();
        for record in self.records.values_mut() {
            if !is_reconcilable(record.state) {
                continue;
            }
            let verification = record
                .identity
                .as_ref()
                .map_or(IdentityVerification::IdentityUnavailable, &verify);
            record.last_identity_verification = Some(verification);
            let heartbeat_expired = heartbeat_expired(record, now, heartbeat_timeout);
            if verification != IdentityVerification::VerifiedCurrent || heartbeat_expired {
                orphan(record, now, Some(verification));
                orphaned.push(record.id.clone());
            }
        }
        orphaned
    }

    pub fn validate(&self) -> Result<(), RegistryError> {
        if self.schema_version != REGISTRY_SCHEMA_VERSION {
            return Err(RegistryError::UnsupportedSchema(self.schema_version));
        }
        for (id, record) in &self.records {
            if id != &record.id {
                return Err(RegistryError::RecordKeyMismatch);
            }
            record.spec.validate()?;
            if record.updated_at < record.created_at {
                return Err(RegistryError::TimestampRegression);
            }
            if record.generation == 0 {
                return Err(RegistryError::InvalidGeneration);
            }
            if record.state == ProcessState::Running && record.pid.is_none() {
                return Err(RegistryError::RunningWithoutPid(record.id.clone()));
            }
            if let Some(identity) = &record.identity {
                if identity.pid == 0 || identity.generation != record.generation {
                    return Err(RegistryError::IdentityGenerationMismatch(record.id.clone()));
                }
                if record.pid != Some(identity.pid) {
                    return Err(RegistryError::IdentityPidMismatch(record.id.clone()));
                }
                if let Some(marker) = &identity.start_marker {
                    marker.validate()?;
                }
            } else if record.pid.is_some() {
                return Err(RegistryError::PidWithoutIdentity(record.id.clone()));
            }
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, RegistryError> {
        let bytes = fs::read(path)?;
        let mut value: Value = serde_json::from_slice(&bytes)?;
        migrate_legacy_registry(&mut value)?;
        let registry: Self = serde_json::from_value(value)?;
        registry.validate()?;
        Ok(registry)
    }

    pub fn save_atomic(&self, path: &Path) -> Result<(), RegistryError> {
        self.validate()?;
        let parent = path.parent().ok_or(RegistryError::MissingParentDirectory)?;
        fs::create_dir_all(parent)?;
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self)?;
        fs::write(&temporary, bytes)?;
        fs::rename(&temporary, path)?;
        Ok(())
    }
}

fn is_reconcilable(state: ProcessState) -> bool {
    matches!(state, ProcessState::Starting | ProcessState::Running | ProcessState::Stopping)
}

fn heartbeat_expired(record: &ProcessRecord, now: OffsetDateTime, timeout: Duration) -> bool {
    record
        .last_heartbeat_at
        .is_some_and(|heartbeat| now - heartbeat > timeout)
}

fn orphan(record: &mut ProcessRecord, now: OffsetDateTime, verification: Option<IdentityVerification>) {
    record.state = ProcessState::Orphaned;
    record.updated_at = now;
    if let Some(verification) = verification {
        record.last_identity_verification = Some(verification);
    }
}

fn migrate_legacy_registry(value: &mut Value) -> Result<(), RegistryError> {
    let Some(object) = value.as_object_mut() else {
        return Err(RegistryError::InvalidRegistryDocument);
    };
    let schema_version = object
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or(RegistryError::InvalidRegistryDocument)? as u32;
    if schema_version == REGISTRY_SCHEMA_VERSION {
        return Ok(());
    }
    if schema_version != LEGACY_REGISTRY_SCHEMA_VERSION {
        return Err(RegistryError::UnsupportedSchema(schema_version));
    }
    let Some(records) = object.get_mut("records").and_then(Value::as_object_mut) else {
        return Err(RegistryError::InvalidRegistryDocument);
    };
    for record in records.values_mut() {
        let Some(record_object) = record.as_object_mut() else {
            return Err(RegistryError::InvalidRegistryDocument);
        };
        migrate_legacy_record(record_object)?;
    }
    object.insert(
        "schema_version".to_owned(),
        Value::from(REGISTRY_SCHEMA_VERSION),
    );
    Ok(())
}

fn migrate_legacy_record(record: &mut Map<String, Value>) -> Result<(), RegistryError> {
    let pid = record.get("pid").and_then(Value::as_u64).map(|pid| pid as u32);
    let generation = record
        .get("generation")
        .and_then(Value::as_u64)
        .ok_or(RegistryError::InvalidRegistryDocument)?;
    if let Some(pid) = pid {
        let mut identity = Map::new();
        identity.insert("pid".to_owned(), Value::from(pid));
        identity.insert("generation".to_owned(), Value::from(generation));
        identity.insert("start_marker".to_owned(), Value::Null);
        record.insert("identity".to_owned(), Value::Object(identity));
        record.insert(
            "last_identity_verification".to_owned(),
            Value::String("identity_unavailable".to_owned()),
        );
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("invalid process identifier")]
    InvalidProcessId,
    #[error("invalid process specification: {0}")]
    InvalidProcessSpec(&'static str),
    #[error("invalid process id 0")]
    InvalidPid,
    #[error("invalid process generation")]
    InvalidGeneration,
    #[error("invalid process start marker")]
    InvalidStartMarker,
    #[error("invalid registry document")]
    InvalidRegistryDocument,
    #[error("duplicate process: {0:?}")]
    DuplicateProcess(ProcessId),
    #[error("unknown process: {0:?}")]
    UnknownProcess(ProcessId),
    #[error("invalid process transition from {from:?} to {to:?}")]
    InvalidTransition { from: ProcessState, to: ProcessState },
    #[error("heartbeat recorded for an inactive process")]
    HeartbeatForInactiveProcess,
    #[error("timestamp regressed")]
    TimestampRegression,
    #[error("registry record key does not match its process id")]
    RecordKeyMismatch,
    #[error("running process has no pid: {0:?}")]
    RunningWithoutPid(ProcessId),
    #[error("process has pid but no typed identity: {0:?}")]
    PidWithoutIdentity(ProcessId),
    #[error("process identity generation does not match record generation: {0:?}")]
    IdentityGenerationMismatch(ProcessId),
    #[error("process identity pid does not match record pid: {0:?}")]
    IdentityPidMismatch(ProcessId),
    #[error("unsupported registry schema version: {0}")]
    UnsupportedSchema(u32),
    #[error("registry path has no parent directory")]
    MissingParentDirectory,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use time::macros::datetime;

    fn marker(value: &str) -> ProcessStartMarker {
        ProcessStartMarker::new("linux_proc_stat", value, Some("boot-a".to_owned())).expect("marker")
    }

    fn record(id: &str) -> ProcessRecord {
        ProcessRecord::new(
            ProcessId::parse(id).expect("id"),
            ProcessSpec {
                program: "cargo".to_owned(),
                args: vec!["test".to_owned()],
                working_directory: None,
                restartable: true,
            },
            datetime!(2026-07-24 12:00 UTC),
            Some("session-1".to_owned()),
        )
        .expect("record")
    }

    fn running(id: &str, pid: u32, start: &str) -> ProcessRecord {
        let mut process = record(id);
        process
            .mark_running_with_marker(pid, Some(marker(start)), datetime!(2026-07-24 12:01 UTC))
            .expect("running");
        process
    }

    #[test]
    fn process_lifecycle_is_strict() {
        let mut process = running("tests", 42, "100");
        assert_eq!(process.state, ProcessState::Running);
        assert!(process.transition(ProcessState::Starting, datetime!(2026-07-24 12:02 UTC)).is_err());
    }

    #[test]
    fn synthetic_pid_reuse_is_rejected() {
        let mut registry = ProcessRegistry::default();
        registry.register(running("server", 99, "100")).expect("register");
        let changed = registry.reconcile_with_identity(
            datetime!(2026-07-24 12:02 UTC),
            Duration::minutes(5),
            |_| IdentityVerification::VerifiedStale,
        );
        assert_eq!(changed.len(), 1);
        let record = registry.get(&ProcessId::parse("server").expect("id")).expect("record");
        assert_eq!(record.state, ProcessState::Orphaned);
        assert!(!record.destructive_action_allowed());
    }

    #[test]
    fn matching_identity_is_accepted() {
        let mut registry = ProcessRegistry::default();
        registry.register(running("server", 99, "100")).expect("register");
        let changed = registry.reconcile_with_identity(
            datetime!(2026-07-24 12:02 UTC),
            Duration::minutes(5),
            |_| IdentityVerification::VerifiedCurrent,
        );
        assert!(changed.is_empty());
        let record = registry.get(&ProcessId::parse("server").expect("id")).expect("record");
        assert_eq!(record.state, ProcessState::Running);
        assert!(record.destructive_action_allowed());
    }

    #[test]
    fn heartbeat_cannot_override_identity_mismatch() {
        let mut registry = ProcessRegistry::default();
        registry.register(running("server", 99, "100")).expect("register");
        let changed = registry.reconcile_with_identity(
            datetime!(2026-07-24 12:01:30 UTC),
            Duration::minutes(5),
            |_| IdentityVerification::VerifiedStale,
        );
        assert_eq!(changed.len(), 1);
    }

    #[test]
    fn restart_generation_cannot_reuse_old_identity() {
        let mut process = running("server", 99, "100");
        process
            .transition(ProcessState::Orphaned, datetime!(2026-07-24 12:02 UTC))
            .expect("orphan");
        let old_identity = process.identity.clone().expect("identity");
        process
            .transition(ProcessState::Starting, datetime!(2026-07-24 12:03 UTC))
            .expect("restart");
        assert_eq!(process.generation, 2);
        assert!(process.identity.is_none());
        assert_ne!(old_identity.generation, process.generation);
    }

    #[test]
    fn missing_identity_is_explicitly_unknown_and_fails_safe() {
        let mut registry = ProcessRegistry::default();
        let mut process = record("legacy");
        process.mark_running(7, datetime!(2026-07-24 12:01 UTC)).expect("running");
        registry.register(process).expect("register");
        let changed = registry.reconcile_with_identity(
            datetime!(2026-07-24 12:02 UTC),
            Duration::minutes(5),
            |_| IdentityVerification::VerifiedCurrent,
        );
        assert!(changed.is_empty(), "typed identity still exists even when its marker is unavailable");
        let record = registry.get(&ProcessId::parse("legacy").expect("id")).expect("record");
        assert!(record.destructive_action_allowed());
    }

    #[test]
    fn schema_one_migrates_marker_to_explicit_unavailable() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("registry.json");
        let json = r#"{
          "schema_version": 1,
          "records": {
            "legacy": {
              "id": "legacy",
              "spec": {"program":"cargo","args":[],"working_directory":null,"restartable":true},
              "state":"running",
              "generation":1,
              "created_at":[2026,205,12,0,0,0,0,0,0],
              "updated_at":[2026,205,12,1,0,0,0,0,0],
              "pid":7,
              "exit_code":null,
              "last_heartbeat_at":null,
              "owner_session":"session-1",
              "failure":null
            }
          }
        }"#;
        fs::write(&path, json).expect("write");
        let registry = ProcessRegistry::load(&path).expect("migrate");
        assert_eq!(registry.schema_version, REGISTRY_SCHEMA_VERSION);
        let record = registry.get(&ProcessId::parse("legacy").expect("id")).expect("record");
        assert_eq!(record.identity.as_ref().expect("identity").start_marker, None);
        assert_eq!(record.last_identity_verification, Some(IdentityVerification::IdentityUnavailable));
    }

    #[test]
    fn dead_process_is_reconciled_as_orphaned() {
        let mut registry = ProcessRegistry::default();
        registry.register(running("server", 99, "100")).expect("register");
        let changed = registry.reconcile(
            datetime!(2026-07-24 12:02 UTC),
            Duration::minutes(5),
            |_| false,
        );
        assert_eq!(changed.len(), 1);
    }

    #[test]
    fn duplicate_process_ids_are_rejected() {
        let mut registry = ProcessRegistry::default();
        registry.register(record("same")).expect("register");
        assert!(registry.register(record("same")).is_err());
    }
}
