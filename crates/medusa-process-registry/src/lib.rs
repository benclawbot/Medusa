//! Durable registry for long-running background processes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{Duration, OffsetDateTime};

pub const REGISTRY_SCHEMA_VERSION: u32 = 1;

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
            self.exit_code = None;
            self.failure = None;
        }
        Ok(())
    }

    pub fn mark_running(
        &mut self,
        pid: u32,
        now: OffsetDateTime,
    ) -> Result<(), RegistryError> {
        if pid == 0 {
            return Err(RegistryError::InvalidPid);
        }
        self.transition(ProcessState::Running, now)?;
        self.pid = Some(pid);
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

    #[must_use]
    pub fn records(&self) -> impl Iterator<Item = &ProcessRecord> {
        self.records.values()
    }

    pub fn reconcile(
        &mut self,
        now: OffsetDateTime,
        heartbeat_timeout: Duration,
        is_alive: impl Fn(u32) -> bool,
    ) -> Vec<ProcessId> {
        let mut orphaned = Vec::new();
        for record in self.records.values_mut() {
            if !matches!(record.state, ProcessState::Starting | ProcessState::Running | ProcessState::Stopping) {
                continue;
            }
            let alive = record.pid.is_some_and(&is_alive);
            let heartbeat_expired = record
                .last_heartbeat_at
                .is_some_and(|heartbeat| now - heartbeat > heartbeat_timeout);
            if !alive || heartbeat_expired {
                record.state = ProcessState::Orphaned;
                record.updated_at = now;
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
            if record.state == ProcessState::Running && record.pid.is_none() {
                return Err(RegistryError::RunningWithoutPid(record.id.clone()));
            }
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, RegistryError> {
        let bytes = fs::read(path)?;
        let registry: Self = serde_json::from_slice(&bytes)?;
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

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("invalid process identifier")]
    InvalidProcessId,
    #[error("invalid process specification: {0}")]
    InvalidProcessSpec(&'static str),
    #[error("invalid process id 0")]
    InvalidPid,
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
    use time::macros::datetime;

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

    #[test]
    fn process_lifecycle_is_strict() {
        let mut process = record("tests");
        process
            .mark_running(42, datetime!(2026-07-24 12:01 UTC))
            .expect("running");
        assert_eq!(process.state, ProcessState::Running);
        assert!(process
            .transition(ProcessState::Starting, datetime!(2026-07-24 12:02 UTC))
            .is_err());
    }

    #[test]
    fn dead_process_is_reconciled_as_orphaned() {
        let mut registry = ProcessRegistry::default();
        let mut process = record("server");
        process
            .mark_running(99, datetime!(2026-07-24 12:01 UTC))
            .expect("running");
        registry.register(process).expect("register");
        let changed = registry.reconcile(
            datetime!(2026-07-24 12:02 UTC),
            Duration::minutes(5),
            |_| false,
        );
        assert_eq!(changed.len(), 1);
        assert_eq!(
            registry.get(&ProcessId::parse("server").expect("id")).expect("record").state,
            ProcessState::Orphaned
        );
    }

    #[test]
    fn stale_heartbeat_is_reconciled() {
        let mut registry = ProcessRegistry::default();
        let mut process = record("watcher");
        process
            .mark_running(7, datetime!(2026-07-24 12:01 UTC))
            .expect("running");
        registry.register(process).expect("register");
        registry.reconcile(
            datetime!(2026-07-24 12:20 UTC),
            Duration::minutes(5),
            |_| true,
        );
        assert_eq!(
            registry.get(&ProcessId::parse("watcher").expect("id")).expect("record").state,
            ProcessState::Orphaned
        );
    }

    #[test]
    fn duplicate_process_ids_are_rejected() {
        let mut registry = ProcessRegistry::default();
        registry.register(record("same")).expect("register");
        assert!(registry.register(record("same")).is_err());
    }
}
